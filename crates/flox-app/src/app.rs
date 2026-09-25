//! The application context and the UI shell.
//!
//! - [`AppContext`] holds the long-lived services `main` creates.
//! - [`Services`] is the part the screens use, behind small traits ([`Catalog`],
//!   [`ImageSource`], [`LibrarySource`]) so `--dev-fixtures` and the snapshot tests
//!   can swap TMDB, the image cache and Telegram for offline data.
//! - [`Shell`] lives on the UI thread. It owns the router, one focus graph per
//!   screen and the Slint models, turns key presses and pointer events into focus
//!   moves and navigation, and runs loads through [`Exec`]: the work runs on the
//!   tokio runtime and its result comes back to the UI thread, where the pure view
//!   models (`vm::*`) decide what to show.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flox_core::error::Result;
use flox_core::images::{ImageCache, Rgba};
use flox_core::model::{
    EpisodeInfo, EpisodeKey, MediaType, SeasonInfo, TitleDetails, TitleSummary, TmdbId,
};
use flox_core::paths::AppPaths;
use flox_core::progress::{ProgressRecord, ProgressStore};
use flox_core::settings::{Settings, SettingsStore};
use flox_core::tmdb::{ImageSize, Tmdb};
use flox_player::ffi::MpvLib;
use flox_rip::queue::Queue;
use flox_td::auth::{Auth, AuthState};
use flox_td::client::TdClient;
use flox_td::library::{Entry, Library, LibraryIndex};
use parking_lot::RwLock;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::focus::{key_action, Direction, Focus, FocusGraph, KeyAction, Modifiers, ZoneId};
use crate::launch::{Integration, TelegramStack};
use crate::player::mpv_engine::TdAccess;
use crate::player::view::{LibraryPrints, PlayerDeps, PlayerView, Underlay};
use crate::router::{PlayRequest, Route, Router, Transition};
use crate::ui::{
    AppWindow, Card, DetailsState, Episode, FocusState, HomeState, PlayerState, Screen,
    SearchState, Season,
};
use crate::vm::{details as dvm, home as hvm, search as svm};
use crate::vm::{ingest as ivm, library as lvm, queue as qvm};

#[path = "ingest_shell.rs"]
mod ingest_shell;
pub use ingest_shell::{
    FilePicker, FourKHdHub, HubSource, Ingest, JobQueue, LibraryAdmin, NoHub, QueueEvents,
    SystemPicker,
};

mod login_screen;
mod settings_screen;

// ---------------------------------------------------------------------------
// Services

/// Movie and TV metadata (TMDB, or fixtures).
#[async_trait]
pub trait Catalog: Send + Sync {
    async fn trending(&self, media: MediaType) -> Result<Vec<TitleSummary>>;
    async fn search(&self, query: &str) -> Result<Vec<TitleSummary>>;
    async fn details(&self, media: MediaType, id: TmdbId) -> Result<TitleDetails>;
    async fn season(&self, id: TmdbId, season: u32) -> Result<Vec<EpisodeInfo>>;
}

#[async_trait]
impl Catalog for Tmdb {
    async fn trending(&self, media: MediaType) -> Result<Vec<TitleSummary>> {
        Tmdb::trending(self, media).await
    }

    async fn search(&self, query: &str) -> Result<Vec<TitleSummary>> {
        Tmdb::search(self, query).await
    }

    async fn details(&self, media: MediaType, id: TmdbId) -> Result<TitleDetails> {
        Tmdb::details(self, media, id).await
    }

    async fn season(&self, id: TmdbId, season: u32) -> Result<Vec<EpisodeInfo>> {
        Tmdb::season(self, id, season).await
    }
}

/// Decoded posters and stills for TMDB image paths.
#[async_trait]
pub trait ImageSource: Send + Sync {
    async fn image(&self, path: &str, size: ImageSize) -> Result<Arc<Rgba>>;
}

#[async_trait]
impl ImageSource for ImageCache {
    async fn image(&self, path: &str, size: ImageSize) -> Result<Arc<Rgba>> {
        self.get(&Tmdb::image_url(path, size)).await
    }
}

/// One uploaded print, as the browse screens see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Print {
    pub quality: String,
    pub size: u64,
    pub newest_message_id: i64,
}

/// A snapshot of the channel index.
pub trait LibraryView: Send + Sync {
    /// Every title with at least one print.
    fn titles(&self) -> Vec<(TmdbId, MediaType)>;
    /// The prints of one movie or episode.
    fn prints(&self, key: EpisodeKey) -> Vec<Print>;
    /// The complete prints of one movie or episode, tallest first (what the player plays).
    fn entries(&self, _key: EpisodeKey) -> Vec<Entry> {
        Vec::new()
    }
    /// Every quality uploaded anywhere in the channel (Settings' Default quality).
    fn all_qualities(&self) -> Vec<String> {
        Vec::new()
    }
}

/// The qualities uploaded for `key`, in index order (tallest first).
pub fn qualities(lib: &dyn LibraryView, key: EpisodeKey) -> Vec<String> {
    lib.prints(key).into_iter().map(|p| p.quality).collect()
}

/// Nothing uploaded (Telegram not ready yet).
#[derive(Clone, Copy, Debug, Default)]
pub struct EmptyLibrary;

impl LibraryView for EmptyLibrary {
    fn titles(&self) -> Vec<(TmdbId, MediaType)> {
        Vec::new()
    }

    fn prints(&self, _key: EpisodeKey) -> Vec<Print> {
        Vec::new()
    }
}

impl LibraryView for LibraryIndex {
    fn titles(&self) -> Vec<(TmdbId, MediaType)> {
        LibraryIndex::titles(self)
    }

    fn prints(&self, key: EpisodeKey) -> Vec<Print> {
        self.entries_for(key)
            .iter()
            .map(|e| Print {
                quality: e.quality.clone(),
                size: e.size,
                newest_message_id: e.newest_message_id,
            })
            .collect()
    }

    fn entries(&self, key: EpisodeKey) -> Vec<Entry> {
        self.entries_for(key).to_vec()
    }

    fn all_qualities(&self) -> Vec<String> {
        self.all().map(|e| e.quality.clone()).collect()
    }
}

/// Rebuilds the channel index.
#[async_trait]
pub trait LibrarySource: Send + Sync {
    async fn refresh(&self, channel_title: &str) -> Result<Arc<dyn LibraryView>>;
}

#[async_trait]
impl LibrarySource for Library {
    async fn refresh(&self, channel_title: &str) -> Result<Arc<dyn LibraryView>> {
        let index: Arc<LibraryIndex> = Library::refresh(self, channel_title).await?;
        Ok(index)
    }
}

