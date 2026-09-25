//! The player screen: the mpv OpenGL underlay, the overlay state, key and mouse routing,
//! media keys, keep-awake, and the controller's effects.
//!
//! - [`Underlay`] owns the window's rendering notifier. When a player attaches an mpv
//!   instance, the next `BeforeRendering` creates the `GlRenderer` with the window's
//!   `get_proc_address` and renders into framebuffer 0 (flipped Y) before Slint paints the
//!   overlay; `AfterRendering` reports the swap. mpv's update callback asks the event loop for
//!   a redraw. The renderer is freed inside a notifier call (the GL context is current), and
//!   on `RenderingTeardown`.
//! - [`PlayerView`] runs one playback: it builds the [`Controller`] over [`MpvEngine`], feeds it
//!   keys, pointer events, mpv events and a 250 ms tick, carries out its effects through
//!   [`Exec`], and writes [`PlayerState`] and the overlay focus.
//!
//! Without libmpv every load fails, the controller ends in PLAYBACK FAILED and the hint says
//! LIBMPV NOT FOUND. The child-HWND compositing fallback (plan section 2) would replace only
//! [`Underlay`] and the engine's `vo`; it is not built.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use flox_core::model::EpisodeKey;
use flox_core::sniff::{SniffMode, SniffResult, Sniffer, StreamKind};
use flox_player::ffi::MpvLib;
use flox_player::mpv::{Mpv, MpvEvent};
use flox_player::render::GlRenderer;
use flox_sys::media_keys::{MediaControls, MediaKey};
use flox_sys::power::KeepAwake;
use flox_td::library::{Entry, Library};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tokio_util::sync::CancellationToken;

use super::controller::{
    Button, Controller, Effect, Engine, Input, Key, Meta, Phase, PlayerConfig, Prints, SystemClock,
    ViewState,
};
use super::mpv_engine::{create_mpv, EventMapper, MpvEngine, Streams, TdAccess};
use super::web::PageSurface;
use crate::app::{Exec, LibraryView, Services};
use crate::focus::{key_action, Direction, KeyAction, Modifiers, PointerGate, ZoneId};
use crate::router::PlayRequest;
use crate::ui::{AppWindow, FocusState, PlayerState};

/// The player's focus zones (mirrored by `PlayerZones` in `player.slint`).
pub const BACK: ZoneId = ZoneId(40);
pub const SEEK: ZoneId = ZoneId(41);
pub const BUTTONS: ZoneId = ZoneId(42);
pub const TRACKS: ZoneId = ZoneId(43);
pub const RESUME: ZoneId = ZoneId(44);

/// How often the controller ticks and the overlay refreshes.
pub const REFRESH: Duration = Duration::from_millis(250);
/// How long a subtitle download may take before the print plays without it.
const SUBTITLE_TIMEOUT: Duration = Duration::from_secs(60);
/// The player starts anyway when the underlay has not rendered a frame by then.
const UNDERLAY_GRACE: Duration = Duration::from_secs(1);

/// The row buttons in slot order (their `PlayerZones.buttons` indices).
const SLOTS: [Button; 9] = [
    Button::Rewind,
    Button::PlayPause,
    Button::Forward,
    Button::VolumeDown,
    Button::VolumeUp,
    Button::Audio,
    Button::Subtitles,
    Button::Quality,
    Button::Next,
];

/// The slot index of a row button (`Back` has its own zone).
pub fn button_slot(button: Button) -> Option<usize> {
    SLOTS.iter().position(|b| *b == button)
}

/// The row buttons the overlay shows, left to right.
pub fn visible_buttons(view: &ViewState) -> Vec<Button> {
    SLOTS
        .iter()
        .copied()
        .filter(|b| match b {
            Button::Audio => view.audio_available,
            Button::Subtitles => view.subtitles_available,
            Button::Quality => view.quality_available,
            Button::Next => view.next_available,
            _ => true,
        })
        .collect()
}

/// `LOADING` / `PLAYBACK FAILED` in the middle of the screen, or nothing.
pub fn stamp_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Loading => "LOADING",
        Phase::Failed => "PLAYBACK FAILED",
        _ => "",
    }
}

/// The bottom-right hint: the controller's, or why playback failed when libmpv is missing.
pub fn hint_text(view: &ViewState, mpv_missing: bool) -> String {
    if view.phase == Phase::Failed && mpv_missing {
        "LIBMPV NOT FOUND".to_owned()
    } else {
        view.hint.clone().unwrap_or_default()
    }
}

/// What a key press does on the player screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerKey {
    Key(Key),
    Fullscreen,
    /// Claimed but ignored (Ctrl+F, Ctrl+, would leave the player running underneath).
    Swallow,
}

/// Plan section 1: Enter, Esc/Backspace, arrows, Space, M or the menu key, Ctrl+R, F11.
pub fn player_key(text: &str, modifiers: Modifiers) -> Option<PlayerKey> {
    Some(match key_action(text, modifiers, false)? {
        KeyAction::Center => PlayerKey::Key(Key::Center),
        KeyAction::Back => PlayerKey::Key(Key::Back),
        KeyAction::Move(Direction::Left) => PlayerKey::Key(Key::Left),
        KeyAction::Move(Direction::Right) => PlayerKey::Key(Key::Right),
        KeyAction::Move(Direction::Up) => PlayerKey::Key(Key::Up),
        KeyAction::Move(Direction::Down) => PlayerKey::Key(Key::Down),
        KeyAction::Space => PlayerKey::Key(Key::PlayPause),
        KeyAction::Menu => PlayerKey::Key(Key::Menu),
        KeyAction::Reload => PlayerKey::Key(Key::Reload),
        KeyAction::Fullscreen => PlayerKey::Fullscreen,
        KeyAction::Search | KeyAction::Settings => PlayerKey::Swallow,
    })
}

/// The releases the controller cares about (short versus long CENTER and MENU).
pub fn released_key(text: &str) -> Option<Key> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if c == char::from(slint::platform::Key::Return) {
        Some(Key::Center)
    } else if c == char::from(slint::platform::Key::Menu) || c.eq_ignore_ascii_case(&'m') {
        Some(Key::Menu)
    } else {
        None
    }
}

