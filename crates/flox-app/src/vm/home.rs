//! Home rows (continue watching, library, trending) and the top bar.
//!
//! Pure logic: what each row shows, which rows exist, and where an activated card
//! goes. The shell (`app.rs`) loads the data and writes it to `HomeState`.

use flox_core::model::{EpisodeKey, MediaType, TitleDetails, TitleSummary};
use flox_core::progress::ProgressRecord;
use flox_core::settings::{LibrarySort, Settings};
use flox_td::auth::AuthState;

use crate::app::LibraryView;
use crate::focus::{Zone, ZoneId};
use crate::router::{PlayRequest, Route};

// Mirrors `HomeZones` in ui/screens/home.slint.
pub const TOP_BAR: ZoneId = ZoneId(0);
pub const CONTINUE: ZoneId = ZoneId(1);
pub const LIBRARY: ZoneId = ZoneId(2);
pub const MOVIES: ZoneId = ZoneId(3);
pub const TV: ZoneId = ZoneId(4);

/// The top bar, left to right: SEARCH · QUEUE (n) · LIBRARY · SETTINGS.
pub const TOP_BAR_ROUTES: [Route; 4] =
    [Route::Search, Route::Queue, Route::Library, Route::Settings];

pub const LOADING: &str = "LOADING";
pub const NO_RESULTS: &str = "NO RESULTS";
pub const REQUEST_FAILED: &str = "REQUEST FAILED";
pub const NOTHING_UPLOADED: &str = "NOTHING UPLOADED YET";
pub const SET_UP_TELEGRAM: &str = "SET UP TELEGRAM";
pub const CONNECT_TELEGRAM: &str = "CONNECT TELEGRAM";
pub const TDLIB_NOT_FOUND: &str = "TDLIB NOT FOUND";

/// A poster card and where activating it goes.
#[derive(Clone, Debug, PartialEq)]
pub struct CardData {
    pub title: String,
    pub meta: String,
    /// A TMDB image path such as `/abc.jpg`.
    pub poster: Option<String>,
    /// 0..1 draws the progress line; negative hides it.
    pub progress: f32,
    pub target: Route,
}

/// `2024 · MOVIE`, or just the type when the year is unknown.
pub fn title_meta(year: Option<u16>, media: MediaType) -> String {
    let kind = type_label(media);
    match year {
        Some(y) => format!("{y} · {kind}"),
        None => kind.to_owned(),
    }
}

pub fn type_label(media: MediaType) -> &'static str {
    match media {
        MediaType::Movie => "MOVIE",
        MediaType::Tv => "TV",
    }
}

/// A catalog card that opens Details.
pub fn media_card(t: &TitleSummary, library_only: bool) -> CardData {
    CardData {
        title: t.title.clone(),
        meta: title_meta(t.year, t.media),
        poster: t.poster_path.clone(),
        progress: -1.0,
        target: Route::Details {
            id: t.id,
            media: t.media,
            library_only,
        },
    }
}

/// `TV · S2 E3` or `MOVIE`.
pub fn continue_meta(r: &ProgressRecord) -> String {
    match r.media {
        MediaType::Tv => format!("TV · S{} E{}", r.season, r.episode),
        MediaType::Movie => "MOVIE".to_owned(),
    }
}

/// How far into the title the record is, 0..1 (0 when the duration is unknown).
pub fn continue_fraction(r: &ProgressRecord) -> f32 {
    if r.duration == 0 {
        return 0.0;
    }
    (r.watched as f32 / r.duration as f32).clamp(0.0, 1.0)
}

/// The player request that resumes a record where it stopped.
pub fn resume_request(r: &ProgressRecord) -> PlayRequest {
    let key = match r.media {
        MediaType::Movie => EpisodeKey::movie(r.id),
        MediaType::Tv => EpisodeKey::episode(r.id, r.season, r.episode),
    };
    PlayRequest {
        key,
        start_at: (r.watched > 0).then_some(r.watched),
        library_only: false,
    }
}

/// A continue-watching card: it goes straight to the player.
pub fn continue_card(r: &ProgressRecord) -> CardData {
    CardData {
        title: r.title.clone(),
        meta: continue_meta(r),
        poster: r.poster.clone(),
        progress: continue_fraction(r),
        target: Route::Player(resume_request(r)),
    }
}