/// Telegram as the browse screens see it.
#[derive(Clone)]
pub enum Telegram {
    /// The API id or hash is missing.
    NotConfigured,
    /// Credentials are set but tdjson could not be found or started.
    Unavailable,
    Connected {
        auth: Arc<Auth>,
        library: Arc<dyn LibrarySource>,
    },
    /// Fixtures: always signed in.
    Offline { library: Arc<dyn LibrarySource> },
}

impl Telegram {
    fn status(&self) -> hvm::TelegramStatus {
        match self {
            Telegram::NotConfigured => hvm::TelegramStatus::NotConfigured,
            Telegram::Unavailable => hvm::TelegramStatus::Unavailable,
            Telegram::Connected { auth, .. } => hvm::TelegramStatus::Auth(auth.current()),
            Telegram::Offline { .. } => hvm::TelegramStatus::Auth(AuthState::Ready {
                user: "fixtures".to_owned(),
            }),
        }
    }

    fn library(&self) -> Option<Arc<dyn LibrarySource>> {
        match self {
            Telegram::Connected { library, .. } | Telegram::Offline { library } => {
                Some(library.clone())
            }
            _ => None,
        }
    }
}

/// What the screens use.
pub struct Services {
    pub settings: SettingsStore,
    pub progress: Arc<ProgressStore>,
    pub catalog: Arc<dyn Catalog>,
    pub images: Arc<dyn ImageSource>,
    /// Swapped by [`crate::launch::Integration`] when the Telegram credentials change;
    /// read it with [`Services::telegram`].
    pub telegram: RwLock<Telegram>,
}

impl Services {
    /// The Telegram stack in effect now.
    pub fn telegram(&self) -> Telegram {
        self.telegram.read().clone()
    }

    /// Replaces the Telegram stack. Call [`Shell::telegram_replaced`] afterwards so
    /// the screens follow it.
    pub fn set_telegram(&self, telegram: Telegram) {
        *self.telegram.write() = telegram;
    }
}

/// Long-lived services, created once in `main`.
pub struct AppContext {
    pub runtime: tokio::runtime::Runtime,
    pub paths: AppPaths,
    pub settings: SettingsStore,
    pub progress: Arc<ProgressStore>,
    pub services: Arc<Services>,
    /// Present when API credentials were set and tdjson resolved at launch. Later
    /// starts and restarts go through [`crate::launch::Integration`].
    pub td: Option<Arc<TdClient>>,
    pub library: Option<Arc<Library>>,
    pub queue: Option<Arc<Queue>>,
    /// Present when libmpv resolves.
    pub player_lib: Option<Arc<MpvLib>>,
    /// `--dev-play <file>`: open the player on a local file at startup.
    pub dev_play: Option<std::path::PathBuf>,
}

// ---------------------------------------------------------------------------
// Execution

/// Where loads run.
#[derive(Clone, Debug)]
pub enum Exec {
    /// Work on the tokio runtime, results on the Slint event loop.
    Live(tokio::runtime::Handle),
    /// Everything at once on the calling thread, delays skipped. For snapshot tests
    /// with fixtures, whose futures never wait on I/O.
    Inline,
}

impl Exec {
    /// Runs `work` off the UI thread, then `then` with its output on the UI thread.
    pub fn run<T, F, G>(&self, work: F, then: G)
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
        G: FnOnce(T) + 'static,
    {
        match self {
            Exec::Inline => then(futures::executor::block_on(work)),
            Exec::Live(handle) => {
                let task = handle.spawn(work);
                let scheduled = slint::spawn_local(async move {
                    match task.await {
                        Ok(value) => then(value),
                        Err(e) => tracing::warn!("background task failed: {e}"),
                    }
                });
                if let Err(e) = scheduled {
                    tracing::warn!("cannot schedule on the UI thread: {e}");
                }
            }
        }
    }

    /// Calls `then` on the UI thread after `delay`.
    pub fn after(&self, delay: Duration, then: impl FnOnce() + 'static) {
        match self {
            Exec::Live(_) if !delay.is_zero() => slint::Timer::single_shot(delay, then),
            _ => then(),
        }
    }

    /// Calls `on` on the UI thread with every new value of `rx` (live only).
    pub fn watch<T>(&self, mut rx: tokio::sync::watch::Receiver<T>, mut on: impl FnMut(T) + 'static)
    where
        T: Clone + 'static,
    {
        if let Exec::Inline = self {
            return;
        }
        let scheduled = slint::spawn_local(async move {
            while rx.changed().await.is_ok() {
                let value = rx.borrow_and_update().clone();
                on(value);
            }
        });
        if let Err(e) = scheduled {
            tracing::warn!("cannot watch for changes: {e}");
        }
    }
}

/// Copies decoded RGBA into a Slint image. Must run on the UI thread.
pub fn to_image(rgba: &Rgba) -> Option<Image> {
    let expected = rgba.width as usize * rgba.height as usize * 4;
    if rgba.width == 0 || rgba.height == 0 || rgba.pixels.len() != expected {
        return None;
    }
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(rgba.width, rgba.height);
    buffer.make_mut_bytes().copy_from_slice(&rgba.pixels);
    Some(Image::from_rgba8(buffer))
}

/// Converted images kept on the UI thread, so revisited rows don't convert again.
/// The decoded bytes are capped by `ImageCache`; this only bounds the entry count.
const UI_IMAGE_ENTRIES: usize = 400;

// ---------------------------------------------------------------------------
// Shell state

/// The poster lists the shell fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cards {
    Continue,
    Library,
    Movies,
    Tv,
    Search,
}

/// A Slint card model with the data behind each card.
struct CardList {
    model: Rc<VecModel<Card>>,
    data: RefCell<Vec<hvm::CardData>>,
    /// Bumped on every refill; late image loads for older contents are dropped.
    generation: Cell<u64>,
}

impl CardList {
    fn new() -> Self {
        Self {
            model: Rc::new(VecModel::default()),
            data: RefCell::new(Vec::new()),
            generation: Cell::new(0),
        }
    }

    fn target(&self, index: usize) -> Option<Route> {
        self.data.borrow().get(index).map(|c| c.target.clone())
    }

    fn len(&self) -> usize {
        self.data.borrow().len()
    }
}

struct HomeModel {
    status: hvm::TelegramStatus,
    library: hvm::LibraryLoad,
    library_generation: u64,
    movies: hvm::RowLoad,
    tv: hvm::RowLoad,
    settings: Settings,
}

#[derive(Default)]
struct SearchModel {
    debounce: svm::Debounce,
    results: Vec<hvm::CardData>,
}

#[derive(Default)]
struct DetailsModel {
    route: Option<(TmdbId, MediaType, bool)>,
    generation: u64,
    details: Option<TitleDetails>,
    progress: Option<ProgressRecord>,
    seasons: Vec<SeasonInfo>,
    selected: Option<u32>,
    episodes: Vec<EpisodeInfo>,
    episodes_generation: u64,
}