/// SMTC buttons: Play and Pause stay separate (the OS sends the one the status implies).
pub fn media_key(key: MediaKey) -> Option<Key> {
    match key {
        MediaKey::PlayPause => Some(Key::PlayPause),
        MediaKey::Play => Some(Key::Play),
        MediaKey::Pause => Some(Key::Pause),
        MediaKey::Next => Some(Key::Next),
        MediaKey::Previous | MediaKey::Stop => None,
    }
}

/// Counts auto-repeats of the held key (0 for a fresh press).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepeatCounter {
    held: Option<String>,
    count: u32,
}

impl RepeatCounter {
    pub fn press(&mut self, text: &str, repeat: bool) -> u32 {
        if repeat && self.held.as_deref() == Some(text) {
            self.count = self.count.saturating_add(1);
        } else {
            self.held = Some(text.to_owned());
            self.count = 0;
        }
        self.count
    }

    pub fn release(&mut self) {
        self.held = None;
        self.count = 0;
    }
}

/// What the overlay focus is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Back,
    Seek,
    Button(Button),
    Track(usize),
}

/// Focus navigation inside the overlay and the tracks panel (the controller hands keys over
/// with `PassToOverlay` while the overlay is up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayNav {
    target: Target,
}

impl Default for OverlayNav {
    fn default() -> Self {
        Self {
            target: Target::Button(Button::PlayPause),
        }
    }
}

impl OverlayNav {
    pub fn target(&self) -> Target {
        self.target
    }

    /// Back to play/pause (the overlay just appeared).
    pub fn reset(&mut self) {
        self.target = Target::Button(Button::PlayPause);
    }

    /// Points at something the pointer hovered or clicked.
    pub fn set(&mut self, target: Target) {
        self.target = target;
    }

    /// Keeps the focus on something visible: the open panel's rows, or a shown button.
    pub fn normalize(&mut self, view: &ViewState) {
        match (&view.tracks, self.target) {
            (Some(panel), Target::Track(i)) => {
                let last = panel.labels.len().saturating_sub(1);
                self.target = Target::Track(i.min(last));
            }
            (Some(panel), _) => self.target = Target::Track(panel.selected),
            (None, Target::Track(_)) => {
                self.target = Target::Button(if view.audio_available {
                    Button::Audio
                } else {
                    Button::PlayPause
                });
            }
            (None, Target::Button(b)) if !visible_buttons(view).contains(&b) => {
                self.target = Target::Button(Button::PlayPause);
            }
            _ => {}
        }
    }

    /// A key while the overlay is up; the input to send, if any.
    pub fn key(&mut self, key: Key, repeat: u32, view: &ViewState) -> Option<Input> {
        self.normalize(view);
        if let Target::Track(i) = self.target {
            let len = view.tracks.as_ref().map_or(0, |p| p.labels.len());
            match key {
                Key::Up => self.target = Target::Track(i.saturating_sub(1)),
                Key::Down => self.target = Target::Track((i + 1).min(len.saturating_sub(1))),
                Key::Center if repeat == 0 => return Some(Input::PickTrack(i)),
                _ => {}
            }
            return None;
        }
        match (self.target, key) {
            (Target::Back, Key::Down) => self.target = Target::Seek,
            (Target::Back, Key::Center) if repeat == 0 => return Some(Input::Button(Button::Back)),
            (Target::Seek, Key::Up) => self.target = Target::Back,
            (Target::Seek, Key::Down) => self.target = Target::Button(Button::PlayPause),
            (Target::Seek, Key::Left | Key::Right) => {
                return Some(Input::ScrubBy {
                    forward: key == Key::Right,
                    repeat,
                })
            }
            (Target::Seek, Key::Center) if repeat == 0 => {
                return Some(Input::Button(Button::PlayPause))
            }
            (Target::Button(_), Key::Up) => self.target = Target::Seek,
            (Target::Button(b), Key::Left | Key::Right) => {
                let row = visible_buttons(view);
                if let Some(i) = row.iter().position(|x| *x == b) {
                    let next = if key == Key::Left {
                        i.saturating_sub(1)
                    } else {
                        (i + 1).min(row.len().saturating_sub(1))
                    };
                    self.target = Target::Button(row[next]);
                }
            }
            (Target::Button(b), Key::Center) if repeat == 0 => return Some(Input::Button(b)),
            _ => {}
        }
        None
    }

    /// The (zone, index) to draw the ring on.
    pub fn slint_focus(&self) -> (i32, i32) {
        let (zone, index) = match self.target {
            Target::Back => (BACK, 0),
            Target::Seek => (SEEK, 0),
            Target::Button(b) => (BUTTONS, button_slot(b).unwrap_or(1)),
            Target::Track(i) => (TRACKS, i),
        };
        (zone.0, i32::try_from(index).unwrap_or(0))
    }
}

/// The target for a pointer on (zone, index).
pub fn pointer_target(zone: ZoneId, index: usize) -> Option<Target> {
    match zone {
        BACK => Some(Target::Back),
        SEEK => Some(Target::Seek),
        BUTTONS => SLOTS.get(index).map(|b| Target::Button(*b)),
        TRACKS => Some(Target::Track(index)),
        _ => None,
    }
}