/// CONTINUE WATCHING is hidden when there is nothing to continue.
pub fn continue_visible(cards: &[CardData]) -> bool {
    !cards.is_empty()
}

/// Where Telegram stands, as far as Home cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TelegramStatus {
    /// The API id or hash is missing.
    NotConfigured,
    /// Credentials are set but tdjson could not be found or started.
    Unavailable,
    Auth(AuthState),
}

impl TelegramStatus {
    pub fn ready(&self) -> bool {
        matches!(self, TelegramStatus::Auth(AuthState::Ready { .. }))
    }
}

/// The library row's data, loaded once Telegram is ready.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryLoad {
    /// Not requested yet, or invalidated (sort or channel changed, signed out).
    Idle,
    Loading,
    Loaded(Vec<CardData>),
    Failed,
}

/// What the LIBRARY row shows.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryRow {
    /// Credentials are missing: a focusable `SET UP TELEGRAM` that opens Settings.
    SetUp,
    /// tdjson is missing: a plain stamp.
    Unavailable,
    /// Not signed in: a focusable `CONNECT TELEGRAM` that opens Login.
    Connect,
    Loading,
    Empty,
    Failed,
    Items(Vec<CardData>),
}

impl LibraryRow {
    /// The focusable prompt, if the row shows one.
    pub fn prompt(&self) -> Option<&'static str> {
        match self {
            LibraryRow::SetUp => Some(SET_UP_TELEGRAM),
            LibraryRow::Connect => Some(CONNECT_TELEGRAM),
            _ => None,
        }
    }

    /// Where the prompt leads.
    pub fn prompt_route(&self) -> Option<Route> {
        match self {
            LibraryRow::SetUp => Some(Route::Settings),
            LibraryRow::Connect => Some(Route::Login),
            _ => None,
        }
    }

    /// The centred state stamp, if the row shows one.
    pub fn stamp(&self) -> Option<&'static str> {
        match self {
            LibraryRow::Unavailable => Some(TDLIB_NOT_FOUND),
            LibraryRow::Loading => Some(LOADING),
            LibraryRow::Empty => Some(NOTHING_UPLOADED),
            LibraryRow::Failed => Some(REQUEST_FAILED),
            _ => None,
        }
    }

    pub fn cards(&self) -> &[CardData] {
        match self {
            LibraryRow::Items(cards) => cards,
            _ => &[],
        }
    }

    /// Focusable items in the row's zone: the prompt counts as one.
    pub fn focusable_len(&self) -> usize {
        match self {
            LibraryRow::Items(cards) => cards.len(),
            LibraryRow::SetUp | LibraryRow::Connect => 1,
            _ => 0,
        }
    }
}

/// Decides the LIBRARY row from the Telegram status and the load.
pub fn library_row(status: &TelegramStatus, load: &LibraryLoad) -> LibraryRow {
    match status {
        TelegramStatus::NotConfigured => LibraryRow::SetUp,
        TelegramStatus::Unavailable => LibraryRow::Unavailable,
        TelegramStatus::Auth(AuthState::Idle | AuthState::Connecting) => LibraryRow::Loading,
        TelegramStatus::Auth(AuthState::Ready { .. }) => match load {
            LibraryLoad::Idle | LibraryLoad::Loading => LibraryRow::Loading,
            LibraryLoad::Failed => LibraryRow::Failed,
            LibraryLoad::Loaded(cards) if cards.is_empty() => LibraryRow::Empty,
            LibraryLoad::Loaded(cards) => LibraryRow::Items(cards.clone()),
        },
        TelegramStatus::Auth(_) => LibraryRow::Connect,
    }
}

/// True when the library should be (re)loaded now.
pub fn should_load_library(status: &TelegramStatus, load: &LibraryLoad) -> bool {
    status.ready() && *load == LibraryLoad::Idle
}

/// The library reloads when the sort order or the channel changes.
pub fn library_settings_changed(old: &Settings, new: &Settings) -> bool {
    old.library_sort != new.library_sort || old.telegram_channel != new.telegram_channel
}

/// A library title with what the sort orders need.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryTitle {
    pub summary: TitleSummary,
    /// The newest message id over every print (date added).
    pub newest: i64,
    /// Bytes over every print.
    pub size: u64,
}