struct Graphs {
    home: FocusGraph,
    search: FocusGraph,
    details: FocusGraph,
    settings: FocusGraph,
    login: FocusGraph,
    queue: FocusGraph,
    library: FocusGraph,
    /// Placeholder screens (Queue, Library, Settings, Login, Player).
    other: FocusGraph,
}

impl Graphs {
    fn get(&self, screen: Screen) -> &FocusGraph {
        match screen {
            Screen::Home => &self.home,
            Screen::Search => &self.search,
            Screen::Details => &self.details,
            Screen::Settings => &self.settings,
            Screen::Login => &self.login,
            Screen::Queue => &self.queue,
            Screen::Library => &self.library,
            _ => &self.other,
        }
    }

    fn get_mut(&mut self, screen: Screen) -> &mut FocusGraph {
        match screen {
            Screen::Home => &mut self.home,
            Screen::Search => &mut self.search,
            Screen::Details => &mut self.details,
            Screen::Settings => &mut self.settings,
            Screen::Login => &mut self.login,
            Screen::Queue => &mut self.queue,
            Screen::Library => &mut self.library,
            _ => &mut self.other,
        }
    }
}

/// The UI-thread side of the app.
pub struct Shell {
    ui: slint::Weak<AppWindow>,
    services: Arc<Services>,
    exec: Exec,
    router: RefCell<Router>,
    graphs: RefCell<Graphs>,
    home: RefCell<HomeModel>,
    search: RefCell<SearchModel>,
    details: RefCell<DetailsModel>,
    library: RefCell<Arc<dyn LibraryView>>,
    images: RefCell<HashMap<String, Image>>,
    continue_cards: CardList,
    library_cards: CardList,
    movie_cards: CardList,
    tv_cards: CardList,
    search_cards: CardList,
    seasons: Rc<VecModel<Season>>,
    episodes: Rc<VecModel<Episode>>,
    player: RefCell<Option<Rc<PlayerView>>>,
    player_deps: RefCell<Option<Rc<PlayerDeps>>>,
    /// Bumped whenever the Telegram stack is replaced; auth watchers of an older stack
    /// stop feeding the shell.
    auth_generation: Cell<u64>,
    settings_screen: settings_screen::SettingsScreen,
    login_screen: login_screen::LoginScreen,
    ingest: ingest_shell::IngestShell,
}

fn modifiers(control: bool, shift: bool, alt: bool, meta: bool) -> Modifiers {
    Modifiers {
        control,
        shift,
        alt,
        meta,
    }
}

impl Shell {
    /// Creates the shell for `ui` and wires its callbacks. Call [`Shell::start`]
    /// to begin loading.
    pub fn new(ui: &AppWindow, services: Arc<Services>, exec: Exec) -> Rc<Shell> {
        let settings = services.settings.get();
        let status = services.telegram().status();
        let shell = Rc::new(Shell {
            ui: ui.as_weak(),
            services,
            exec,
            router: RefCell::new(Router::new()),
            graphs: RefCell::new(Graphs {
                home: FocusGraph::with_zones(hvm::zones(0, 0, 0, 0)),
                search: FocusGraph::with_zones(svm::zones(0, 1)),
                details: FocusGraph::with_zones(dvm::zones(false, 0, 0)),
                settings: FocusGraph::new(),
                login: FocusGraph::new(),
                queue: FocusGraph::with_zones(qvm::zones(&[])),
                library: FocusGraph::with_zones(lvm::zones(0)),
                other: FocusGraph::new(),
            }),
            home: RefCell::new(HomeModel {
                status,
                library: hvm::LibraryLoad::Idle,
                library_generation: 0,
                movies: hvm::RowLoad::Loading,
                tv: hvm::RowLoad::Loading,
                settings,
            }),
            search: RefCell::new(SearchModel::default()),
            details: RefCell::new(DetailsModel::default()),
            library: RefCell::new(Arc::new(EmptyLibrary)),
            images: RefCell::new(HashMap::new()),
            continue_cards: CardList::new(),
            library_cards: CardList::new(),
            movie_cards: CardList::new(),
            tv_cards: CardList::new(),
            search_cards: CardList::new(),
            seasons: Rc::new(VecModel::default()),
            episodes: Rc::new(VecModel::default()),
            player: RefCell::new(None),
            player_deps: RefCell::new(None),
            auth_generation: Cell::new(0),
            settings_screen: settings_screen::SettingsScreen::new(),
            login_screen: login_screen::LoginScreen::new(),
            ingest: ingest_shell::IngestShell::default(),
        });
        shell.bind(ui);
        shell
    }

    fn bind(self: &Rc<Self>, ui: &AppWindow) {
        let home = ui.global::<HomeState>();
        home.set_continue_items(ModelRc::from(self.continue_cards.model.clone()));
        home.set_library_items(ModelRc::from(self.library_cards.model.clone()));
        home.set_movies_items(ModelRc::from(self.movie_cards.model.clone()));
        home.set_tv_items(ModelRc::from(self.tv_cards.model.clone()));
        ui.global::<SearchState>()
            .set_results(ModelRc::from(self.search_cards.model.clone()));
        let details = ui.global::<DetailsState>();
        details.set_seasons(ModelRc::from(self.seasons.clone()));
        details.set_episodes(ModelRc::from(self.episodes.clone()));

        let shell = self.clone();
        ui.on_key(move |text, control, shift, alt, meta, repeat| {
            if let Some(player) = shell.player_view() {
                return player.key(&text, modifiers(control, shift, alt, meta), repeat);
            }
            shell.key(&text, modifiers(control, shift, alt, meta))
        });
        let shell = self.clone();
        ui.on_key_released(move |text| {
            shell
                .player_view()
                .is_some_and(|player| player.key_released(&text))
        });
        let player = ui.global::<PlayerState>();
        let shell = self.clone();
        player.on_pointer_moved(move |x, y| {
            if let Some(player) = shell.player_view() {
                player.pointer_moved(x, y);
            }
        });
        let shell = self.clone();
        player.on_video_clicked(move || {
            if let Some(player) = shell.player_view() {
                player.video_clicked();
            }
        });
        let shell = self.clone();
        player.on_menu_clicked(move || {
            if let Some(player) = shell.player_view() {
                player.menu_clicked();
            }
        });
        let shell = self.clone();
        player.on_seek(move |fraction| {
            if let Some(player) = shell.player_view() {
                player.seek(fraction);
            }
        });

        let focus = ui.global::<FocusState>();
        let shell = self.clone();
        focus.on_hovered(move |zone, index| {
            if let Ok(index) = usize::try_from(index) {
                shell.hover(ZoneId(zone), index);
            }
        });
        let shell = self.clone();
        focus.on_clicked(move |zone, index| {
            if let Ok(index) = usize::try_from(index) {
                shell.click(ZoneId(zone), index);
            }
        });

        let search = ui.global::<SearchState>();
        let shell = self.clone();
        search.on_edited(move |text| shell.search_edited(&text, false));
        let shell = self.clone();
        search.on_accepted(move |text| shell.search_edited(&text, true));

        self.bind_settings(ui);
        self.bind_login(ui);
        self.bind_ingest(ui);
    }