/// Writes the view state to `PlayerState`. `buffered` is the demuxer cache end in seconds.
pub fn apply(ui: &AppWindow, view: &ViewState, buffered: f64, mpv_missing: bool) {
    let s = ui.global::<PlayerState>();
    s.set_stamp(stamp_text(view.phase).into());
    s.set_overlay(view.overlay_visible);
    s.set_asking(view.phase == Phase::Asking);
    s.set_resume_label(view.resume_label.clone().unwrap_or_default().into());
    s.set_eyebrow(view.eyebrow.as_str().into());
    s.set_title(view.title.as_str().into());
    s.set_position(view.position.as_str().into());
    s.set_duration(view.duration.as_str().into());
    s.set_time(view.time as f32);
    s.set_length(view.length as f32);
    s.set_buffered(buffered.clamp(0.0, view.length.max(0.0)) as f32);
    s.set_paused(view.paused);
    s.set_audio_available(view.audio_available);
    s.set_subtitles_available(view.subtitles_available);
    s.set_quality_available(view.quality_available);
    s.set_next_available(view.next_available);
    s.set_tracks_open(view.tracks.is_some());
    if let Some(panel) = &view.tracks {
        s.set_tracks_heading(panel.heading.as_str().into());
        s.set_tracks_selected(i32::try_from(panel.selected).unwrap_or(0));
        let current = s.get_tracks();
        let same = current.row_count() == panel.labels.len()
            && current
                .iter()
                .zip(&panel.labels)
                .all(|(a, b)| a.as_str() == b);
        if !same {
            let rows: Vec<SharedString> = panel.labels.iter().map(|l| l.as_str().into()).collect();
            s.set_tracks(ModelRc::new(VecModel::from(rows)));
        }
    }
    s.set_hint(hint_text(view, mpv_missing).into());
}

/// Clears the screen state (a player is opening or has closed).
pub fn reset(ui: &AppWindow, stamp: &str) {
    let s = ui.global::<PlayerState>();
    s.set_stamp(stamp.into());
    s.set_overlay(false);
    s.set_asking(false);
    s.set_tracks_open(false);
    s.set_hint(SharedString::new());
    s.set_title(SharedString::new());
    s.set_eyebrow(SharedString::new());
    s.set_time(0.0);
    s.set_length(0.0);
    s.set_buffered(0.0);
}

/// The OS UI language as ISO 639-1 (from the locale variables), `en` when unknown.
pub fn ui_language() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find_map(|v| {
            let code: String = v.chars().take_while(char::is_ascii_alphabetic).collect();
            (code.len() == 2).then(|| code.to_ascii_lowercase())
        })
        .unwrap_or_else(|| "en".to_owned())
}

// ---- the underlay ---------------------------------------------------------------------------

struct Active {
    // Dropped first: the Mpv handle (its event thread), then the renderer, which frees the
    // render context before the core can be destroyed.
    _mpv: Arc<Mpv>,
    renderer: GlRenderer,
}

#[derive(Default)]
struct UnderlayState {
    wanted: Option<Arc<Mpv>>,
    active: Option<Active>,
    active_for: Option<usize>,
    on_ready: Option<Box<dyn FnOnce()>>,
}

/// mpv drawn under the Slint scene through the window's rendering notifier.
pub struct Underlay {
    ui: slint::Weak<AppWindow>,
    state: RefCell<UnderlayState>,
}

fn mpv_id(mpv: &Arc<Mpv>) -> usize {
    Arc::as_ptr(mpv) as usize
}

/// mpv's update callback: coalesced redraw requests on the event loop.
fn redraw_request(ui: slint::Weak<AppWindow>) -> Box<dyn Fn() + Send + Sync> {
    let pending = Arc::new(AtomicBool::new(false));
    Box::new(move || {
        if pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let flag = pending.clone();
        let requested = ui.upgrade_in_event_loop(move |ui| {
            flag.store(false, Ordering::Release);
            ui.window().request_redraw();
        });
        if requested.is_err() {
            pending.store(false, Ordering::Release);
        }
    })
}

impl Underlay {
    /// Installs the rendering notifier. Call before the window is shown; a window has one
    /// notifier, so this happens once.
    pub fn install(ui: &AppWindow) -> anyhow::Result<Rc<Underlay>> {
        let underlay = Rc::new(Underlay {
            ui: ui.as_weak(),
            state: RefCell::new(UnderlayState::default()),
        });
        let weak = Rc::downgrade(&underlay);
        ui.window()
            .set_rendering_notifier(move |state, api| {
                let Some(u) = weak.upgrade() else {
                    return;
                };
                match state {
                    slint::RenderingState::BeforeRendering => {
                        if let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                            u.before_rendering(get_proc_address);
                        }
                    }
                    slint::RenderingState::AfterRendering => u.after_rendering(),
                    slint::RenderingState::RenderingTeardown => u.teardown(),
                    _ => {}
                }
            })
            .map_err(|e| anyhow::anyhow!("rendering notifier: {e:?}"))?;
        Ok(underlay)
    }

    /// Shows `mpv` under the scene from the next frame; `on_ready` runs once the renderer
    /// exists (or failed to start).
    pub fn attach(&self, mpv: Arc<Mpv>, on_ready: Box<dyn FnOnce()>) {
        {
            let mut s = self.state.borrow_mut();
            s.wanted = Some(mpv);
            s.on_ready = Some(on_ready);
        }
        self.redraw();
    }

    /// Stops drawing; the renderer is freed on the next frame.
    pub fn detach(&self) {
        {
            let mut s = self.state.borrow_mut();
            s.wanted = None;
            s.on_ready = None;
        }
        self.redraw();
    }

    fn redraw(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.window().request_redraw();
        }
    }

    fn before_rendering(
        &self,
        get_proc_address: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void,
    ) {
        let mut ready = None;
        {
            let mut s = self.state.borrow_mut();
            let wanted_id = s.wanted.as_ref().map(mpv_id);
            if s.active.is_some() && s.active_for != wanted_id {
                // The GL context is current inside the notifier.
                s.active = None;
                s.active_for = None;
            }
            if s.active.is_none() {
                if let Some(mpv) = s.wanted.clone() {
                    match GlRenderer::new(&mpv, get_proc_address, redraw_request(self.ui.clone())) {
                        Ok(renderer) => {
                            s.active_for = Some(mpv_id(&mpv));
                            s.active = Some(Active {
                                _mpv: mpv,
                                renderer,
                            });
                        }
                        Err(e) => {
                            tracing::warn!("mpv render context: {e}");
                            s.wanted = None;
                        }
                    }
                    ready = s.on_ready.take();
                }
            }
            if let (Some(active), Some(ui)) = (&s.active, self.ui.upgrade()) {
                let size = ui.window().size();
                let w = i32::try_from(size.width).unwrap_or(i32::MAX);
                let h = i32::try_from(size.height).unwrap_or(i32::MAX);
                if let Err(e) = active.renderer.render(0, w, h) {
                    tracing::debug!("mpv render: {e}");
                }
            }
        }
        if let Some(ready) = ready {
            // Outside the rendering pass: starting playback touches mpv and the UI.
            slint::Timer::single_shot(Duration::ZERO, ready);
        }
    }

    fn after_rendering(&self) {
        if let Some(active) = &self.state.borrow().active {
            active.renderer.report_swap();
        }
    }

    fn teardown(&self) {
        let mut s = self.state.borrow_mut();
        s.active = None;
        s.active_for = None;
    }
}