/// Every key a title can have prints under: the movie, or each episode of each
/// season TMDB knows about.
pub fn library_keys(d: &TitleDetails) -> Vec<EpisodeKey> {
    match d.summary.media {
        MediaType::Movie => vec![EpisodeKey::movie(d.summary.id)],
        MediaType::Tv => d
            .seasons
            .iter()
            .flat_map(|s| {
                (1..=s.episode_count).map(move |e| EpisodeKey::episode(d.summary.id, s.number, e))
            })
            .collect(),
    }
}

/// Totals a resolved title's prints.
pub fn library_title(d: TitleDetails, lib: &dyn LibraryView) -> LibraryTitle {
    let (newest, size) = library_keys(&d)
        .into_iter()
        .flat_map(|k| lib.prints(k))
        .fold((i64::MIN, 0u64), |(newest, size), p| {
            (newest.max(p.newest_message_id), size.saturating_add(p.size))
        });
    LibraryTitle {
        summary: d.summary,
        newest: if newest == i64::MIN { 0 } else { newest },
        size,
    }
}

/// Orders the row by the `library_sort` setting: TITLE (case-insensitive A to Z),
/// DATE ADDED (newest first) or SIZE (largest first). Ties keep their order.
pub fn sort_library(mut titles: Vec<LibraryTitle>, sort: LibrarySort) -> Vec<TitleSummary> {
    match sort {
        LibrarySort::Title => {
            titles.sort_by_key(|t| t.summary.title.to_lowercase());
        }
        LibrarySort::DateAdded => titles.sort_by_key(|t| std::cmp::Reverse(t.newest)),
        LibrarySort::Size => titles.sort_by_key(|t| std::cmp::Reverse(t.size)),
    }
    titles.into_iter().map(|t| t.summary).collect()
}

/// A trending row's load.
#[derive(Clone, Debug, PartialEq)]
pub enum RowLoad {
    Loading,
    Loaded(Vec<CardData>),
    Failed,
}

impl RowLoad {
    pub fn stamp(&self) -> Option<&'static str> {
        match self {
            RowLoad::Loading => Some(LOADING),
            RowLoad::Loaded(cards) if cards.is_empty() => Some(NO_RESULTS),
            RowLoad::Loaded(_) => None,
            RowLoad::Failed => Some(REQUEST_FAILED),
        }
    }

    pub fn cards(&self) -> &[CardData] {
        match self {
            RowLoad::Loaded(cards) => cards,
            _ => &[],
        }
    }
}