    /// Starts every Home load and the Telegram and settings watchers.
    pub fn start(self: &Rc<Self>) {
        self.graphs.borrow_mut().home.focus_first();
        self.refresh_continue();
        self.load_trending(MediaType::Movie);
        self.load_trending(MediaType::Tv);
        self.render_library();
        self.maybe_load_library();

        self.follow_auth();
        let shell = self.clone();
        self.exec
            .watch(self.services.settings.subscribe(), move |s| {
                shell.on_settings(s)
            });
        self.sync_focus();
    }

    // -- helpers ------------------------------------------------------------

    fn screen(&self) -> Screen {
        self.router.borrow().current().screen()
    }

    fn with_graph<R>(&self, f: impl FnOnce(&mut FocusGraph) -> R) -> R {
        let screen = self.screen();
        f(self.graphs.borrow_mut().get_mut(screen))
    }

    fn focus(&self) -> Option<Focus> {
        let screen = self.screen();
        self.graphs.borrow().get(screen).focus()
    }

    /// Writes the current screen's focus to `FocusState`.
    fn sync_focus(&self) {
        let (focus, editing) = {
            let screen = self.screen();
            let graphs = self.graphs.borrow();
            let graph = graphs.get(screen);
            (graph.focus(), graph.editing())
        };
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let (zone, index) = focus.map(Focus::to_slint).unwrap_or((-1, -1));
        let state = ui.global::<FocusState>();
        state.set_zone(zone);
        state.set_index(index);
        state.set_editing(editing);
    }

    fn set_len(&self, screen: Screen, zone: ZoneId, len: usize) {
        self.graphs.borrow_mut().get_mut(screen).set_len(zone, len);
        if self.screen() == screen {
            self.sync_focus();
        }
    }

    fn cards(&self, which: Cards) -> &CardList {
        match which {
            Cards::Continue => &self.continue_cards,
            Cards::Library => &self.library_cards,
            Cards::Movies => &self.movie_cards,
            Cards::Tv => &self.tv_cards,
            Cards::Search => &self.search_cards,
        }
    }