// ---- the view -------------------------------------------------------------------------------

/// The library as the player sees it: a snapshot taken when the player opens.
pub struct LibraryPrints {
    ready: bool,
    view: Arc<dyn LibraryView>,
}

impl LibraryPrints {
    pub fn new(ready: bool, view: Arc<dyn LibraryView>) -> Self {
        Self { ready, view }
    }
}

impl Prints for LibraryPrints {
    fn ready(&self) -> bool {
        self.ready
    }

    fn prints(&self, key: EpisodeKey) -> Vec<Entry> {
        self.view.entries(key)
    }
}

/// Everything a playback needs beyond the shell's services.
pub struct PlayerDeps {
    pub mpv_lib: Option<Arc<MpvLib>>,
    pub td: Option<TdAccess>,
    pub library: Option<Arc<Library>>,
    pub runtime: Option<tokio::runtime::Handle>,
    pub sniffer: Arc<dyn Sniffer>,
    /// `--dev-play <file>`: the sniff answers with this path, given straight to `loadfile`.
    pub dev_file: Option<PathBuf>,
    /// Absent in tests (the software renderer has no OpenGL).
    pub underlay: Option<Rc<Underlay>>,
}

impl PlayerDeps {
    /// No mpv, no Telegram, no WebView2: every playback fails (tests and fixtures).
    pub fn offline() -> Self {
        Self {
            mpv_lib: None,
            td: None,
            library: None,
            runtime: None,
            sniffer: flox_web::unavailable_sniffer(),
            dev_file: None,
            underlay: None,
        }
    }
}

type PlayerController = Controller<MpvEngine, SystemClock, LibraryPrints>;

/// One open player screen.
pub struct PlayerView {
    ui: slint::Weak<AppWindow>,
    services: Arc<Services>,
    exec: Exec,
    deps: Rc<PlayerDeps>,
    request: PlayRequest,
    prints: RefCell<Option<LibraryPrints>>,
    controller: RefCell<Option<PlayerController>>,
    mpv: RefCell<Option<Arc<Mpv>>>,
    mapper: RefCell<EventMapper>,
    nav: RefCell<OverlayNav>,
    repeat: RefCell<RepeatCounter>,
    pointer: RefCell<PointerGate>,
    resume_focus: Cell<bool>,
    overlay_was_visible: Cell<bool>,
    started: Cell<bool>,
    closed: Cell<bool>,
    timer: slint::Timer,
    sniff_cancel: RefCell<Option<CancellationToken>>,
    keep_awake: RefCell<Option<KeepAwake>>,
    media: RefCell<Option<MediaControls>>,
    media_state: RefCell<Option<(bool, String)>>,
    on_exit: Box<dyn Fn()>,
}

impl PlayerView {
    /// Opens the player for `request`. `on_exit` leaves the screen (the controller's `Exit`).
    pub fn open(
        ui: &AppWindow,
        services: Arc<Services>,
        exec: Exec,
        deps: Rc<PlayerDeps>,
        request: PlayRequest,
        prints: LibraryPrints,
        on_exit: Box<dyn Fn()>,
    ) -> Rc<PlayerView> {
        reset(ui, "LOADING");
        let view = Rc::new(PlayerView {
            ui: ui.as_weak(),
            services,
            exec,
            deps,
            request,
            prints: RefCell::new(Some(prints)),
            controller: RefCell::new(None),
            mpv: RefCell::new(None),
            mapper: RefCell::new(EventMapper::new()),
            nav: RefCell::new(OverlayNav::default()),
            repeat: RefCell::new(RepeatCounter::default()),
            pointer: RefCell::new(PointerGate::default()),
            resume_focus: Cell::new(true),
            overlay_was_visible: Cell::new(false),
            started: Cell::new(false),
            closed: Cell::new(false),
            timer: slint::Timer::default(),
            sniff_cancel: RefCell::new(None),
            keep_awake: RefCell::new(Some(KeepAwake::display())),
            media: RefCell::new(None),
            media_state: RefCell::new(None),
            on_exit,
        });
        view.attach_media_keys(ui);
        let weak = Rc::downgrade(&view);
        view.timer
            .start(slint::TimerMode::Repeated, REFRESH, move || {
                if let Some(view) = weak.upgrade() {
                    view.tick();
                }
            });
        view.resolve_meta();
        view
    }

    // -- setup ----------------------------------------------------------------------------