/// The Home focus zones, top to bottom. Empty zones are skipped by the graph.
pub fn zones(continue_len: usize, library_len: usize, movies: usize, tv: usize) -> Vec<Zone> {
    vec![
        Zone::top_bar(TOP_BAR, TOP_BAR_ROUTES.len()),
        Zone::row(CONTINUE, continue_len),
        Zone::row(LIBRARY, library_len),
        Zone::row(MOVIES, movies),
        Zone::row(TV, tv),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{EmptyLibrary, Print};
    use flox_core::model::SeasonInfo;
    use std::collections::HashMap;

    fn summary(id: u64, media: MediaType, title: &str) -> TitleSummary {
        TitleSummary {
            id,
            media,
            title: title.to_owned(),
            year: Some(2020),
            poster_path: Some(format!("/{id}.jpg")),
            overview: String::new(),
            popularity: 1.0,
        }
    }

    fn record(media: MediaType, watched: u32, duration: u32) -> ProgressRecord {
        ProgressRecord {
            id: 42,
            media,
            title: "Severance".to_owned(),
            poster: Some("/sev.jpg".to_owned()),
            watched,
            duration,
            season: 2,
            episode: 3,
            updated: 1,
        }
    }

    #[derive(Default)]
    struct FakeLibrary(HashMap<EpisodeKey, Vec<Print>>);

    impl LibraryView for FakeLibrary {
        fn titles(&self) -> Vec<(u64, MediaType)> {
            Vec::new()
        }
        fn prints(&self, key: EpisodeKey) -> Vec<Print> {
            self.0.get(&key).cloned().unwrap_or_default()
        }
    }

    fn print(quality: &str, size: u64, newest: i64) -> Print {
        Print {
            quality: quality.to_owned(),
            size,
            newest_message_id: newest,
        }
    }

    #[test]
    fn media_card_meta_and_target() {
        let card = media_card(&summary(7, MediaType::Tv, "Dark"), true);
        assert_eq!(card.meta, "2020 · TV");
        assert_eq!(card.progress, -1.0);
        assert_eq!(
            card.target,
            Route::Details {
                id: 7,
                media: MediaType::Tv,
                library_only: true
            }
        );
        let mut s = summary(8, MediaType::Movie, "Heat");
        s.year = None;
        assert_eq!(media_card(&s, false).meta, "MOVIE");
    }

    #[test]
    fn continue_card_resumes_in_the_player() {
        let card = continue_card(&record(MediaType::Tv, 600, 2400));
        assert_eq!(card.meta, "TV · S2 E3");
        assert!((card.progress - 0.25).abs() < 1e-6);
        assert_eq!(
            card.target,
            Route::Player(PlayRequest {
                key: EpisodeKey::episode(42, 2, 3),
                start_at: Some(600),
                library_only: false
            })
        );
        let movie = continue_card(&record(MediaType::Movie, 0, 0));
        assert_eq!(movie.meta, "MOVIE");
        assert_eq!(movie.progress, 0.0);
        assert_eq!(
            movie.target,
            Route::Player(PlayRequest {
                key: EpisodeKey::movie(42),
                start_at: None,
                library_only: false
            })
        );
    }

    #[test]
    fn continue_row_hidden_when_empty() {
        assert!(!continue_visible(&[]));
        assert!(continue_visible(&[continue_card(&record(
            MediaType::Movie,
            10,
            100
        ))]));
    }

    #[test]
    fn library_row_states() {
        let ready = TelegramStatus::Auth(AuthState::Ready {
            user: "me".to_owned(),
        });
        assert_eq!(
            library_row(&TelegramStatus::NotConfigured, &LibraryLoad::Idle),
            LibraryRow::SetUp
        );
        assert_eq!(
            library_row(&TelegramStatus::Unavailable, &LibraryLoad::Idle),
            LibraryRow::Unavailable
        );
        assert_eq!(
            library_row(
                &TelegramStatus::Auth(AuthState::Connecting),
                &LibraryLoad::Idle
            ),
            LibraryRow::Loading
        );
        for waiting in [
            AuthState::WaitPhone,
            AuthState::WaitQr {
                link: "tg://login?token=x".to_owned(),
            },
            AuthState::WaitCode,
            AuthState::Failed("x".to_owned()),
            AuthState::LoggingOut,
        ] {
            assert_eq!(
                library_row(&TelegramStatus::Auth(waiting), &LibraryLoad::Idle),
                LibraryRow::Connect
            );
        }
        assert_eq!(library_row(&ready, &LibraryLoad::Idle), LibraryRow::Loading);
        assert_eq!(
            library_row(&ready, &LibraryLoad::Loading),
            LibraryRow::Loading
        );
        assert_eq!(
            library_row(&ready, &LibraryLoad::Failed),
            LibraryRow::Failed
        );
        assert_eq!(
            library_row(&ready, &LibraryLoad::Loaded(Vec::new())),
            LibraryRow::Empty
        );
        let cards = vec![media_card(&summary(1, MediaType::Movie, "A"), true)];
        assert_eq!(
            library_row(&ready, &LibraryLoad::Loaded(cards.clone())),
            LibraryRow::Items(cards)
        );
    }

    #[test]
    fn library_row_labels_and_routes() {
        assert_eq!(LibraryRow::SetUp.prompt(), Some("SET UP TELEGRAM"));
        assert_eq!(LibraryRow::SetUp.prompt_route(), Some(Route::Settings));
        assert_eq!(LibraryRow::Connect.prompt(), Some("CONNECT TELEGRAM"));
        assert_eq!(LibraryRow::Connect.prompt_route(), Some(Route::Login));
        assert_eq!(LibraryRow::Connect.focusable_len(), 1);
        assert_eq!(LibraryRow::Loading.stamp(), Some("LOADING"));
        assert_eq!(LibraryRow::Loading.focusable_len(), 0);
        assert_eq!(LibraryRow::Empty.stamp(), Some("NOTHING UPLOADED YET"));
        assert_eq!(LibraryRow::Failed.stamp(), Some("REQUEST FAILED"));
        assert_eq!(LibraryRow::Items(Vec::new()).stamp(), None);
    }

    #[test]
    fn library_loads_once_when_ready() {
        let ready = TelegramStatus::Auth(AuthState::Ready {
            user: "me".to_owned(),
        });
        assert!(should_load_library(&ready, &LibraryLoad::Idle));
        assert!(!should_load_library(&ready, &LibraryLoad::Loading));
        assert!(!should_load_library(&ready, &LibraryLoad::Failed));
        assert!(!should_load_library(
            &TelegramStatus::Auth(AuthState::WaitQr {
                link: String::new()
            }),
            &LibraryLoad::Idle
        ));
    }

    #[test]
    fn library_reloads_on_sort_or_channel_only() {
        let old = Settings::default();
        let mut sort = old.clone();
        sort.library_sort = LibrarySort::Size;
        let mut channel = old.clone();
        channel.telegram_channel = "Other".to_owned();
        let mut speed = old.clone();
        speed.playback_speed = 2.0;
        assert!(library_settings_changed(&old, &sort));
        assert!(library_settings_changed(&old, &channel));
        assert!(!library_settings_changed(&old, &speed));
    }

    #[test]
    fn library_title_totals_prints() {
        let mut lib = FakeLibrary::default();
        lib.0.insert(
            EpisodeKey::episode(5, 1, 2),
            vec![print("1080p", 100, 30), print("2160p DV", 400, 31)],
        );
        lib.0
            .insert(EpisodeKey::episode(5, 2, 1), vec![print("720p", 50, 10)]);
        let d = TitleDetails {
            summary: summary(5, MediaType::Tv, "Dark"),
            runtime_min: None,
            seasons: vec![
                SeasonInfo {
                    number: 1,
                    name: String::new(),
                    episode_count: 3,
                },
                SeasonInfo {
                    number: 2,
                    name: String::new(),
                    episode_count: 1,
                },
            ],
        };
        assert_eq!(library_keys(&d).len(), 4);
        let t = library_title(d, &lib);
        assert_eq!((t.newest, t.size), (31, 550));
        let none = library_title(
            TitleDetails {
                summary: summary(6, MediaType::Movie, "Heat"),
                runtime_min: None,
                seasons: Vec::new(),
            },
            &EmptyLibrary,
        );
        assert_eq!((none.newest, none.size), (0, 0));
    }

    #[test]
    fn library_sort_orders() {
        let t = |id: u64, title: &str, newest: i64, size: u64| LibraryTitle {
            summary: summary(id, MediaType::Movie, title),
            newest,
            size,
        };
        let titles = vec![
            t(1, "beta", 5, 10),
            t(2, "Alpha", 9, 5),
            t(3, "gamma", 1, 99),
        ];
        let ids = |v: Vec<TitleSummary>| v.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(
            ids(sort_library(titles.clone(), LibrarySort::Title)),
            [2, 1, 3]
        );
        assert_eq!(
            ids(sort_library(titles.clone(), LibrarySort::DateAdded)),
            [2, 1, 3]
        );
        assert_eq!(ids(sort_library(titles, LibrarySort::Size)), [3, 1, 2]);
    }

    #[test]
    fn trending_row_stamps() {
        assert_eq!(RowLoad::Loading.stamp(), Some("LOADING"));
        assert_eq!(RowLoad::Failed.stamp(), Some("REQUEST FAILED"));
        assert_eq!(RowLoad::Loaded(Vec::new()).stamp(), Some("NO RESULTS"));
        let cards = vec![media_card(&summary(1, MediaType::Movie, "A"), false)];
        assert_eq!(RowLoad::Loaded(cards).stamp(), None);
    }

    #[test]
    fn zones_skip_hidden_rows() {
        let mut graph = crate::focus::FocusGraph::with_zones(zones(0, 0, 3, 2));
        assert_eq!(
            graph.focus_first(),
            Some(crate::focus::Focus::new(TOP_BAR, 0))
        );
        let down = graph.move_focus(crate::focus::Direction::Down);
        assert_eq!(down, Some(crate::focus::Focus::new(MOVIES, 0)));
    }
}