    /// Replaces a card list and starts its poster loads.
    fn set_cards(self: &Rc<Self>, which: Cards, cards: Vec<hvm::CardData>) {
        let list = self.cards(which);
        let generation = list.generation.get().wrapping_add(1);
        list.generation.set(generation);
        let rows: Vec<Card> = cards
            .iter()
            .map(|c| Card {
                title: c.title.as_str().into(),
                meta: c.meta.as_str().into(),
                poster: Image::default(),
                progress: c.progress,
            })
            .collect();
        let posters: Vec<(usize, String)> = cards
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.poster.clone().map(|p| (i, p)))
            .collect();
        *list.data.borrow_mut() = cards;
        list.model.set_vec(rows);
        for (index, path) in posters {
            self.load_image(&path, ImageSize::W342, move |shell, image| {
                let list = shell.cards(which);
                if list.generation.get() != generation {
                    return;
                }
                if let Some(mut row) = list.model.row_data(index) {
                    row.poster = image;
                    list.model.set_row_data(index, row);
                }
            });
        }
    }

    /// Loads an image and hands it to `apply` on the UI thread.
    fn load_image(
        self: &Rc<Self>,
        path: &str,
        size: ImageSize,
        apply: impl FnOnce(&Rc<Shell>, Image) + 'static,
    ) {
        let key = format!("{}{path}", size.as_str());
        let cached = self.images.borrow().get(&key).cloned();
        if let Some(image) = cached {
            apply(self, image);
            return;
        }
        let source = self.services.images.clone();
        let owned = path.to_owned();
        let shell = self.clone();
        self.exec.run(
            async move { source.image(&owned, size).await },
            move |result| match result {
                Ok(rgba) => {
                    if let Some(image) = to_image(&rgba) {
                        {
                            let mut images = shell.images.borrow_mut();
                            if images.len() >= UI_IMAGE_ENTRIES {
                                images.clear();
                            }
                            images.insert(key, image.clone());
                        }
                        apply(&shell, image);
                    }
                }
                Err(e) => tracing::debug!("image {key}: {e}"),
            },
        );
    }

    // -- input --------------------------------------------------------------

    /// A key press from the root FocusScope. True when handled.
    pub fn key(self: &Rc<Self>, text: &str, modifiers: Modifiers) -> bool {
        if let Some(handled) = self.ingest_key(text, modifiers) {
            return handled;
        }
        let screen = self.screen();
        if screen == Screen::Search {
            self.update_search_columns();
        }
        let editing = self.graphs.borrow().get(screen).editing();
        let Some(action) = key_action(text, modifiers, editing) else {
            return false;
        };
        if let KeyAction::Move(direction @ (Direction::Left | Direction::Right)) = action {
            if self.settings_cycle(direction) {
                return true;
            }
        }
        match action {
            KeyAction::Search => {
                if screen == Screen::Search {
                    self.with_graph(|g| g.focus_zone(svm::INPUT));
                    self.sync_focus();
                } else {
                    self.navigate(Route::Search);
                }
            }
            KeyAction::Settings => self.navigate(Route::Settings),
            KeyAction::Fullscreen => {
                if let Some(ui) = self.ui.upgrade() {
                    let window = ui.window();
                    window.set_fullscreen(!window.is_fullscreen());
                }
            }
            KeyAction::Back => self.back_key(),
            KeyAction::Move(direction) => self.move_focus(direction),
            KeyAction::Center => {
                if let Some(focus) = self.focus() {
                    self.activate(focus);
                }
            }
            KeyAction::Menu | KeyAction::Space | KeyAction::Reload => return false,
        }
        true
    }

    fn move_focus(&self, direction: Direction) {
        self.with_graph(|g| g.move_focus(direction));
        self.sync_focus();
    }

    fn hover(&self, zone: ZoneId, index: usize) {
        if let Some(player) = self.player_view() {
            player.hover(zone, index);
            return;
        }
        if self.with_graph(|g| g.hover(zone, index)).is_some() {
            self.sync_focus();
        }
    }

    fn click(self: &Rc<Self>, zone: ZoneId, index: usize) {
        if let Some(player) = self.player_view() {
            player.click(zone, index);
            return;
        }
        if let Some(focus) = self.with_graph(|g| g.click(zone, index)) {
            self.sync_focus();
            self.activate(focus);
        }
    }

    fn back_key(self: &Rc<Self>) {
        if self.close_settings_dialog() || self.ingest_back() {
            return;
        }
        if self.screen() == Screen::Search {
            let in_grid = self.focus().is_some_and(|f| f.zone == svm::GRID);
            let results = self.search_cards.len();
            if svm::back(in_grid, results) == svm::Back::FocusInput {
                self.with_graph(|g| g.focus_zone(svm::INPUT));
                self.sync_focus();
                return;
            }
        }
        self.back();
    }

    /// CENTER or a click on the focused item.
    fn activate(self: &Rc<Self>, focus: Focus) {
        let target = match (self.screen(), focus.zone) {
            (Screen::Home, hvm::TOP_BAR) => hvm::TOP_BAR_ROUTES.get(focus.index).cloned(),
            (Screen::Home, hvm::CONTINUE) => self.continue_cards.target(focus.index),
            (Screen::Home, hvm::LIBRARY) => {
                let row = {
                    let home = self.home.borrow();
                    hvm::library_row(&home.status, &home.library)
                };
                row.prompt_route()
                    .or_else(|| self.library_cards.target(focus.index))
            }
            (Screen::Home, hvm::MOVIES) => self.movie_cards.target(focus.index),
            (Screen::Home, hvm::TV) => self.tv_cards.target(focus.index),
            (Screen::Search, svm::GRID) => self.search_cards.target(focus.index),
            (Screen::Details, dvm::PLAY) => self.details_play().map(Route::Player),
            (Screen::Details, dvm::SEASONS) => {
                self.select_season(focus.index);
                None
            }
            (Screen::Details, dvm::EPISODES) => self.episode_play(focus.index).map(Route::Player),
            (Screen::Settings, _) => self.settings_activate(focus),
            (Screen::Login, _) => self.login_activate(focus),
            (Screen::Details, dvm::INGEST) => {
                self.ingest_bar(focus.index);
                None
            }
            (Screen::Details, zone) if ivm::dialog_zone(zone) => {
                self.dialog_activate(focus);
                None
            }
            (Screen::Queue, _) => {
                self.queue_activate(focus);
                None
            }
            (Screen::Library, _) => {
                self.library_activate(focus);
                None
            }
            _ => None,
        };
        if let Some(route) = target {
            self.navigate(route);
        }
    }

    // -- navigation ---------------------------------------------------------

    /// Opens `route`.
    pub fn navigate(self: &Rc<Self>, route: Route) {
        let (closed, transition) = self.router.borrow_mut().push(route);
        for route in &closed {
            self.closed(route);
        }
        self.show(transition);
    }

    /// Closes the top screen.
    pub fn back(self: &Rc<Self>) {
        let transition = self.router.borrow_mut().back();
        if let Transition::Popped { from, .. } = &transition {
            self.closed(from);
        }
        self.show(transition);
    }

    /// The route on top.
    pub fn route(&self) -> Route {
        self.router.borrow().current().clone()
    }

    fn closed(&self, route: &Route) {
        match route {
            Route::Search => self.search.borrow_mut().debounce.reset(),
            Route::Player(_) => {
                let player = self.player.borrow_mut().take();
                if let Some(player) = player {
                    player.close();
                }
            }
            Route::Details { .. } => {
                let mut d = self.details.borrow_mut();
                d.generation = d.generation.wrapping_add(1);
                d.episodes_generation = d.episodes_generation.wrapping_add(1);
                drop(d);
                self.ingest_details_closed();
            }
            _ => {}
        }
    }

    fn show(self: &Rc<Self>, transition: Transition) {
        let (route, fresh) = match transition {
            Transition::Pushed { to } => (to, true),
            Transition::Popped { to, .. } => (to, false),
            Transition::Stayed => return,
        };
        if let Some(ui) = self.ui.upgrade() {
            ui.set_screen(route.screen());
        }
        match &route {
            Route::Home => self.refresh_continue(),
            Route::Search if fresh => self.open_search(),
            Route::Details {
                id,
                media,
                library_only,
            } => {
                if fresh {
                    self.open_details(*id, *media, *library_only);
                } else {
                    self.refresh_details_progress();
                }
            }
            Route::Player(request) => {
                self.open_player(*request);
                return;
            }
            Route::Settings => self.show_settings(fresh),
            Route::Login => self.show_login(fresh),
            Route::Queue => self.open_queue(),
            Route::Library => self.open_library(fresh),
            _ => {}
        }
        self.sync_focus();
    }

    // -- player -------------------------------------------------------------

    /// What the player uses beyond the services (mpv, Telegram, the underlay).
    pub fn set_player_deps(&self, deps: PlayerDeps) {
        *self.player_deps.borrow_mut() = Some(Rc::new(deps));
    }

    /// The open player, while its screen shows.
    pub fn player_view(&self) -> Option<Rc<PlayerView>> {
        if self.screen() != Screen::Player {
            return None;
        }
        self.player.borrow().clone()
    }

    fn open_player(self: &Rc<Self>, request: PlayRequest) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        tracing::info!("play {request:?}");
        let deps = self
            .player_deps
            .borrow()
            .clone()
            .unwrap_or_else(|| Rc::new(PlayerDeps::offline()));
        let prints = LibraryPrints::new(
            self.home.borrow().status.ready(),
            self.library.borrow().clone(),
        );
        let shell = Rc::downgrade(self);
        let on_exit = Box::new(move || {
            if let Some(shell) = shell.upgrade() {
                if shell.screen() == Screen::Player {
                    shell.back();
                }
            }
        });
        let view = PlayerView::open(
            &ui,
            self.services.clone(),
            self.exec.clone(),
            deps,
            request,
            prints,
            on_exit,
        );
        let previous = self.player.borrow_mut().replace(view);
        if let Some(previous) = previous {
            previous.close();
        }
        if let Some(player) = self.player.borrow().as_ref() {
            player.render();
        }
    }

    // -- home ---------------------------------------------------------------

    fn refresh_continue(self: &Rc<Self>) {
        let s = self.services.settings.get();
        let limit = usize::try_from(s.continue_watching_limit).unwrap_or(usize::MAX);
        let cards: Vec<hvm::CardData> = self
            .services
            .progress
            .continue_watching(s.finished_threshold_percent, limit)
            .iter()
            .map(hvm::continue_card)
            .collect();
        let len = cards.len();
        self.set_cards(Cards::Continue, cards);
        self.set_len(Screen::Home, hvm::CONTINUE, len);
    }

    fn load_trending(self: &Rc<Self>, media: MediaType) {
        self.set_trending(media, hvm::RowLoad::Loading);
        let catalog = self.services.catalog.clone();
        let shell = self.clone();
        self.exec.run(
            async move { catalog.trending(media).await },
            move |result| {
                let load = match result {
                    Ok(list) => hvm::RowLoad::Loaded(
                        list.iter().map(|t| hvm::media_card(t, false)).collect(),
                    ),
                    Err(e) => {
                        tracing::warn!("trending {media:?}: {e}");
                        hvm::RowLoad::Failed
                    }
                };
                shell.set_trending(media, load);
            },
        );
    }

    fn set_trending(self: &Rc<Self>, media: MediaType, load: hvm::RowLoad) {
        let stamp = load.stamp().unwrap_or_default();
        let cards = load.cards().to_vec();
        let len = cards.len();
        {
            let mut home = self.home.borrow_mut();
            match media {
                MediaType::Movie => home.movies = load,
                MediaType::Tv => home.tv = load,
            }
        }
        let (which, zone) = match media {
            MediaType::Movie => (Cards::Movies, hvm::MOVIES),
            MediaType::Tv => (Cards::Tv, hvm::TV),
        };
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<HomeState>();
            match media {
                MediaType::Movie => state.set_movies_state(stamp.into()),
                MediaType::Tv => state.set_tv_state(stamp.into()),
            }
        }
        self.set_cards(which, cards);
        self.set_len(Screen::Home, zone, len);
    }

    fn render_library(self: &Rc<Self>) {
        let (row, ready) = {
            let home = self.home.borrow();
            (
                hvm::library_row(&home.status, &home.library),
                home.status.ready(),
            )
        };
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<HomeState>();
            state.set_library_prompt(row.prompt().unwrap_or_default().into());
            state.set_library_state(row.stamp().unwrap_or_default().into());
            state.set_telegram_ready(ready);
        }
        self.set_cards(Cards::Library, row.cards().to_vec());
        self.set_len(Screen::Home, hvm::LIBRARY, row.focusable_len());
    }

    fn maybe_load_library(self: &Rc<Self>) {
        let should = {
            let home = self.home.borrow();
            hvm::should_load_library(&home.status, &home.library)
        };
        if should {
            self.load_library();
        }
    }

    fn load_library(self: &Rc<Self>) {
        let Some(source) = self.services.telegram().library() else {
            return;
        };
        let generation = {
            let mut home = self.home.borrow_mut();
            home.library_generation = home.library_generation.wrapping_add(1);
            home.library = hvm::LibraryLoad::Loading;
            home.library_generation
        };
        self.render_library();
        let settings = self.services.settings.get();
        let catalog = self.services.catalog.clone();
        let shell = self.clone();
        self.exec.run(
            async move {
                let view = source.refresh(&settings.telegram_channel).await?;
                let lookups = view
                    .titles()
                    .into_iter()
                    .map(|(id, media)| catalog.details(media, id));
                let mut titles = Vec::new();
                for resolved in futures::future::join_all(lookups).await {
                    match resolved {
                        Ok(d) => titles.push(hvm::library_title(d, view.as_ref())),
                        Err(e) => tracing::warn!("library title: {e}"),
                    }
                }
                Ok::<_, flox_core::Error>((view, hvm::sort_library(titles, settings.library_sort)))
            },
            move |result| shell.library_loaded(generation, result),
        );
    }

    fn library_loaded(
        self: &Rc<Self>,
        generation: u64,
        result: Result<(Arc<dyn LibraryView>, Vec<TitleSummary>)>,
    ) {
        {
            let mut home = self.home.borrow_mut();
            if home.library_generation != generation {
                return;
            }
            home.library = match result {
                Ok((view, titles)) => {
                    *self.library.borrow_mut() = view;
                    hvm::LibraryLoad::Loaded(
                        titles.iter().map(|t| hvm::media_card(t, true)).collect(),
                    )
                }
                Err(e) => {
                    tracing::warn!("library: {e}");
                    hvm::LibraryLoad::Failed
                }
            };
        }
        self.render_library();
    }

    fn invalidate_library(&self) {
        let mut home = self.home.borrow_mut();
        home.library_generation = home.library_generation.wrapping_add(1);
        home.library = hvm::LibraryLoad::Idle;
    }

    /// Follows the auth state of the Telegram stack in [`Services::telegram`], if it
    /// is connected. Watchers installed for an earlier stack go quiet.
    fn follow_auth(self: &Rc<Self>) {
        let generation = self.auth_generation.get().wrapping_add(1);
        self.auth_generation.set(generation);
        if let Telegram::Connected { auth, .. } = self.services.telegram() {
            let shell = Rc::downgrade(self);
            self.exec.watch(auth.state(), move |state| {
                if let Some(shell) = shell.upgrade() {
                    if shell.auth_generation.get() == generation {
                        shell.on_auth(state);
                    }
                }
            });
        }
    }

    /// [`Services::telegram`] was replaced (new credentials, or Telegram started or
    /// stopped while the app runs): show its status and follow its auth state.
    pub fn telegram_replaced(self: &Rc<Self>) {
        self.follow_auth();
        let status = self.services.telegram().status();
        self.on_status(status);
    }

    /// Gives the player the Telegram client and library that [`Services::telegram`]
    /// now uses (the library path and its subtitles).
    pub fn set_player_telegram(&self, td: Option<TdAccess>, library: Option<Arc<Library>>) {
        let mut slot = self.player_deps.borrow_mut();
        let Some(current) = slot.as_ref() else {
            return;
        };
        *slot = Some(Rc::new(PlayerDeps {
            mpv_lib: current.mpv_lib.clone(),
            td,
            library,
            runtime: current.runtime.clone(),
            sniffer: current.sniffer.clone(),
            dev_file: current.dev_file.clone(),
            underlay: current.underlay.clone(),
        }));
    }

    fn on_auth(self: &Rc<Self>, state: AuthState) {
        self.on_status(hvm::TelegramStatus::Auth(state));
    }

    fn on_status(self: &Rc<Self>, status: hvm::TelegramStatus) {
        let ready = status.ready();
        self.home.borrow_mut().status = status;
        if !ready {
            self.invalidate_library();
            *self.library.borrow_mut() = Arc::new(EmptyLibrary);
        }
        self.render_library();
        self.maybe_load_library();
        self.account_auth_changed();
    }

    fn on_settings(self: &Rc<Self>, next: Settings) {
        let previous = std::mem::replace(&mut self.home.borrow_mut().settings, next.clone());
        if hvm::library_settings_changed(&previous, &next) {
            self.invalidate_library();
            self.render_library();
            self.maybe_load_library();
        }
        if previous.effective_tmdb_api_key() != next.effective_tmdb_api_key() {
            self.load_trending(MediaType::Movie);
            self.load_trending(MediaType::Tv);
        }
        if previous.continue_watching_limit != next.continue_watching_limit
            || previous.finished_threshold_percent != next.finished_threshold_percent
        {
            self.refresh_continue();
        }
        self.render_settings();
    }

    // -- search -------------------------------------------------------------

    fn open_search(self: &Rc<Self>) {
        self.search.borrow_mut().debounce.reset();
        self.search.borrow_mut().results.clear();
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<SearchState>();
            state.set_query("".into());
            state.set_state("".into());
            state.set_scroll_y(0.0);
        }
        self.set_cards(Cards::Search, Vec::new());
        let columns = self.search_columns();
        let mut graph = FocusGraph::with_zones(svm::zones(0, columns));
        graph.focus_zone(svm::INPUT);
        self.graphs.borrow_mut().search = graph;
    }

    /// The grid's column count for the current window width, as search.slint lays it out.
    fn search_columns(&self) -> usize {
        self.ui
            .upgrade()
            .map(|ui| {
                let window = ui.window();
                svm::columns(window.size().to_logical(window.scale_factor()).width)
            })
            .unwrap_or(1)
    }

    fn update_search_columns(&self) {
        let columns = self.search_columns();
        self.graphs
            .borrow_mut()
            .search
            .set_columns(svm::GRID, columns);
    }

    /// The query text changed; `immediate` for Enter.
    pub fn search_edited(self: &Rc<Self>, text: &str, immediate: bool) {
        let plan = self.search.borrow_mut().debounce.edit(text, immediate);
        match plan {
            svm::Plan::Clear => self.show_results(svm::Results::Idle, Vec::new()),
            svm::Plan::Run {
                query,
                delay,
                generation,
            } => {
                let shell = self.clone();
                self.exec
                    .after(delay, move || shell.search_fire(generation, query));
            }
        }
    }

    fn search_fire(self: &Rc<Self>, generation: u64, query: String) {
        if !self.search.borrow_mut().debounce.fire(generation, &query) {
            return;
        }
        self.show_results(svm::Results::Loading, Vec::new());
        let catalog = self.services.catalog.clone();
        let shell = self.clone();
        self.exec
            .run(async move { catalog.search(&query).await }, move |result| {
                if !shell.search.borrow().debounce.is_current(generation) {
                    return;
                }
                match result {
                    Ok(list) => {
                        let cards: Vec<hvm::CardData> =
                            list.iter().map(|t| hvm::media_card(t, false)).collect();
                        shell.show_results(svm::Results::Found(cards.len()), cards);
                    }
                    Err(e) => {
                        tracing::warn!("search: {e}");
                        shell.show_results(svm::Results::Failed, Vec::new());
                    }
                }
            });
    }

    fn show_results(self: &Rc<Self>, results: svm::Results, cards: Vec<hvm::CardData>) {
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<SearchState>();
            state.set_state(results.stamp().unwrap_or_default().into());
            state.set_scroll_y(0.0);
        }
        let len = cards.len();
        self.search.borrow_mut().results = cards.clone();
        self.set_cards(Cards::Search, cards);
        self.update_search_columns();
        self.set_len(Screen::Search, svm::GRID, len);
    }

    // -- details ------------------------------------------------------------

    fn open_details(self: &Rc<Self>, id: TmdbId, media: MediaType, library_only: bool) {
        let progress = self.services.progress.get(media, id);
        let generation = {
            let mut d = self.details.borrow_mut();
            let generation = d.generation.wrapping_add(1);
            *d = DetailsModel {
                route: Some((id, media, library_only)),
                generation,
                progress,
                episodes_generation: d.episodes_generation.wrapping_add(1),
                ..DetailsModel::default()
            };
            generation
        };
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<DetailsState>();
            state.set_state(hvm::LOADING.into());
            state.set_poster(Image::default());
            state.set_meta("".into());
            state.set_title("".into());
            state.set_overview("".into());
            state.set_play_label("PLAY".into());
            state.set_resume(false);
            state.set_is_tv(media == MediaType::Tv);
            state.set_library_only(library_only);
            state.set_episodes_state("".into());
            state.set_scroll_y(0.0);
            state.set_seasons_x(0.0);
        }
        self.seasons.set_vec(Vec::new());
        self.episodes.set_vec(Vec::new());
        self.graphs.borrow_mut().details = FocusGraph::with_zones(dvm::zones(false, 0, 0));

        let catalog = self.services.catalog.clone();
        let shell = self.clone();
        self.exec.run(
            async move { catalog.details(media, id).await },
            move |result| shell.details_loaded(generation, result),
        );
    }

    fn details_loaded(self: &Rc<Self>, generation: u64, result: Result<TitleDetails>) {
        if self.details.borrow().generation != generation {
            return;
        }
        let d = match result {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("details: {e}");
                if let Some(ui) = self.ui.upgrade() {
                    ui.global::<DetailsState>()
                        .set_state(hvm::REQUEST_FAILED.into());
                }
                return;
            }
        };
        let library = self.library.borrow().clone();
        let (library_only, progress) = {
            let m = self.details.borrow();
            (m.route.is_some_and(|(_, _, l)| l), m.progress.clone())
        };
        let media = d.summary.media;
        let seasons = if media == MediaType::Tv {
            dvm::visible_seasons(&d, library.as_ref(), library_only)
        } else {
            Vec::new()
        };
        let selected = dvm::initial_season(&seasons, progress.as_ref());
        let label = dvm::play_label(media, progress.as_ref());

        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<DetailsState>();
            state.set_state("".into());
            state.set_meta(dvm::header_meta(&d, library.as_ref()).into());
            state.set_title(d.summary.title.as_str().into());
            state.set_overview(d.summary.overview.as_str().into());
            state.set_play_label(label.text.into());
            state.set_resume(label.resume);
        }
        self.seasons.set_vec(
            seasons
                .iter()
                .map(|s| Season {
                    label: dvm::season_label(s.number).into(),
                    selected: Some(s.number) == selected,
                })
                .collect::<Vec<_>>(),
        );
        let selected_index = seasons
            .iter()
            .position(|s| Some(s.number) == selected)
            .unwrap_or(0);
        {
            let mut graph = FocusGraph::with_zones(dvm::zones(true, seasons.len(), 0));
            graph.remember(dvm::SEASONS, selected_index);
            graph.focus_first();
            self.graphs.borrow_mut().details = graph;
        }
        let poster = d.summary.poster_path.clone();
        {
            let mut m = self.details.borrow_mut();
            m.details = Some(d);
            m.seasons = seasons;
            m.selected = selected;
        }
        self.ingest_details_loaded();
        if self.screen() == Screen::Details {
            self.sync_focus();
        }
        if let Some(path) = poster {
            self.load_image(&path, ImageSize::W500, move |shell, image| {
                if shell.details.borrow().generation != generation {
                    return;
                }
                if let Some(ui) = shell.ui.upgrade() {
                    ui.global::<DetailsState>().set_poster(image);
                }
            });
        }
        if let Some(season) = selected {
            self.load_episodes(season);
        }
    }

    fn load_episodes(self: &Rc<Self>, season: u32) {
        let (id, generation) = {
            let mut m = self.details.borrow_mut();
            m.episodes_generation = m.episodes_generation.wrapping_add(1);
            m.episodes.clear();
            (m.route.map(|(id, _, _)| id), m.episodes_generation)
        };
        self.ingest_season_changed();
        let Some(id) = id else {
            return;
        };
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<DetailsState>()
                .set_episodes_state(hvm::LOADING.into());
        }
        self.episodes.set_vec(Vec::new());
        self.set_len(Screen::Details, dvm::EPISODES, 0);
        let catalog = self.services.catalog.clone();
        let shell = self.clone();
        self.exec.run(
            async move { catalog.season(id, season).await },
            move |result| shell.episodes_loaded(generation, id, result),
        );
    }

    fn episodes_loaded(
        self: &Rc<Self>,
        generation: u64,
        id: TmdbId,
        result: Result<Vec<EpisodeInfo>>,
    ) {
        let library_only = {
            let m = self.details.borrow();
            if m.episodes_generation != generation {
                return;
            }
            m.route.is_some_and(|(_, _, l)| l)
        };
        let library = self.library.borrow().clone();
        let episodes = match result {
            Ok(all) => dvm::visible_episodes(id, all, library.as_ref(), library_only),
            Err(e) => {
                tracing::warn!("season: {e}");
                if let Some(ui) = self.ui.upgrade() {
                    ui.global::<DetailsState>()
                        .set_episodes_state(hvm::REQUEST_FAILED.into());
                }
                return;
            }
        };
        if let Some(ui) = self.ui.upgrade() {
            let stamp = if episodes.is_empty() {
                hvm::NO_RESULTS
            } else {
                ""
            };
            ui.global::<DetailsState>().set_episodes_state(stamp.into());
        }
        let rows: Vec<Episode> = episodes
            .iter()
            .map(|e| Episode {
                still: Image::default(),
                meta: dvm::episode_meta(
                    e,
                    &qualities(
                        library.as_ref(),
                        EpisodeKey::episode(id, e.season, e.episode),
                    ),
                )
                .into(),
                name: e.name.as_str().into(),
                overview: e.overview.as_str().into(),
                selected: false,
            })
            .collect();
        let stills: Vec<(usize, String)> = episodes
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.still_path.clone().map(|p| (i, p)))
            .collect();
        let len = episodes.len();
        self.details.borrow_mut().episodes = episodes;
        self.episodes.set_vec(rows);
        self.set_len(Screen::Details, dvm::EPISODES, len);
        self.ingest_episodes_loaded();
        for (index, path) in stills {
            self.load_image(&path, ImageSize::W300, move |shell, image| {
                if shell.details.borrow().episodes_generation != generation {
                    return;
                }
                if let Some(mut row) = shell.episodes.row_data(index) {
                    row.still = image;
                    shell.episodes.set_row_data(index, row);
                }
            });
        }
    }

    fn select_season(self: &Rc<Self>, index: usize) {
        let season = {
            let mut m = self.details.borrow_mut();
            let Some(number) = m.seasons.get(index).map(|s| s.number) else {
                return;
            };
            if m.selected == Some(number) {
                return;
            }
            m.selected = Some(number);
            number
        };
        for i in 0..self.seasons.row_count() {
            if let Some(mut row) = self.seasons.row_data(i) {
                row.selected = i == index;
                self.seasons.set_row_data(i, row);
            }
        }
        self.load_episodes(season);
    }

    fn refresh_details_progress(&self) {
        let (progress, media) = {
            let mut m = self.details.borrow_mut();
            let Some((id, media, _)) = m.route else {
                return;
            };
            m.progress = self.services.progress.get(media, id);
            (m.progress.clone(), media)
        };
        let label = dvm::play_label(media, progress.as_ref());
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<DetailsState>();
            state.set_play_label(label.text.into());
            state.set_resume(label.resume);
        }
    }

    fn details_play(&self) -> Option<PlayRequest> {
        let m = self.details.borrow();
        let d = m.details.as_ref()?;
        let library_only = m.route.is_some_and(|(_, _, l)| l);
        let first = if library_only {
            m.episodes.first().map(|e| (e.season, e.episode))
        } else {
            None
        };
        Some(dvm::play_request(
            d,
            m.progress.as_ref(),
            first,
            library_only,
        ))
    }

    fn episode_play(&self, index: usize) -> Option<PlayRequest> {
        let m = self.details.borrow();
        let (id, _, library_only) = m.route?;
        let e = m.episodes.get(index)?;
        Some(dvm::episode_request(
            id,
            e,
            m.progress.as_ref(),
            library_only,
        ))
    }
}