    fn attach_media_keys(self: &Rc<Self>, ui: &AppWindow) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<MediaKey>();
        let hwnd = window_handle(ui).unwrap_or(0);
        match MediaControls::attach(
            hwnd,
            Box::new(move |key| {
                let _ = tx.send(key);
            }),
        ) {
            Ok(controls) => *self.media.borrow_mut() = Some(controls),
            Err(e) => tracing::warn!("media keys: {e}"),
        }
        let weak = Rc::downgrade(self);
        if let Exec::Live(_) = self.exec {
            let listening = slint::spawn_local(async move {
                while let Some(key) = rx.recv().await {
                    let Some(view) = weak.upgrade() else {
                        break;
                    };
                    view.on_media_key(key);
                }
            });
            if let Err(e) = listening {
                tracing::warn!("media keys: {e}");
            }
        }
    }

    /// The title and poster from the catalog, then the controller.
    fn resolve_meta(self: &Rc<Self>) {
        let key = self.request.key;
        if let Some(file) = &self.deps.dev_file {
            let title = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.begin(Meta {
                id: key.tmdb,
                media: key.media,
                title,
                poster: None,
                season: key.season,
                episode: key.episode,
            });
            return;
        }
        let catalog = self.services.catalog.clone();
        let weak = Rc::downgrade(self);
        self.exec.run(
            async move { catalog.details(key.media, key.tmdb).await },
            move |result| {
                let Some(view) = weak.upgrade() else {
                    return;
                };
                let (title, poster) = match result {
                    Ok(d) => (d.summary.title, d.summary.poster_path),
                    Err(e) => {
                        tracing::warn!("player title: {e}");
                        (String::new(), None)
                    }
                };
                view.begin(Meta {
                    id: key.tmdb,
                    media: key.media,
                    title,
                    poster,
                    season: key.season,
                    episode: key.episode,
                });
            },
        );
    }

    /// Creates mpv and the controller, then starts once the underlay can draw.
    fn begin(self: &Rc<Self>, meta: Meta) {
        if self.closed.get() {
            return;
        }
        let Some(prints) = self.prints.borrow_mut().take() else {
            return;
        };
        let settings = self.services.settings.get();
        let streams: Streams = Arc::default();
        let mpv = self.deps.mpv_lib.clone().and_then(|lib| {
            match create_mpv(
                lib,
                &settings,
                &ui_language(),
                self.deps.td.clone(),
                streams.clone(),
            ) {
                Ok(mpv) => Some(mpv),
                Err(e) => {
                    tracing::warn!("mpv: {e:#}");
                    None
                }
            }
        });
        let engine = MpvEngine::new(mpv.clone(), settings.clone(), streams, self.page_surface());
        let config = PlayerConfig {
            meta,
            start_at: self.request.start_at.unwrap_or(0),
            settings,
            ui_language: ui_language(),
        };
        *self.controller.borrow_mut() =
            Some(Controller::new(config, engine, SystemClock::new(), prints));
        *self.mpv.borrow_mut() = mpv.clone();

        match (mpv, &self.deps.underlay) {
            (Some(mpv), Some(underlay)) => {
                self.listen(&mpv);
                let weak = Rc::downgrade(self);
                underlay.attach(
                    mpv,
                    Box::new(move || {
                        if let Some(view) = weak.upgrade() {
                            view.start();
                        }
                    }),
                );
                let weak = Rc::downgrade(self);
                slint::Timer::single_shot(UNDERLAY_GRACE, move || {
                    if let Some(view) = weak.upgrade() {
                        view.start();
                    }
                });
            }
            (Some(mpv), None) => {
                self.listen(&mpv);
                self.start();
            }
            (None, _) => self.start(),
        }
    }

    fn page_surface(&self) -> PageSurface {
        #[cfg(windows)]
        if let (Some(ui), Some(runtime)) = (self.ui.upgrade(), self.deps.runtime.clone()) {
            if let Some(hwnd) = window_handle(&ui) {
                let weak = self.ui.clone();
                return PageSurface::windows(
                    runtime,
                    hwnd,
                    Box::new(move || {
                        weak.upgrade().map_or((1280, 720), |ui| {
                            let size = ui.window().size();
                            (
                                i32::try_from(size.width).unwrap_or(1280),
                                i32::try_from(size.height).unwrap_or(720),
                            )
                        })
                    }),
                );
            }
        }
        PageSurface::unavailable()
    }

    /// Forwards mpv events to the controller on the UI thread.
    fn listen(self: &Rc<Self>, mpv: &Mpv) {
        let mut events = mpv.events();
        let weak = Rc::downgrade(self);
        let listening = slint::spawn_local(async move {
            while let Some(event) = events.recv().await {
                let Some(view) = weak.upgrade() else {
                    break;
                };
                view.on_mpv(event);
            }
        });
        if let Err(e) = listening {
            tracing::warn!("mpv events: {e}");
        }
    }

    fn start(self: &Rc<Self>) {
        if self.started.replace(true) || self.closed.get() {
            return;
        }
        let effects = match self.controller.borrow_mut().as_mut() {
            Some(c) => c.start(),
            None => return,
        };
        self.run_effects(effects);
        self.drain_engine();
        self.render();
    }

    // -- the controller loop --------------------------------------------------------------

    fn feed(self: &Rc<Self>, input: Input) {
        if self.closed.get() {
            return;
        }
        let effects = match self.controller.borrow_mut().as_mut() {
            Some(c) => c.handle(input),
            None => return,
        };
        self.run_effects(effects);
        self.drain_engine();
    }

    /// Inputs the engine queued (the page surface).
    fn drain_engine(self: &Rc<Self>) {
        loop {
            let inputs = match self.controller.borrow_mut().as_mut() {
                Some(c) => c.engine_mut().take_inputs(),
                None => return,
            };
            if inputs.is_empty() {
                return;
            }
            for input in inputs {
                self.feed(input);
            }
        }
    }

    fn on_mpv(self: &Rc<Self>, event: MpvEvent) {
        let loaded = event == MpvEvent::FileLoaded;
        let inputs = self.mapper.borrow_mut().map(event);
        if loaded {
            if let Some(c) = self.controller.borrow_mut().as_mut() {
                c.engine_mut().on_file_loaded();
            }
        }
        for input in inputs {
            self.feed(input);
        }
    }

    fn tick(self: &Rc<Self>) {
        self.feed(Input::Tick);
        self.render();
    }

    fn run_effects(self: &Rc<Self>, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Exit => (self.on_exit)(),
                Effect::SaveProgress(record) => {
                    if self.deps.dev_file.is_some() {
                        continue;
                    }
                    let progress = self.services.progress.clone();
                    self.exec.run(async move { progress.put(record) }, |saved| {
                        if let Err(e) = saved {
                            tracing::warn!("saving progress: {e}");
                        }
                    });
                }
                Effect::SaveQuality(quality) => {
                    if let Err(e) = self.services.settings.update(|s| s.quality = Some(quality)) {
                        tracing::warn!("saving quality: {e}");
                    }
                }
                Effect::SaveAudioLanguage(code) => {
                    if let Err(e) = self
                        .services
                        .settings
                        .update(|s| s.audio_language = Some(code))
                    {
                        tracing::warn!("saving audio language: {e}");
                    }
                }
                Effect::Sniff { seq, page_url } => self.sniff(seq, page_url),
                Effect::PrepareLibrary { seq, entry } => self.prepare(seq, entry),
                Effect::FetchEpisodeCount { tmdb, season } => {
                    let catalog = self.services.catalog.clone();
                    let weak = Rc::downgrade(self);
                    self.exec.run(
                        async move { catalog.season(tmdb, season).await },
                        move |result| {
                            let count = match result {
                                Ok(episodes) => u32::try_from(episodes.len()).unwrap_or(0),
                                Err(e) => {
                                    tracing::warn!("episode count: {e}");
                                    0
                                }
                            };
                            if let Some(view) = weak.upgrade() {
                                view.feed(Input::EpisodeCount(count));
                            }
                        },
                    );
                }
                Effect::PassToOverlay { key, repeat } => self.overlay_key(key, repeat),
            }
        }
    }

    fn sniff(self: &Rc<Self>, seq: u64, page_url: String) {
        if let Some(file) = &self.deps.dev_file {
            let result = SniffResult {
                url: file.to_string_lossy().into_owned(),
                kind: StreamKind::File,
                headers: Vec::new(),
                captions: Vec::new(),
            };
            self.feed(Input::Sniffed { seq, result });
            return;
        }
        let cancel = CancellationToken::new();
        if let Some(previous) = self.sniff_cancel.replace(Some(cancel.clone())) {
            previous.cancel();
        }
        let sniffer = self.deps.sniffer.clone();
        let weak = Rc::downgrade(self);
        self.exec.run(
            async move { sniffer.sniff(&page_url, SniffMode::Playback, cancel).await },
            move |result| {
                let Some(view) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(result) => view.feed(Input::Sniffed { seq, result }),
                    Err(e) => {
                        tracing::warn!("sniff: {e}");
                        view.feed(Input::SniffFailed { seq });
                    }
                }
            },
        );
    }

    fn prepare(self: &Rc<Self>, seq: u64, entry: Entry) {
        let library = match (&entry.subtitle, &self.deps.library) {
            (Some(_), Some(library)) => library.clone(),
            _ => {
                self.feed(Input::LibraryPrepared {
                    seq,
                    subtitle: None,
                });
                return;
            }
        };
        let weak = Rc::downgrade(self);
        self.exec.run(
            async move { library.download_subtitle(&entry, SUBTITLE_TIMEOUT).await },
            move |result| {
                let subtitle = result.map_err(|e| tracing::warn!("subtitle: {e}")).ok();
                if let Some(view) = weak.upgrade() {
                    view.feed(Input::LibraryPrepared { seq, subtitle });
                }
            },
        );
    }

    // -- input ----------------------------------------------------------------------------

    fn view_state(&self) -> Option<ViewState> {
        self.controller.borrow().as_ref().map(Controller::view)
    }

    fn overlay_key(self: &Rc<Self>, key: Key, repeat: u32) {
        let Some(view) = self.view_state() else {
            return;
        };
        let input = self.nav.borrow_mut().key(key, repeat, &view);
        if let Some(input) = input {
            self.feed(input);
        }
    }

    /// A key press on the player screen. True when handled.
    pub fn key(self: &Rc<Self>, text: &str, modifiers: Modifiers, repeat: bool) -> bool {
        let Some(action) = player_key(text, modifiers) else {
            return false;
        };
        let count = self.repeat.borrow_mut().press(text, repeat);
        let key = match action {
            PlayerKey::Fullscreen => {
                if count == 0 {
                    if let Some(ui) = self.ui.upgrade() {
                        let window = ui.window();
                        window.set_fullscreen(!window.is_fullscreen());
                    }
                }
                return true;
            }
            PlayerKey::Swallow => return true,
            PlayerKey::Key(key) => key,
        };
        let asking = self.view_state().is_some_and(|v| v.phase == Phase::Asking);
        if asking && key != Key::Back {
            match key {
                Key::Left | Key::Right => self.resume_focus.set(!self.resume_focus.get()),
                Key::Center if count == 0 => self.feed(Input::Resume(self.resume_focus.get())),
                _ => {}
            }
        } else {
            self.feed(Input::Key { key, repeat: count });
        }
        self.render();
        true
    }

    /// A key release. True when handled.
    pub fn key_released(self: &Rc<Self>, text: &str) -> bool {
        self.repeat.borrow_mut().release();
        let Some(key) = released_key(text) else {
            return false;
        };
        self.feed(Input::KeyUp(key));
        self.render();
        true
    }

    fn on_media_key(self: &Rc<Self>, key: MediaKey) {
        if let Some(key) = media_key(key) {
            self.feed(Input::Key { key, repeat: 0 });
            self.render();
        }
    }

    /// The pointer moved over an overlay control.
    pub fn hover(self: &Rc<Self>, zone: ZoneId, index: usize) {
        if let Some(target) = pointer_target(zone, index) {
            self.nav.borrow_mut().set(target);
        } else if zone == RESUME {
            self.resume_focus.set(index == 0);
        }
        self.feed(Input::MouseMove);
        self.render();
    }

    /// A click on an overlay control.
    pub fn click(self: &Rc<Self>, zone: ZoneId, index: usize) {
        if let Some(target) = pointer_target(zone, index) {
            self.nav.borrow_mut().set(target);
        }
        match zone {
            BACK => self.feed(Input::Button(Button::Back)),
            BUTTONS => {
                if let Some(button) = SLOTS.get(index) {
                    self.feed(Input::Button(*button));
                }
            }
            TRACKS => self.feed(Input::PickTrack(index)),
            RESUME => self.feed(Input::Resume(index == 0)),
            _ => {}
        }
        self.render();
    }

    /// The pointer moved over the video.
    pub fn pointer_moved(self: &Rc<Self>, x: f32, y: f32) {
        if self.pointer.borrow_mut().moved(x, y) {
            self.feed(Input::MouseMove);
            self.render();
        }
    }

    pub fn video_clicked(self: &Rc<Self>) {
        self.feed(Input::VideoClick);
        self.render();
    }

    /// A right click: MENU pressed and released.
    pub fn menu_clicked(self: &Rc<Self>) {
        self.feed(Input::Key {
            key: Key::Menu,
            repeat: 0,
        });
        self.feed(Input::KeyUp(Key::Menu));
        self.render();
    }

    /// The seek bar was clicked at `fraction` of its length.
    pub fn seek(self: &Rc<Self>, fraction: f32) {
        if let Some(view) = self.view_state() {
            self.feed(Input::ScrubTo(f64::from(fraction) * view.length));
            self.render();
        }
    }

    // -- output ---------------------------------------------------------------------------

    /// Writes the controller's view to the screen, the focus ring and the OS media flyout.
    pub fn render(&self) {
        if self.closed.get() {
            return;
        }
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let Some(view) = self.view_state() else {
            return;
        };
        if view.overlay_visible && !self.overlay_was_visible.get() {
            self.nav.borrow_mut().reset();
        }
        self.overlay_was_visible.set(view.overlay_visible);
        let buffered = self.mapper.borrow().buffered();
        apply(&ui, &view, buffered, self.deps.mpv_lib.is_none());

        let (zone, index) = if view.phase == Phase::Asking {
            (RESUME.0, if self.resume_focus.get() { 0 } else { 1 })
        } else if view.overlay_visible {
            let mut nav = self.nav.borrow_mut();
            nav.normalize(&view);
            nav.slint_focus()
        } else {
            (-1, -1)
        };
        let focus = ui.global::<FocusState>();
        focus.set_zone(zone);
        focus.set_index(index);
        focus.set_editing(false);

        let playing = view.phase == Phase::Native && !view.paused;
        let state = (playing, view.title.clone());
        if self.media_state.borrow().as_ref() != Some(&state) {
            if let Some(media) = self.media.borrow().as_ref() {
                media.set_playing(playing, &view.title);
            }
            *self.media_state.borrow_mut() = Some(state);
        }
    }

    /// Stops playback and releases the window, the media keys and keep-awake.
    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.timer.stop();
        if let Some(cancel) = self.sniff_cancel.borrow_mut().take() {
            cancel.cancel();
        }
        let controller = self.controller.borrow_mut().take();
        if let Some(mut controller) = controller {
            controller.engine_mut().stop();
            controller.engine_mut().page_close();
        }
        if let Some(underlay) = &self.deps.underlay {
            underlay.detach();
        }
        self.mpv.borrow_mut().take();
        self.media.borrow_mut().take();
        self.keep_awake.borrow_mut().take();
        if let Some(ui) = self.ui.upgrade() {
            reset(&ui, "");
        }
    }

    /// The controller's phase, once it exists.
    pub fn phase(&self) -> Option<Phase> {
        self.controller.borrow().as_ref().map(Controller::phase)
    }
}