/// Applies the `ui_scale` setting on top of the monitor's scale factor.
pub fn apply_ui_scale(ui: &AppWindow, ui_scale: f32) {
    if (ui_scale - 1.0).abs() < f32::EPSILON {
        return;
    }
    let window = ui.window();
    let scale_factor = window.scale_factor() * ui_scale;
    window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor });
}

/// Opens the main window and runs the event loop until it closes.
pub fn run(ctx: AppContext) -> anyhow::Result<()> {
    let ui = AppWindow::new()?;
    ui.set_version(flox_core::VERSION.into());
    let shell = Shell::new(
        &ui,
        ctx.services.clone(),
        Exec::Live(ctx.runtime.handle().clone()),
    );
    shell.set_ingest(Ingest::from_context(&ctx));
    let underlay = match Underlay::install(&ui) {
        Ok(underlay) => Some(underlay),
        Err(e) => {
            tracing::warn!("no video underlay: {e}");
            None
        }
    };
    shell.set_player_deps(PlayerDeps {
        mpv_lib: ctx.player_lib.clone(),
        td: ctx.td.clone().map(|td| TdAccess {
            transport: td,
            runtime: ctx.runtime.handle().clone(),
        }),
        library: ctx.library.clone(),
        runtime: Some(ctx.runtime.handle().clone()),
        sniffer: flox_web::platform_sniffer(flox_web::assets::ScriptOptions::default()),
        dev_file: ctx.dev_play.clone(),
        underlay,
    });
    // Fixtures have no Telegram to restart; everything else follows Settings.
    let _integration = (!matches!(ctx.services.telegram(), Telegram::Offline { .. })).then(|| {
        let integration = Integration::new(
            ctx.runtime.handle().clone(),
            ctx.paths.clone(),
            ctx.services.clone(),
            &shell,
            TelegramStack {
                telegram: ctx.services.telegram(),
                client: ctx.td.clone(),
                library: ctx.library.clone(),
            },
            ctx.queue.clone(),
        );
        integration.install(&shell);
        integration
    });
    shell.start();
    if ctx.dev_play.is_some() {
        shell.navigate(Route::Player(PlayRequest {
            key: EpisodeKey::movie(0),
            start_at: None,
            library_only: false,
        }));
    }
    ui.show()?;
    apply_ui_scale(&ui, ctx.settings.get().ui_scale);
    slint::run_event_loop()?;
    ui.hide()?;
    drop(shell);
    drop(ui);
    ctx.runtime.shutdown_timeout(Duration::from_secs(2));
    Ok(())
}