impl Drop for PlayerView {
    fn drop(&mut self) {
        self.close();
    }
}

/// The window's HWND on Windows.
#[cfg(windows)]
pub fn window_handle(ui: &AppWindow) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = ui.window().window_handle();
    let raw = handle.window_handle().ok()?.as_raw();
    match raw {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// No native handle is needed off Windows (media keys and the page player are no-ops).
#[cfg(not(windows))]
pub fn window_handle(_ui: &AppWindow) -> Option<isize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::controller::TracksPanel;

    fn view() -> ViewState {
        ViewState {
            phase: Phase::Native,
            resume_label: None,
            overlay_visible: true,
            tracks: None,
            hint: None,
            eyebrow: "S1 · E3".to_owned(),
            title: "Title".to_owned(),
            position: "00:10".to_owned(),
            duration: "42:00".to_owned(),
            time: 10.0,
            length: 2520.0,
            paused: false,
            volume: 100,
            nav_mode: false,
            audio_available: false,
            subtitles_available: false,
            quality_available: false,
            next_available: false,
        }
    }

    fn mods() -> Modifiers {
        Modifiers::default()
    }

    fn text(key: slint::platform::Key) -> String {
        char::from(key).to_string()
    }

    #[test]
    fn keys_follow_the_global_table() {
        use slint::platform::Key as K;
        assert_eq!(
            player_key(&text(K::Return), mods()),
            Some(PlayerKey::Key(Key::Center))
        );
        assert_eq!(
            player_key(&text(K::Escape), mods()),
            Some(PlayerKey::Key(Key::Back))
        );
        assert_eq!(
            player_key(&text(K::Backspace), mods()),
            Some(PlayerKey::Key(Key::Back))
        );
        assert_eq!(
            player_key(&text(K::LeftArrow), mods()),
            Some(PlayerKey::Key(Key::Left))
        );
        assert_eq!(
            player_key(&text(K::UpArrow), mods()),
            Some(PlayerKey::Key(Key::Up))
        );
        assert_eq!(
            player_key(" ", mods()),
            Some(PlayerKey::Key(Key::PlayPause))
        );
        assert_eq!(player_key("m", mods()), Some(PlayerKey::Key(Key::Menu)));
        assert_eq!(
            player_key(&text(K::F11), mods()),
            Some(PlayerKey::Fullscreen)
        );
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(player_key("r", ctrl), Some(PlayerKey::Key(Key::Reload)));
        assert_eq!(player_key("f", ctrl), Some(PlayerKey::Swallow));
        assert_eq!(player_key("x", mods()), None);

        assert_eq!(released_key(&text(K::Return)), Some(Key::Center));
        assert_eq!(released_key("M"), Some(Key::Menu));
        assert_eq!(released_key(&text(K::Menu)), Some(Key::Menu));
        assert_eq!(released_key(" "), None);
    }

    #[test]
    fn media_keys_keep_play_and_pause_apart() {
        assert_eq!(media_key(MediaKey::Play), Some(Key::Play));
        assert_eq!(media_key(MediaKey::Pause), Some(Key::Pause));
        assert_eq!(media_key(MediaKey::PlayPause), Some(Key::PlayPause));
        assert_eq!(media_key(MediaKey::Next), Some(Key::Next));
        assert_eq!(media_key(MediaKey::Previous), None);
    }

    #[test]
    fn repeats_count_while_the_same_key_is_held() {
        let mut r = RepeatCounter::default();
        assert_eq!(r.press("a", false), 0);
        assert_eq!(r.press("a", true), 1);
        assert_eq!(r.press("a", true), 2);
        assert_eq!(r.press("b", true), 0, "another key starts over");
        r.release();
        assert_eq!(r.press("b", true), 0);
        assert_eq!(r.press("b", false), 0);
    }

    #[test]
    fn extras_show_only_when_available() {
        let mut v = view();
        assert_eq!(
            visible_buttons(&v),
            vec![
                Button::Rewind,
                Button::PlayPause,
                Button::Forward,
                Button::VolumeDown,
                Button::VolumeUp
            ]
        );
        v.subtitles_available = true;
        v.next_available = true;
        assert_eq!(visible_buttons(&v)[5..], [Button::Subtitles, Button::Next]);
        assert_eq!(button_slot(Button::Next), Some(8));
        assert_eq!(button_slot(Button::Back), None);
    }

    #[test]
    fn overlay_navigation() {
        let mut v = view();
        v.audio_available = true;
        let mut nav = OverlayNav::default();
        assert_eq!(nav.slint_focus(), (BUTTONS.0, 1));
        assert_eq!(nav.key(Key::Right, 0, &v), None);
        assert_eq!(nav.target(), Target::Button(Button::Forward));
        for _ in 0..10 {
            nav.key(Key::Right, 0, &v);
        }
        assert_eq!(
            nav.target(),
            Target::Button(Button::Audio),
            "clamped at the end"
        );
        assert_eq!(
            nav.key(Key::Center, 0, &v),
            Some(Input::Button(Button::Audio))
        );
        assert_eq!(
            nav.key(Key::Center, 3, &v),
            None,
            "held CENTER does not repeat"
        );
        nav.key(Key::Up, 0, &v);
        assert_eq!(nav.target(), Target::Seek);
        assert_eq!(
            nav.key(Key::Left, 5, &v),
            Some(Input::ScrubBy {
                forward: false,
                repeat: 5
            })
        );
        assert_eq!(
            nav.key(Key::Center, 0, &v),
            Some(Input::Button(Button::PlayPause))
        );
        nav.key(Key::Up, 0, &v);
        assert_eq!(nav.target(), Target::Back);
        assert_eq!(nav.slint_focus(), (BACK.0, 0));
        assert_eq!(
            nav.key(Key::Center, 0, &v),
            Some(Input::Button(Button::Back))
        );
        nav.key(Key::Down, 0, &v);
        nav.key(Key::Down, 0, &v);
        assert_eq!(nav.target(), Target::Button(Button::PlayPause));
    }

    #[test]
    fn hidden_buttons_lose_the_focus() {
        let mut v = view();
        v.next_available = true;
        let mut nav = OverlayNav::default();
        nav.set(Target::Button(Button::Next));
        v.next_available = false;
        nav.normalize(&v);
        assert_eq!(nav.target(), Target::Button(Button::PlayPause));
    }

    #[test]
    fn the_tracks_panel_traps_the_focus() {
        let mut v = view();
        v.audio_available = true;
        v.tracks = Some(TracksPanel {
            heading: "AUDIO".to_owned(),
            labels: vec![
                "English".to_owned(),
                "Japanese".to_owned(),
                "French".to_owned(),
            ],
            selected: 1,
        });
        let mut nav = OverlayNav::default();
        assert_eq!(nav.key(Key::Down, 0, &v), None);
        assert_eq!(nav.target(), Target::Track(2));
        nav.key(Key::Down, 0, &v);
        assert_eq!(nav.target(), Target::Track(2));
        nav.key(Key::Left, 0, &v);
        assert_eq!(nav.target(), Target::Track(2));
        nav.key(Key::Up, 0, &v);
        assert_eq!(nav.key(Key::Center, 0, &v), Some(Input::PickTrack(1)));
        assert_eq!(nav.slint_focus(), (TRACKS.0, 1));
        v.tracks = None;
        nav.normalize(&v);
        assert_eq!(nav.target(), Target::Button(Button::Audio));
    }

    #[test]
    fn pointer_targets() {
        assert_eq!(pointer_target(BACK, 0), Some(Target::Back));
        assert_eq!(pointer_target(SEEK, 0), Some(Target::Seek));
        assert_eq!(
            pointer_target(BUTTONS, 6),
            Some(Target::Button(Button::Subtitles))
        );
        assert_eq!(pointer_target(BUTTONS, 9), None);
        assert_eq!(pointer_target(TRACKS, 2), Some(Target::Track(2)));
        assert_eq!(pointer_target(RESUME, 0), None);
    }

    #[test]
    fn stamps_and_hints() {
        assert_eq!(stamp_text(Phase::Loading), "LOADING");
        assert_eq!(stamp_text(Phase::Failed), "PLAYBACK FAILED");
        assert_eq!(stamp_text(Phase::Native), "");
        assert_eq!(stamp_text(Phase::Asking), "");
        let mut v = view();
        v.hint = Some("+10 S".to_owned());
        assert_eq!(hint_text(&v, true), "+10 S");
        v.phase = Phase::Failed;
        assert_eq!(hint_text(&v, true), "LIBMPV NOT FOUND");
        assert_eq!(hint_text(&v, false), "+10 S");
    }

    #[test]
    fn ui_language_is_two_letters() {
        let lang = ui_language();
        assert_eq!(lang.len(), 2);
        assert!(lang.chars().all(|c| c.is_ascii_lowercase()));
    }
}
