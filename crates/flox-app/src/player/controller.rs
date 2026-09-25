//! Player rules as a state machine over an [`Engine`], a [`Clock`] and the library [`Prints`].
//!
//! The controller never blocks and never touches mpv, Slint or the network. The view feeds it
//! [`Input`]s (keys, mouse, overlay buttons, engine events, results of async work and a
//! periodic [`Input::Tick`]), it drives the engine directly, and it returns the [`Effect`]s the
//! view has to carry out (async work, persistence, leaving). What to draw comes from
//! [`Controller::view`].
//!
//! Every rule is ported from the Android `PlayerActivity`, `NativePlayer`, `PlayerControls` and
//! `PlayerBridge`. Where Windows differs (the page is sniffed in a hidden WebView instead of
//! playing under the native surface) the difference is noted on the rule.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use flox_core::lang;
use flox_core::model::{EpisodeKey, MediaType, TmdbId};
use flox_core::progress::ProgressRecord;
use flox_core::settings::{ResumeMode, Settings};
use flox_core::sniff::{vidlink_url, Caption, SniffResult, VIDLINK_HOST};
use flox_player::tracks::{audio_label, AudioOption, SubTrack};
use flox_td::library::Entry;

/// The source is abandoned when nothing has played this long after a page load.
pub const WATCHDOG_MS: u64 = 45_000;
/// Progress is sampled this often.
pub const PROGRESS_TICK_MS: u64 = 2_000;
/// Progress is written at most this often, except at the end.
pub const PROGRESS_WRITE_MS: u64 = 10_000;
/// How long a hint stays up.
pub const HINT_MS: u64 = 2_500;
/// A key held this long (with repeats arriving) is a long press.
pub const LONG_PRESS_MS: u64 = 600;
/// Volume step in percent.
pub const VOLUME_STEP: u32 = 5;
/// Highest volume in percent (mpv amplifies above 100).
pub const VOLUME_MAX: u32 = 130;
/// The saved position is ignored when it is this close to the end.
pub const START_END_MARGIN_SECS: f64 = 5.0;

/// Request headers the page set that are not forwarded to mpv.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "accept-encoding",
    "user-agent",
    "cookie",
];

/// What plays the video: mpv for the library and sniffed streams, and the visible page player
/// as the last resort.
///
/// Engine failures are asynchronous and come back as [`Input::EngineError`]; a load that fails
/// synchronously returns `Err` and is treated the same way.
pub trait Engine {
    /// Plays a library print through the `flox://` stream. `start` is the position the
    /// controller will seek to on [`Input::FileLoaded`]; the engine must not seek by itself and
    /// may only use it as a hint (for example to prefetch the right part).
    fn load_library(
        &mut self,
        entry: &Entry,
        subtitle: Option<&Path>,
        start: u32,
    ) -> anyhow::Result<()>;
    /// Plays a sniffed manifest. `headers` are final (Referer and Origin included), `captions`
    /// are ordered preferred-first and must not be selected. `start` is as in `load_library`.
    fn load_url(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        captions: &[Caption],
        start: u32,
    ) -> anyhow::Result<()>;
    fn seek_to(&mut self, secs: f64);
    fn seek_by(&mut self, secs: i64);
    fn set_paused(&mut self, paused: bool);
    fn set_speed(&mut self, speed: f32);
    /// Selects the audio option; `ids` are the mpv track ids of one [`AudioOption`].
    fn set_audio(&mut self, ids: &[i64]);
    /// Selects a subtitle track by mpv id, or turns subtitles off.
    fn set_subtitle(&mut self, id: Option<i64>);
    /// Volume in percent, 0 to [`VOLUME_MAX`].
    fn set_volume(&mut self, percent: u32);
    /// Stops mpv and drops any cached library bytes.
    fn stop(&mut self);
    /// Shows the page player over the player area and loads `url` in it.
    fn page_load(&mut self, url: &str);
    /// Runs an action in the page player.
    fn page_action(&mut self, action: PageAction);
    /// Hides and unloads the page player.
    fn page_close(&mut self);
}

/// Time for the controller. `now_ms` is monotonic; `epoch_ms` stamps progress records.
pub trait Clock {
    fn now_ms(&self) -> u64;
    fn epoch_ms(&self) -> i64;
}

/// The library as the player sees it.
pub trait Prints {
    /// Whether Telegram is signed in and the library index is usable.
    fn ready(&self) -> bool;
    /// Every complete print of an item, highest first.
    fn prints(&self, key: EpisodeKey) -> Vec<Entry>;
}

/// The real clock.
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn epoch_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }
}

/// Page player actions (the `window.__flox` API of `flox_nav.js`).
#[derive(Clone, Debug, PartialEq)]
pub enum PageAction {
    /// Space into the page: toggles play, or activates in the page's own UI.
    Space,
    /// Space only when the page video is paused.
    Play,
    /// Space only when the page video is playing.
    Pause,
    ArrowLeft,
    ArrowRight,
    /// Seeks the page video by seconds.
    Seek(i64),
    EnterNav,
    ExitNav,
    Nav(Direction),
    /// Activates the focused page control.
    Activate,
    /// Opens the page's settings panel (entering navigation mode first).
    OpenSettings,
    /// Closes an open page panel; the result comes back as [`Input::PagePanelClosed`].
    ClosePanel,
    /// `__floxApplyStart(secs)`.
    ApplyStart(u32),
    /// `__floxApplySpeed(speed)`.
    ApplySpeed(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Logical keys (section 1 of the plan maps the keyboard, mouse and SMTC onto these).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Center,
    Back,
    Left,
    Right,
    Up,
    Down,
    /// M, the context-menu key or a right click.
    Menu,
    /// Ctrl+R.
    Reload,
    /// Space, or the play/pause media key.
    PlayPause,
    Play,
    Pause,
    Rewind,
    FastForward,
    /// The next-track media key.
    Next,
}

/// Overlay buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Back,
    Rewind,
    PlayPause,
    Forward,
    VolumeDown,
    VolumeUp,
    Audio,
    Subtitles,
    Quality,
    Next,
}

/// Player state reported by mpv observers or the page's `FLOX_TICK`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Playback {
    pub time: f64,
    pub duration: f64,
    pub paused: bool,
    pub ended: bool,
}

/// Everything the controller reacts to.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A key press or auto-repeat; `repeat` is 0 for the first press.
    Key {
        key: Key,
        repeat: u32,
    },
    /// A key release (needed for MENU and CENTER long presses).
    KeyUp(Key),
    Button(Button),
    /// The focused seek bar received LEFT (`forward = false`) or RIGHT.
    ScrubBy {
        forward: bool,
        repeat: u32,
    },
    /// The seek bar was clicked or dragged to a position.
    ScrubTo(f64),
    /// A track row in the open panel was picked.
    PickTrack(usize),
    MouseMove,
    /// A click on the video area (not on a control).
    VideoClick,
    /// The ASK dialog: `true` resumes, `false` starts over.
    Resume(bool),
    /// Drives every timer. Send it often (every 250 ms is plenty).
    Tick,
    /// The library subtitle finished downloading (or failed: `None`).
    LibraryPrepared {
        seq: u64,
        subtitle: Option<PathBuf>,
    },
    Sniffed {
        seq: u64,
        result: SniffResult,
    },
    SniffFailed {
        seq: u64,
    },
    /// The season's episode count from TMDB; 0 when the request failed.
    EpisodeCount(u32),
    /// mpv `file-loaded`, with the duration when already known (0 otherwise).
    FileLoaded {
        duration: f64,
    },
    /// The first video frame was rendered.
    FirstFrame,
    Playback(Playback),
    /// mpv's `track-list` changed. `aid` and `sid` are the selected track ids.
    Tracks {
        audio: Vec<AudioOption>,
        subs: Vec<SubTrack>,
        aid: Option<i64>,
        sid: Option<i64>,
    },
    EngineError(String),
    /// The page player finished loading its document.
    PageReady,
    /// The page player failed to load.
    PageFailed,
    /// The answer to [`PageAction::ClosePanel`].
    PagePanelClosed(bool),
}

/// Work the view carries out for the controller.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Close the player screen.
    Exit,
    SaveProgress(ProgressRecord),
    /// Persist `quality` in settings.
    SaveQuality(String),
    /// Persist `audio_language` in settings.
    SaveAudioLanguage(String),
    /// Sniff `page_url` in Playback mode and answer with `Sniffed` or `SniffFailed` carrying `seq`.
    /// Any earlier sniff can be cancelled.
    Sniff {
        seq: u64,
        page_url: String,
    },
    /// Download the entry's subtitle (if any) fully and answer with `LibraryPrepared`.
    PrepareLibrary {
        seq: u64,
        entry: Entry,
    },
    /// Fetch the season's episode count and answer with `EpisodeCount`.
    FetchEpisodeCount {
        tmdb: TmdbId,
        season: u32,
    },
    /// The overlay is up: its focus navigation owns this key.
    PassToOverlay {
        key: Key,
        repeat: u32,
    },
}

/// What is playing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub id: TmdbId,
    pub media: MediaType,
    pub title: String,
    pub poster: Option<String>,
    pub season: u32,
    pub episode: u32,
}

impl Meta {
    fn key(&self) -> EpisodeKey {
        match self.media {
            MediaType::Tv => EpisodeKey::episode(self.id, self.season, self.episode),
            MediaType::Movie => EpisodeKey::movie(self.id),
        }
    }
}

/// How the player screen was opened.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerConfig {
    pub meta: Meta,
    /// Saved position in seconds, 0 for none.
    pub start_at: u32,
    pub settings: Settings,
    /// The OS UI language (ISO 639-1), used when no subtitle language is set.
    pub ui_language: String,
}

/// Which surface is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The ASK dialog is showing.
    Asking,
    /// Resolving or opening a source; nothing is shown yet.
    Loading,
    /// mpv is showing video under the flox overlay.
    Native,
    /// The page player is visible with its own UI.
    Page,
    /// PLAYBACK FAILED; CENTER retries.
    Failed,
    Exited,
}

/// The tracks panel beside the overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TracksPanel {
    pub heading: String,
    pub labels: Vec<String>,
    pub selected: usize,
}

/// Everything the view draws.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewState {
    pub phase: Phase,
    /// `Resume from 12:34` while asking.
    pub resume_label: Option<String>,
    pub overlay_visible: bool,
    pub tracks: Option<TracksPanel>,
    pub hint: Option<String>,
    /// `S1 · E3` or `MOVIE`.
    pub eyebrow: String,
    pub title: String,
    pub position: String,
    pub duration: String,
    pub time: f64,
    pub length: f64,
    pub paused: bool,
    pub volume: u32,
    pub nav_mode: bool,
    pub audio_available: bool,
    pub subtitles_available: bool,
    pub quality_available: bool,
    pub next_available: bool,
}

/// `12:34`, or `1:02:03` from an hour: Android's `%02d:%02d` player stamp.
pub fn stamp(secs: u32) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// One step per press, growing to three and then six steps while the key is held.
pub fn seek_step(step: u32, repeat: u32) -> u32 {
    step * match repeat {
        0..=3 => 1,
        4..=9 => 3,
        _ => 6,
    }
}

/// `+10 S` / `-30 S`.
pub fn seek_hint(secs: i64) -> String {
    format!(
        "{}{} S",
        if secs < 0 { "-" } else { "+" },
        secs.unsigned_abs()
    )
}

/// A two-letter code for a track language (`"eng"` → `"en"`), else the code as given.
fn iso1(code: &str) -> String {
    lang::display_name(code)
        .and_then(lang::iso_from_english_name)
        .map_or_else(|| code.to_owned(), str::to_owned)
}

/// Referer and Origin for VidLink, then the page's headers minus the reserved ones; a page
/// header replaces a base one of the same name.
pub fn stream_headers(page: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = vec![
        ("Referer".to_owned(), format!("https://{VIDLINK_HOST}/")),
        ("Origin".to_owned(), format!("https://{VIDLINK_HOST}")),
    ];
    for (k, v) in page {
        let lower = k.to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        match out.iter_mut().find(|(o, _)| o.eq_ignore_ascii_case(k)) {
            Some(slot) => *slot = (k.clone(), v.clone()),
            None => out.push((k.clone(), v.clone())),
        }
    }
    out
}

/// Captions in the preferred language first, otherwise in page order.
pub fn order_captions(captions: &[Caption], preferred: &str) -> Vec<Caption> {
    let matches = |c: &Caption| {
        c.language.eq_ignore_ascii_case(preferred)
            || lang::iso_from_english_name(&c.language) == Some(preferred)
            || iso1(&c.language) == preferred
    };
    let mut out: Vec<Caption> = captions.iter().filter(|c| matches(c)).cloned().collect();
    out.extend(captions.iter().filter(|c| !matches(c)).cloned());
    out
}

#[derive(Clone, Copy, Debug, Default)]
struct Press {
    down_at: Option<u64>,
    long: bool,
}

impl Press {
    /// Records a key-down; true when it just became a long press.
    fn down(&mut self, repeat: u32, now: u64) -> bool {
        if repeat == 0 || self.down_at.is_none() {
            self.down_at = Some(now);
            self.long = false;
            return false;
        }
        let held = self.down_at.map_or(0, |at| now.saturating_sub(at));
        if !self.long && held >= LONG_PRESS_MS {
            self.long = true;
            return true;
        }
        false
    }

    /// Records a key-up; true when it ends a short press.
    fn up(&mut self) -> bool {
        let short = self.down_at.is_some() && !self.long;
        self.down_at = None;
        self.long = false;
        short
    }
}

/// The player state machine.
pub struct Controller<E: Engine, C: Clock, P: Prints> {
    engine: E,
    clock: C,
    prints: P,
    settings: Settings,
    ui_language: String,
    meta: Meta,
    start_at: u32,

    asking: bool,
    exited: bool,
    failed: bool,
    retried: bool,
    native_allowed: bool,
    native_shown: bool,
    native_active: bool,
    page_active: bool,
    library_failed: bool,
    playing_library: bool,
    library_entry: Option<Entry>,
    start_applied: bool,
    seq: u64,

    episode_count: u32,
    count_requested: bool,
    pending_end: bool,
    next_available: bool,

    playback: Playback,
    has_playback: bool,
    last_write: Option<u64>,
    ended_fired: bool,
    next_progress_at: u64,

    overlay_shown: bool,
    overlay_hide_at: u64,
    tracks_open: bool,
    hint: Option<String>,
    hint_hide_at: u64,
    watchdog_at: Option<u64>,
    nav_mode: bool,
    menu: Press,
    center: Press,

    audio: Vec<AudioOption>,
    subs: Vec<SubTrack>,
    aid: Option<i64>,
    sid: Option<i64>,
    volume: u32,

    effects: Vec<Effect>,
}

impl<E: Engine, C: Clock, P: Prints> Controller<E, C, P> {
    pub fn new(config: PlayerConfig, engine: E, clock: C, prints: P) -> Self {
        let now = clock.now_ms();
        Self {
            engine,
            clock,
            prints,
            settings: config.settings,
            ui_language: config.ui_language,
            meta: config.meta,
            start_at: config.start_at,
            asking: false,
            exited: false,
            failed: false,
            retried: false,
            native_allowed: true,
            native_shown: false,
            native_active: false,
            page_active: false,
            library_failed: false,
            playing_library: false,
            library_entry: None,
            start_applied: false,
            seq: 0,
            episode_count: 0,
            count_requested: false,
            pending_end: false,
            next_available: false,
            playback: Playback::default(),
            has_playback: false,
            last_write: None,
            ended_fired: false,
            next_progress_at: now + PROGRESS_TICK_MS,
            overlay_shown: false,
            overlay_hide_at: 0,
            tracks_open: false,
            hint: None,
            hint_hide_at: 0,
            watchdog_at: None,
            nav_mode: false,
            menu: Press::default(),
            center: Press::default(),
            audio: Vec::new(),
            subs: Vec::new(),
            aid: None,
            sid: None,
            volume: 100,
            effects: Vec::new(),
        }
    }

    pub fn engine(&self) -> &E {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut E {
        &mut self.engine
    }

    pub fn clock(&self) -> &C {
        &self.clock
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// The saved position the next load starts from.
    pub fn start_at(&self) -> u32 {
        self.start_at
    }

    /// Applies the resume mode: ALWAYS resumes, NEVER starts over, ASK shows the dialog.
    pub fn start(&mut self) -> Vec<Effect> {
        if self.start_at == 0 {
            self.load();
        } else {
            match self.settings.resume_mode {
                ResumeMode::Never => {
                    self.start_at = 0;
                    self.load();
                }
                ResumeMode::Ask => self.asking = true,
                ResumeMode::Always => self.load(),
            }
        }
        std::mem::take(&mut self.effects)
    }

    /// Feeds one input and returns what the view has to do.
    pub fn handle(&mut self, input: Input) -> Vec<Effect> {
        if !self.exited {
            self.dispatch(input);
        }
        std::mem::take(&mut self.effects)
    }

    pub fn phase(&self) -> Phase {
        if self.exited {
            Phase::Exited
        } else if self.asking {
            Phase::Asking
        } else if self.failed {
            Phase::Failed
        } else if self.native_shown {
            Phase::Native
        } else if self.page_active {
            Phase::Page
        } else {
            Phase::Loading
        }
    }

    pub fn view(&self) -> ViewState {
        let time = self.playback.time.max(0.0);
        let length = self.playback.duration.max(0.0);
        ViewState {
            phase: self.phase(),
            resume_label: self
                .asking
                .then(|| format!("Resume from {}", stamp(self.start_at))),
            overlay_visible: self.native_shown && self.overlay_shown,
            tracks: (self.native_shown && self.overlay_shown && self.tracks_open)
                .then(|| self.audio_panel()),
            hint: self.hint.clone(),
            eyebrow: match self.meta.media {
                MediaType::Tv => format!("S{} · E{}", self.meta.season, self.meta.episode),
                MediaType::Movie => "MOVIE".to_owned(),
            },
            title: self.meta.title.clone(),
            position: stamp(time as u32),
            duration: if length > 0.0 {
                stamp(length as u32)
            } else {
                "--:--".to_owned()
            },
            time,
            length,
            paused: self.playback.paused,
            volume: self.volume,
            nav_mode: self.nav_mode,
            audio_available: self.audio.len() > 1,
            subtitles_available: !self.subs.is_empty(),
            quality_available: self.library_entry.is_some() && self.variants().len() > 1,
            next_available: self.next_available,
        }
    }

    fn dispatch(&mut self, input: Input) {
        match input {
            Input::Key { key, repeat } => self.on_key(key, repeat),
            Input::KeyUp(key) => self.on_key_up(key),
            Input::Button(b) => self.on_button(b),
            Input::ScrubBy { forward, repeat } => {
                if self.native_shown {
                    let step = i64::from(seek_step(self.settings.seek_step_seconds, repeat));
                    self.engine.seek_by(if forward { step } else { -step });
                    self.touch();
                }
            }
            Input::ScrubTo(secs) => {
                if self.native_shown {
                    self.engine.seek_to(secs.max(0.0));
                    self.touch();
                }
            }
            Input::PickTrack(i) => self.pick_audio(i),
            Input::MouseMove => {
                if self.native_shown {
                    self.show_overlay();
                }
            }
            Input::VideoClick => {
                if self.native_shown {
                    self.toggle_play();
                    if self.overlay_shown {
                        self.touch();
                    }
                }
            }
            Input::Resume(resume) => {
                if self.asking {
                    self.asking = false;
                    if !resume {
                        self.start_at = 0;
                    }
                    self.load();
                }
            }
            Input::Tick => self.on_tick(),
            Input::LibraryPrepared { seq, subtitle } => {
                if seq == self.seq {
                    self.start_library(subtitle);
                }
            }
            Input::Sniffed { seq, result } => {
                if seq == self.seq {
                    self.start_url(result);
                }
            }
            Input::SniffFailed { seq } => {
                if seq == self.seq && !self.native_active {
                    self.on_load_failed();
                }
            }
            Input::EpisodeCount(n) => {
                self.episode_count = n;
                self.count_requested = false;
                self.next_available =
                    self.meta.media == MediaType::Tv && self.meta.episode < self.episode_count;
                if self.pending_end {
                    self.pending_end = false;
                    self.resolve_end();
                }
            }
            Input::FileLoaded { duration } => self.on_file_loaded(duration),
            Input::FirstFrame => self.on_first_frame(),
            Input::Playback(p) => {
                self.playback = p;
                if p.duration > 0.0 && p.time > 0.0 {
                    self.has_playback = true;
                }
                if p.ended {
                    self.progress();
                }
            }
            Input::Tracks {
                audio,
                subs,
                aid,
                sid,
            } => {
                self.audio = audio;
                self.subs = subs;
                self.aid = aid;
                self.sid = sid;
            }
            Input::EngineError(_) => self.on_engine_error(),
            Input::PageReady => {
                if self.page_active {
                    if self.start_at > 0 {
                        self.engine
                            .page_action(PageAction::ApplyStart(self.start_at));
                    }
                    let speed = self.settings.playback_speed;
                    if (speed - 1.0).abs() > f32::EPSILON {
                        self.engine.page_action(PageAction::ApplySpeed(speed));
                    }
                }
            }
            Input::PageFailed => {
                if self.page_active {
                    self.on_load_failed();
                }
            }
            Input::PagePanelClosed(closed) => {
                if !closed {
                    self.exit_nav();
                }
            }
        }
    }

    // ---- sources ----------------------------------------------------------------------------

    /// Picks the source: the library print when Telegram is ready, one exists and the library
    /// has not failed this episode; else the VidLink page (sniffed for mpv, or the page player
    /// once native playback has failed this session).
    fn load(&mut self) {
        self.reset_bridge();
        self.exit_nav();
        self.stop_native();
        self.hide_overlay();
        self.native_shown = false;
        self.failed = false;
        self.refresh_next();
        self.watchdog_at = None;
        self.seq += 1;
        let entry = if self.library_failed || !self.prints.ready() {
            None
        } else {
            self.default_print()
        };
        self.playing_library = entry.is_some();
        self.library_entry = entry.clone();
        if let Some(entry) = entry {
            if self.page_active {
                self.page_active = false;
                self.engine.page_close();
            }
            self.effects.push(Effect::PrepareLibrary {
                seq: self.seq,
                entry,
            });
            return;
        }
        self.watchdog_at = Some(self.clock.now_ms() + WATCHDOG_MS);
        let url = vidlink_url(self.meta.key(), Some(self.start_at));
        if self.native_allowed {
            self.effects.push(Effect::Sniff {
                seq: self.seq,
                page_url: url,
            });
        } else {
            self.page_active = true;
            self.engine.page_load(&url);
        }
    }

    /// The preferred quality when uploaded, else the highest.
    fn default_print(&self) -> Option<Entry> {
        let all = self.prints.prints(self.meta.key());
        let preferred = self.settings.quality.as_deref().unwrap_or("");
        all.iter()
            .find(|e| e.quality == preferred)
            .or_else(|| all.first())
            .cloned()
    }

    fn variants(&self) -> Vec<Entry> {
        self.prints.prints(self.meta.key())
    }

    fn start_library(&mut self, subtitle: Option<PathBuf>) {
        let Some(entry) = self.library_entry.clone() else {
            return;
        };
        self.start_applied = false;
        match self
            .engine
            .load_library(&entry, subtitle.as_deref(), self.start_at)
        {
            Ok(()) => self.after_native_load(),
            Err(_) => {
                self.native_active = true;
                self.on_engine_error();
            }
        }
    }

    fn start_url(&mut self, result: SniffResult) {
        if !self.native_allowed || self.native_active || self.page_active {
            return;
        }
        let preferred = self
            .settings
            .subtitle_language
            .clone()
            .unwrap_or_else(|| self.ui_language.clone());
        let captions = order_captions(&result.captions, &preferred);
        let headers = stream_headers(&result.headers);
        let at = self.start_at.max(self.playback.time.max(0.0) as u32);
        self.start_at = at;
        self.start_applied = false;
        match self.engine.load_url(&result.url, &headers, &captions, at) {
            Ok(()) => self.after_native_load(),
            Err(_) => {
                self.native_active = true;
                self.on_engine_error();
            }
        }
    }

    fn after_native_load(&mut self) {
        self.native_active = true;
        self.engine.set_speed(self.settings.playback_speed);
        if self.volume != 100 {
            self.engine.set_volume(self.volume);
        }
    }

    /// The saved position is applied once, and only when more than 5 s before the end.
    fn on_file_loaded(&mut self, duration: f64) {
        if !self.native_active || self.start_applied {
            return;
        }
        self.start_applied = true;
        let start = f64::from(self.start_at);
        if self.start_at > 0 && (duration <= 0.0 || start < duration - START_END_MARGIN_SECS) {
            self.engine.seek_to(start);
        }
    }

    fn on_first_frame(&mut self) {
        if self.native_shown || !self.native_active {
            return;
        }
        self.native_shown = true;
        self.watchdog_at = None;
        self.exit_nav();
    }

    fn stop_native(&mut self) {
        self.engine.stop();
        self.native_active = false;
        self.audio.clear();
        self.subs.clear();
        self.aid = None;
        self.sid = None;
    }

    /// A native failure: the library is dropped for this episode, or a sniffed stream sends
    /// the rest of the session to the page player. Windows has no page playing underneath, so
    /// both cases reload (Android only reloads once the page was unloaded).
    fn on_engine_error(&mut self) {
        if !self.native_active {
            return;
        }
        let at = self.playback.time.max(0.0) as u32;
        self.stop_native();
        if self.playing_library {
            self.library_failed = true;
        } else {
            self.native_allowed = false;
        }
        self.start_at = self.start_at.max(at);
        if self.native_shown {
            self.show_hint("RELOADING PLAYER".to_owned());
        }
        self.load();
    }

    /// One automatic reload covers transient failures before giving up.
    fn on_load_failed(&mut self) {
        if self.retried {
            self.show_failed();
            return;
        }
        self.retried = true;
        self.reload();
    }

    fn reload(&mut self) {
        self.start_at = self.start_at.max(self.playback.time.max(0.0) as u32);
        self.show_hint("RELOADING PLAYER".to_owned());
        self.load();
    }

    fn manual_reload(&mut self) {
        self.retried = false;
        self.reload();
    }

    fn show_failed(&mut self) {
        self.watchdog_at = None;
        self.failed = true;
        if self.page_active {
            self.page_active = false;
            self.engine.page_close();
        }
    }

    fn leave(&mut self) {
        self.stop_native();
        if self.page_active {
            self.page_active = false;
            self.engine.page_close();
        }
        self.exited = true;
        self.effects.push(Effect::Exit);
    }

    // ---- episodes ---------------------------------------------------------------------------

    fn refresh_next(&mut self) {
        let tv = self.meta.media == MediaType::Tv;
        self.next_available = tv && self.meta.episode < self.episode_count;
        if tv && self.episode_count == 0 && !self.count_requested {
            self.request_count();
        }
    }

    fn request_count(&mut self) {
        self.count_requested = true;
        self.effects.push(Effect::FetchEpisodeCount {
            tmdb: self.meta.id,
            season: self.meta.season,
        });
    }

    /// Next only within the season: no rollover.
    fn play_next(&mut self) {
        if self.meta.media != MediaType::Tv || self.meta.episode >= self.episode_count {
            return;
        }
        self.meta.episode += 1;
        self.start_at = 0;
        self.retried = false;
        self.library_failed = false;
        self.show_hint(format!(
            "NEXT · S{} E{}",
            self.meta.season, self.meta.episode
        ));
        self.load();
    }

    fn on_ended(&mut self) {
        if self.meta.media != MediaType::Tv {
            self.leave();
            return;
        }
        if self.episode_count == 0 {
            self.pending_end = true;
            if !self.count_requested {
                self.request_count();
            }
            return;
        }
        self.resolve_end();
    }

    fn resolve_end(&mut self) {
        if self.meta.episode >= self.episode_count {
            self.leave();
        } else if self.settings.autoplay_next {
            self.play_next();
        } else if self.native_shown {
            // stay on the last frame; the overlay offers the next episode
            self.next_available = true;
            self.show_overlay();
        } else {
            self.show_hint("EPISODE FINISHED".to_owned());
        }
    }

    // ---- progress ---------------------------------------------------------------------------

    fn reset_bridge(&mut self) {
        self.playback = Playback::default();
        self.has_playback = false;
        self.ended_fired = false;
        self.last_write = None;
    }

    /// Android's `PlayerBridge.tick`: written at most every 10 s, or at the end.
    fn progress(&mut self) {
        let p = self.playback;
        if p.duration <= 0.0 {
            return;
        }
        if p.time > 0.0 {
            self.has_playback = true;
        }
        let ended = p.ended || (p.time > 0.0 && p.time >= p.duration - 1.0);
        let now = self.clock.now_ms();
        let due = self
            .last_write
            .is_none_or(|at| now.saturating_sub(at) >= PROGRESS_WRITE_MS);
        if ended || due {
            self.last_write = Some(now);
            self.effects.push(Effect::SaveProgress(ProgressRecord {
                id: self.meta.id,
                media: self.meta.media,
                title: self.meta.title.clone(),
                poster: self.meta.poster.clone(),
                watched: p.time.max(0.0) as u32,
                duration: p.duration as u32,
                season: self.meta.season,
                episode: self.meta.episode,
                updated: self.clock.epoch_ms(),
            }));
        }
        if ended && !self.ended_fired {
            self.ended_fired = true;
            self.on_ended();
        }
    }

    fn on_tick(&mut self) {
        let now = self.clock.now_ms();
        if self.hint.is_some() && now >= self.hint_hide_at {
            self.hint = None;
        }
        if self.overlay_shown && now >= self.overlay_hide_at {
            self.hide_overlay();
        }
        if let Some(at) = self.watchdog_at {
            if now >= at {
                self.watchdog_at = None;
                if !self.has_playback {
                    self.on_load_failed();
                }
            }
        }
        if now >= self.next_progress_at {
            self.next_progress_at = now + PROGRESS_TICK_MS;
            if self.native_active || self.page_active {
                self.progress();
            }
        }
    }

    // ---- overlay ----------------------------------------------------------------------------

    fn show_overlay(&mut self) {
        self.overlay_shown = true;
        self.touch();
    }

    fn hide_overlay(&mut self) {
        self.tracks_open = false;
        self.overlay_shown = false;
    }

    /// Any interaction restarts the auto-hide timer.
    fn touch(&mut self) {
        self.overlay_hide_at = self.clock.now_ms() + u64::from(self.settings.overlay_hide_ms);
    }

    fn show_hint(&mut self, text: String) {
        self.hint = Some(text);
        self.hint_hide_at = self.clock.now_ms() + HINT_MS;
    }

    fn toggle_play(&mut self) {
        if self.playback.ended {
            return;
        }
        let paused = !self.playback.paused;
        self.playback.paused = paused;
        self.engine.set_paused(paused);
    }

    fn set_paused(&mut self, paused: bool) {
        self.playback.paused = paused;
        self.engine.set_paused(paused);
    }

    fn seek_hidden(&mut self, secs: i64) {
        self.engine.seek_by(secs);
        self.show_hint(seek_hint(secs));
    }

    fn media_key_step(&self) -> i64 {
        i64::from(self.settings.seek_step_seconds) * 3
    }

    fn adjust_volume(&mut self, up: bool) {
        self.volume = if up {
            (self.volume + VOLUME_STEP).min(VOLUME_MAX)
        } else {
            self.volume.saturating_sub(VOLUME_STEP)
        };
        self.engine.set_volume(self.volume);
        self.show_hint(format!("VOLUME · {}%", self.volume));
    }

    fn audio_panel(&self) -> TracksPanel {
        TracksPanel {
            heading: "AUDIO".to_owned(),
            labels: self.audio.iter().map(audio_label).collect(),
            selected: self.selected_audio().unwrap_or(0),
        }
    }

    fn selected_audio(&self) -> Option<usize> {
        let aid = self.aid?;
        self.audio.iter().position(|o| o.ids.contains(&aid))
    }

    fn show_audio_tracks(&mut self) {
        if self.audio.is_empty() {
            return;
        }
        self.tracks_open = true;
        self.touch();
    }

    /// Pins the audio option and remembers its language.
    fn pick_audio(&mut self, index: usize) {
        if !self.tracks_open {
            return;
        }
        self.tracks_open = false;
        let Some(option) = self.audio.get(index).cloned() else {
            return;
        };
        self.engine.set_audio(&option.ids);
        self.aid = option.ids.first().copied();
        if let Some(lang) = option.lang.as_deref() {
            let code = iso1(lang);
            self.settings.audio_language = Some(code.clone());
            self.effects.push(Effect::SaveAudioLanguage(code));
        }
        self.show_hint(format!("AUDIO · {}", audio_label(&option).to_uppercase()));
        self.touch();
    }

    /// Steps through off and each subtitle track.
    fn cycle_subtitles(&mut self) {
        if self.subs.is_empty() {
            self.show_hint("SUBTITLES OFF".to_owned());
            return;
        }
        let current = self
            .sid
            .and_then(|sid| self.subs.iter().position(|s| s.id == sid));
        let next = current.map_or(0, |i| i + 1);
        match self.subs.get(next).cloned() {
            None => {
                self.sid = None;
                self.engine.set_subtitle(None);
                self.show_hint("SUBTITLES OFF".to_owned());
            }
            Some(track) => {
                self.sid = Some(track.id);
                self.engine.set_subtitle(Some(track.id));
                let label = track
                    .title
                    .clone()
                    .or_else(|| {
                        track.lang.as_deref().map(|l| {
                            lang::display_name(l).map_or_else(|| l.to_owned(), str::to_owned)
                        })
                    })
                    .unwrap_or_else(|| "Subtitles".to_owned());
                self.show_hint(format!("SUBTITLES · {}", label.to_uppercase()));
            }
        }
    }

    /// Restarts the library file at the next uploaded print, keeping the position, and
    /// remembers the choice.
    fn cycle_quality(&mut self) {
        let Some(current) = self.library_entry.clone() else {
            return;
        };
        let all = self.variants();
        if all.len() < 2 {
            return;
        }
        let at = all
            .iter()
            .position(|e| e.label() == current.label())
            .map_or(0, |i| (i + 1) % all.len());
        let next = all[at].clone();
        self.settings.quality = Some(next.quality.clone());
        self.effects.push(Effect::SaveQuality(next.quality.clone()));
        self.start_at = self.playback.time.max(0.0) as u32;
        self.stop_native();
        self.hide_overlay();
        self.native_shown = false;
        self.library_entry = Some(next.clone());
        self.show_hint(format!("QUALITY · {}", next.label().to_uppercase()));
        self.seq += 1;
        self.effects.push(Effect::PrepareLibrary {
            seq: self.seq,
            entry: next,
        });
    }

    // ---- page navigation mode ---------------------------------------------------------------

    fn enter_nav(&mut self) {
        if self.nav_mode || !self.page_active {
            return;
        }
        self.nav_mode = true;
        self.engine.page_action(PageAction::EnterNav);
        self.show_hint("NAVIGATE · CENTER SELECT · BACK EXIT".to_owned());
    }

    fn exit_nav(&mut self) {
        if !self.nav_mode {
            return;
        }
        self.nav_mode = false;
        self.engine.page_action(PageAction::ExitNav);
        self.hint = None;
    }

    // ---- keys and buttons -------------------------------------------------------------------

    fn on_back(&mut self) {
        if self.asking {
            self.leave();
        } else if self.native_shown && self.tracks_open && self.overlay_shown {
            self.tracks_open = false;
        } else if self.native_shown && self.overlay_shown {
            self.hide_overlay();
        } else if self.nav_mode {
            self.engine.page_action(PageAction::ClosePanel);
        } else {
            self.leave();
        }
    }

    fn on_key(&mut self, key: Key, repeat: u32) {
        if key == Key::Back {
            if repeat == 0 {
                self.on_back();
            }
            return;
        }
        if self.asking {
            return;
        }
        let now = self.clock.now_ms();
        if key == Key::Reload {
            if repeat == 0 {
                self.manual_reload();
            }
            return;
        }
        if key == Key::Menu {
            if self.menu.down(repeat, now) {
                self.manual_reload();
            }
            return;
        }
        if self.native_shown {
            self.native_key(key, repeat);
            return;
        }
        self.page_key(key, repeat, now);
    }

    fn on_key_up(&mut self, key: Key) {
        if self.asking {
            return;
        }
        match key {
            // the release decides between a short and a long press
            Key::Menu if self.menu.up() => {
                if self.native_shown {
                    self.cycle_subtitles();
                } else if self.page_active {
                    self.enter_nav();
                    self.engine.page_action(PageAction::OpenSettings);
                }
            }
            Key::Center if self.center.up() && !self.native_shown && self.page_active => {
                let action = if self.nav_mode {
                    PageAction::Activate
                } else {
                    PageAction::Space
                };
                self.engine.page_action(action);
            }
            _ => {}
        }
    }

    fn native_key(&mut self, key: Key, repeat: u32) {
        // media keys work whether or not the overlay is up
        let media = match key {
            Key::PlayPause => {
                self.toggle_play();
                true
            }
            Key::Play => {
                self.set_paused(false);
                true
            }
            Key::Pause => {
                self.set_paused(true);
                true
            }
            Key::Rewind => {
                self.seek_hidden(-self.media_key_step());
                true
            }
            Key::FastForward => {
                self.seek_hidden(self.media_key_step());
                true
            }
            Key::Next => {
                self.play_next();
                true
            }
            _ => false,
        };
        if media {
            if self.overlay_shown {
                self.touch();
            }
            return;
        }
        // while the overlay is up, focus navigation owns the arrows and CENTER
        if self.overlay_shown {
            self.touch();
            self.effects.push(Effect::PassToOverlay { key, repeat });
            return;
        }
        let step = i64::from(seek_step(self.settings.seek_step_seconds, repeat));
        match key {
            Key::Center if repeat == 0 => {
                self.toggle_play();
                self.show_overlay();
            }
            Key::Left => self.seek_hidden(-step),
            Key::Right => self.seek_hidden(step),
            Key::Up | Key::Down => self.show_overlay(),
            _ => {}
        }
    }

    fn page_key(&mut self, key: Key, repeat: u32, now: u64) {
        if key == Key::Center {
            if self.failed {
                if repeat == 0 {
                    self.manual_reload();
                }
                return;
            }
            if self.center.down(repeat, now) && self.page_active && !self.nav_mode {
                self.enter_nav();
            }
            return;
        }
        if !self.page_active {
            return;
        }
        if self.nav_mode {
            let action = match key {
                Key::Left => Some(PageAction::Nav(Direction::Left)),
                Key::Right => Some(PageAction::Nav(Direction::Right)),
                Key::Up => Some(PageAction::Nav(Direction::Up)),
                Key::Down => Some(PageAction::Nav(Direction::Down)),
                Key::PlayPause => Some(PageAction::Space),
                _ => None,
            };
            if let Some(a) = action {
                self.engine.page_action(a);
            }
            return;
        }
        let step = self.media_key_step();
        match key {
            Key::PlayPause => self.engine.page_action(PageAction::Space),
            Key::Play => self.engine.page_action(PageAction::Play),
            Key::Pause => self.engine.page_action(PageAction::Pause),
            Key::Left => self.engine.page_action(PageAction::ArrowLeft),
            Key::Right => self.engine.page_action(PageAction::ArrowRight),
            Key::Rewind => self.engine.page_action(PageAction::Seek(-step)),
            Key::FastForward => self.engine.page_action(PageAction::Seek(step)),
            Key::Up | Key::Down => self.enter_nav(),
            _ => {}
        }
    }

    fn on_button(&mut self, button: Button) {
        if !self.native_shown {
            if button == Button::Back {
                self.leave();
            }
            return;
        }
        let step = i64::from(self.settings.seek_step_seconds);
        match button {
            Button::Back => {
                self.leave();
                return;
            }
            Button::Next => {
                self.play_next();
                return;
            }
            Button::PlayPause => self.toggle_play(),
            Button::Rewind => self.engine.seek_by(-step),
            Button::Forward => self.engine.seek_by(step),
            Button::VolumeDown => self.adjust_volume(false),
            Button::VolumeUp => self.adjust_volume(true),
            Button::Audio => self.show_audio_tracks(),
            Button::Subtitles => self.cycle_subtitles(),
            Button::Quality => self.cycle_quality(),
        }
        self.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use flox_core::sniff::StreamKind;
    use flox_td::library::Part;

    #[derive(Clone, Debug, PartialEq)]
    enum Call {
        LoadLibrary(String, Option<PathBuf>, u32),
        LoadUrl(String, Vec<(String, String)>, Vec<String>, u32),
        SeekTo(f64),
        SeekBy(i64),
        Paused(bool),
        Speed(f32),
        Audio(Vec<i64>),
        Subtitle(Option<i64>),
        Volume(u32),
        Stop,
        PageLoad(String),
        Page(PageAction),
        PageClose,
    }

    #[derive(Default)]
    struct FakeEngine {
        calls: Vec<Call>,
        fail_loads: bool,
    }

    impl FakeEngine {
        fn load_result(&self) -> anyhow::Result<()> {
            if self.fail_loads {
                anyhow::bail!("refused")
            }
            Ok(())
        }
    }

    impl Engine for FakeEngine {
        fn load_library(
            &mut self,
            entry: &Entry,
            subtitle: Option<&Path>,
            start: u32,
        ) -> anyhow::Result<()> {
            self.calls.push(Call::LoadLibrary(
                entry.label(),
                subtitle.map(Path::to_path_buf),
                start,
            ));
            self.load_result()
        }
        fn load_url(
            &mut self,
            url: &str,
            headers: &[(String, String)],
            captions: &[Caption],
            start: u32,
        ) -> anyhow::Result<()> {
            self.calls.push(Call::LoadUrl(
                url.to_owned(),
                headers.to_vec(),
                captions.iter().map(|c| c.language.clone()).collect(),
                start,
            ));
            self.load_result()
        }
        fn seek_to(&mut self, secs: f64) {
            self.calls.push(Call::SeekTo(secs));
        }
        fn seek_by(&mut self, secs: i64) {
            self.calls.push(Call::SeekBy(secs));
        }
        fn set_paused(&mut self, paused: bool) {
            self.calls.push(Call::Paused(paused));
        }
        fn set_speed(&mut self, speed: f32) {
            self.calls.push(Call::Speed(speed));
        }
        fn set_audio(&mut self, ids: &[i64]) {
            self.calls.push(Call::Audio(ids.to_vec()));
        }
        fn set_subtitle(&mut self, id: Option<i64>) {
            self.calls.push(Call::Subtitle(id));
        }
        fn set_volume(&mut self, percent: u32) {
            self.calls.push(Call::Volume(percent));
        }
        fn stop(&mut self) {
            self.calls.push(Call::Stop);
        }
        fn page_load(&mut self, url: &str) {
            self.calls.push(Call::PageLoad(url.to_owned()));
        }
        fn page_action(&mut self, action: PageAction) {
            self.calls.push(Call::Page(action));
        }
        fn page_close(&mut self) {
            self.calls.push(Call::PageClose);
        }
    }

    #[derive(Clone, Default)]
    struct FakeClock(Rc<Cell<u64>>);

    impl FakeClock {
        fn advance(&self, ms: u64) {
            self.0.set(self.0.get() + ms);
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.get()
        }
        fn epoch_ms(&self) -> i64 {
            1_700_000_000_000 + i64::try_from(self.0.get()).unwrap_or(0)
        }
    }

    #[derive(Clone, Default)]
    struct FakePrints {
        ready: Rc<Cell<bool>>,
        entries: Rc<RefCell<Vec<Entry>>>,
    }

    impl Prints for FakePrints {
        fn ready(&self) -> bool {
            self.ready.get()
        }
        fn prints(&self, key: EpisodeKey) -> Vec<Entry> {
            self.entries
                .borrow()
                .iter()
                .filter(|e| e.key == key)
                .cloned()
                .collect()
        }
    }

    type Ctl = Controller<FakeEngine, FakeClock, FakePrints>;

    struct Rig {
        c: Ctl,
        clock: FakeClock,
        prints: FakePrints,
    }

    fn print(key: EpisodeKey, quality: &str, codec: &str) -> Entry {
        Entry {
            key,
            quality: quality.to_owned(),
            codec: codec.to_owned(),
            parts: vec![Part {
                message_id: 1,
                file_id: 1,
                size: 10,
            }],
            subtitle: None,
            size: 10,
            newest_message_id: 1,
        }
    }

    fn tv_meta(episode: u32) -> Meta {
        Meta {
            id: 1399,
            media: MediaType::Tv,
            title: "Show".to_owned(),
            poster: Some("/p.jpg".to_owned()),
            season: 1,
            episode,
        }
    }

    fn movie_meta() -> Meta {
        Meta {
            id: 550,
            media: MediaType::Movie,
            title: "Film".to_owned(),
            poster: None,
            season: 0,
            episode: 0,
        }
    }

    fn rig_with(meta: Meta, start_at: u32, settings: Settings) -> Rig {
        let clock = FakeClock::default();
        clock.advance(1_000);
        let prints = FakePrints::default();
        let c = Controller::new(
            PlayerConfig {
                meta,
                start_at,
                settings,
                ui_language: "en".to_owned(),
            },
            FakeEngine::default(),
            clock.clone(),
            prints.clone(),
        );
        Rig { c, clock, prints }
    }

    fn rig(meta: Meta, start_at: u32) -> Rig {
        rig_with(meta, start_at, Settings::default())
    }

    impl Rig {
        fn calls(&self) -> &[Call] {
            &self.c.engine().calls
        }
        fn clear(&mut self) {
            self.c.engine_mut().calls.clear();
        }
        fn key(&mut self, key: Key) -> Vec<Effect> {
            self.c.handle(Input::Key { key, repeat: 0 })
        }
        fn tick(&mut self, ms: u64) -> Vec<Effect> {
            self.clock.advance(ms);
            self.c.handle(Input::Tick)
        }
        fn seq(effects: &[Effect]) -> Option<u64> {
            effects.iter().find_map(|e| match e {
                Effect::Sniff { seq, .. } | Effect::PrepareLibrary { seq, .. } => Some(*seq),
                _ => None,
            })
        }
        fn sniff_ok(&mut self, seq: u64) -> Vec<Effect> {
            self.c.handle(Input::Sniffed {
                seq,
                result: SniffResult {
                    url: "https://cdn/x.m3u8".to_owned(),
                    kind: StreamKind::Hls,
                    headers: vec![("X-Token".to_owned(), "t".to_owned())],
                    captions: Vec::new(),
                },
            })
        }
        /// Starts on the sniffed path and shows the first frame.
        fn native_url(&mut self) {
            let fx = self.c.start();
            let seq = Rig::seq(&fx).unwrap_or(0);
            self.sniff_ok(seq);
            self.c.handle(Input::FileLoaded { duration: 3000.0 });
            self.c.handle(Input::FirstFrame);
            self.c.handle(Input::Playback(Playback {
                time: 100.0,
                duration: 3000.0,
                paused: false,
                ended: false,
            }));
            self.clear();
        }
        /// Starts on the library path and shows the first frame.
        fn native_library(&mut self) {
            self.prints.ready.set(true);
            let fx = self.c.start();
            let seq = Rig::seq(&fx).unwrap_or(0);
            self.c.handle(Input::LibraryPrepared {
                seq,
                subtitle: None,
            });
            self.c.handle(Input::FileLoaded { duration: 3000.0 });
            self.c.handle(Input::FirstFrame);
            self.c.handle(Input::Playback(Playback {
                time: 100.0,
                duration: 3000.0,
                paused: false,
                ended: false,
            }));
            self.clear();
        }
        fn end(&mut self) -> Vec<Effect> {
            self.c.handle(Input::Playback(Playback {
                time: 3000.0,
                duration: 3000.0,
                paused: false,
                ended: true,
            }))
        }
        fn hint(&self) -> Option<String> {
            self.c.view().hint
        }
    }

    fn sub(id: i64, title: Option<&str>, lang: Option<&str>) -> SubTrack {
        SubTrack {
            id,
            lang: lang.map(str::to_owned),
            title: title.map(str::to_owned),
            codec: "subrip".to_owned(),
            external: false,
        }
    }

    fn audio(ids: &[i64], lang: &str, codec: &str, channels: u32) -> AudioOption {
        AudioOption {
            ids: ids.to_vec(),
            lang: Some(lang.to_owned()),
            codec: codec.to_owned(),
            channels,
        }
    }

    // ---- helpers ----

    #[test]
    fn stamp_is_zero_padded() {
        assert_eq!(stamp(0), "00:00");
        assert_eq!(stamp(65), "01:05");
        assert_eq!(stamp(3723), "1:02:03");
    }

    #[test]
    fn seek_step_accelerates_with_repeats() {
        assert_eq!(seek_step(10, 0), 10);
        assert_eq!(seek_step(10, 3), 10);
        assert_eq!(seek_step(10, 4), 30);
        assert_eq!(seek_step(10, 9), 30);
        assert_eq!(seek_step(10, 10), 60);
        assert_eq!(seek_hint(10), "+10 S");
        assert_eq!(seek_hint(-60), "-60 S");
    }

    #[test]
    fn headers_add_vidlink_and_drop_reserved() {
        let h = stream_headers(&[
            ("Cookie".to_owned(), "c".to_owned()),
            ("user-agent".to_owned(), "u".to_owned()),
            ("referer".to_owned(), "https://other/".to_owned()),
            ("X-A".to_owned(), "1".to_owned()),
        ]);
        assert_eq!(
            h,
            vec![
                ("referer".to_owned(), "https://other/".to_owned()),
                ("Origin".to_owned(), "https://vidlink.pro".to_owned()),
                ("X-A".to_owned(), "1".to_owned()),
            ]
        );
    }

    #[test]
    fn captions_put_the_preferred_language_first() {
        let cap = |l: &str| Caption {
            url: format!("https://c/{l}.vtt"),
            language: l.to_owned(),
            kind: "vtt".to_owned(),
        };
        let out = order_captions(&[cap("Spanish"), cap("English"), cap("fr")], "en");
        let langs: Vec<&str> = out.iter().map(|c| c.language.as_str()).collect();
        assert_eq!(langs, ["English", "Spanish", "fr"]);
    }

    // ---- resume ----

    #[test]
    fn resume_always_starts_at_the_saved_position() {
        let mut r = rig(movie_meta(), 600);
        let fx = r.c.start();
        assert_eq!(r.c.phase(), Phase::Loading);
        assert!(fx.iter().any(
            |e| matches!(e, Effect::Sniff { page_url, .. } if page_url.ends_with("&startAt=600"))
        ));
    }

    #[test]
    fn resume_never_starts_over() {
        let settings = Settings {
            resume_mode: ResumeMode::Never,
            ..Settings::default()
        };
        let mut r = rig_with(movie_meta(), 600, settings);
        let fx = r.c.start();
        assert_eq!(r.c.start_at(), 0);
        assert!(fx
            .iter()
            .any(|e| matches!(e, Effect::Sniff { page_url, .. } if !page_url.contains("startAt"))));
    }

    #[test]
    fn resume_ask_shows_the_dialog_and_resumes() {
        let settings = Settings {
            resume_mode: ResumeMode::Ask,
            ..Settings::default()
        };
        let mut r = rig_with(movie_meta(), 754, settings);
        assert!(r.c.start().is_empty());
        let v = r.c.view();
        assert_eq!(v.phase, Phase::Asking);
        assert_eq!(v.resume_label.as_deref(), Some("Resume from 12:34"));
        let fx = r.c.handle(Input::Resume(true));
        assert_eq!(r.c.start_at(), 754);
        assert!(matches!(fx.as_slice(), [Effect::Sniff { .. }]));
    }

    #[test]
    fn resume_ask_start_over_resets_the_position() {
        let settings = Settings {
            resume_mode: ResumeMode::Ask,
            ..Settings::default()
        };
        let mut r = rig_with(movie_meta(), 754, settings);
        r.c.start();
        r.c.handle(Input::Resume(false));
        assert_eq!(r.c.start_at(), 0);
        assert_eq!(r.c.phase(), Phase::Loading);
    }

    #[test]
    fn resume_ask_back_leaves_the_player() {
        let settings = Settings {
            resume_mode: ResumeMode::Ask,
            ..Settings::default()
        };
        let mut r = rig_with(movie_meta(), 754, settings);
        r.c.start();
        let fx = r.key(Key::Back);
        assert!(fx.contains(&Effect::Exit));
        assert_eq!(r.c.phase(), Phase::Exited);
        assert!(!fx.iter().any(|e| matches!(e, Effect::Sniff { .. })));
    }

    #[test]
    fn no_saved_position_skips_the_dialog() {
        let settings = Settings {
            resume_mode: ResumeMode::Ask,
            ..Settings::default()
        };
        let mut r = rig_with(movie_meta(), 0, settings);
        r.c.start();
        assert_eq!(r.c.phase(), Phase::Loading);
    }

    // ---- sources ----

    #[test]
    fn library_is_used_when_ready_with_an_entry() {
        let mut r = rig(tv_meta(2), 90);
        let key = EpisodeKey::episode(1399, 1, 2);
        r.prints.ready.set(true);
        r.prints
            .entries
            .borrow_mut()
            .push(print(key, "1080p", "h264"));
        let fx = r.c.start();
        let seq = Rig::seq(&fx).unwrap_or(0);
        assert!(fx.iter().any(
            |e| matches!(e, Effect::PrepareLibrary { entry, .. } if entry.quality == "1080p")
        ));
        r.c.handle(Input::LibraryPrepared {
            seq,
            subtitle: Some(PathBuf::from("/tmp/s.srt")),
        });
        assert_eq!(
            r.calls()[1..],
            [
                Call::LoadLibrary(
                    "1080p h264".to_owned(),
                    Some(PathBuf::from("/tmp/s.srt")),
                    90
                ),
                Call::Speed(1.0),
            ]
        );
    }

    #[test]
    fn library_prefers_the_saved_quality_else_the_highest() {
        let key = EpisodeKey::movie(550);
        let mut r = rig_with(
            movie_meta(),
            0,
            Settings {
                quality: Some("1080p".to_owned()),
                ..Settings::default()
            },
        );
        r.prints.ready.set(true);
        r.prints
            .entries
            .borrow_mut()
            .extend([print(key, "2160p DV", "hevc"), print(key, "1080p", "h264")]);
        let fx = r.c.start();
        assert!(fx.iter().any(
            |e| matches!(e, Effect::PrepareLibrary { entry, .. } if entry.quality == "1080p")
        ));

        let mut r = rig(movie_meta(), 0);
        r.prints.ready.set(true);
        r.prints
            .entries
            .borrow_mut()
            .extend([print(key, "2160p DV", "hevc"), print(key, "1080p", "h264")]);
        let fx = r.c.start();
        assert!(fx.iter().any(
            |e| matches!(e, Effect::PrepareLibrary { entry, .. } if entry.quality == "2160p DV")
        ));
    }

    #[test]
    fn page_is_used_when_telegram_is_not_ready() {
        let mut r = rig(tv_meta(2), 0);
        r.prints
            .entries
            .borrow_mut()
            .push(print(EpisodeKey::episode(1399, 1, 2), "1080p", "h264"));
        let fx = r.c.start();
        assert!(fx.iter().any(|e| matches!(e, Effect::Sniff { page_url, .. } if page_url == "https://vidlink.pro/tv/1399/1/2?autoplay=true&primaryColor=fafafa&nextbutton=false")));
        assert!(fx.contains(&Effect::FetchEpisodeCount {
            tmdb: 1399,
            season: 1
        }));
    }

    #[test]
    fn sniffed_stream_plays_natively_with_headers() {
        let mut r = rig(movie_meta(), 42);
        let fx = r.c.start();
        r.sniff_ok(Rig::seq(&fx).unwrap_or(0));
        let Call::LoadUrl(url, headers, _, start) = &r.calls()[1] else {
            panic!("expected load_url, got {:?}", r.calls());
        };
        assert_eq!(url, "https://cdn/x.m3u8");
        assert_eq!(*start, 42);
        assert_eq!(headers.len(), 3);
        assert_eq!(r.calls()[2], Call::Speed(1.0));
    }

    #[test]
    fn stale_async_results_are_ignored() {
        let mut r = rig(movie_meta(), 0);
        let fx = r.c.start();
        let seq = Rig::seq(&fx).unwrap_or(0);
        r.key(Key::Reload);
        r.clear();
        r.sniff_ok(seq);
        r.c.handle(Input::SniffFailed { seq });
        assert!(r.calls().is_empty());
        assert_eq!(r.c.phase(), Phase::Loading);
    }

    #[test]
    fn library_failure_falls_back_to_the_page_for_this_episode() {
        let mut r = rig(movie_meta(), 0);
        r.prints
            .entries
            .borrow_mut()
            .push(print(EpisodeKey::movie(550), "1080p", "h264"));
        r.native_library();
        r.c.handle(Input::Playback(Playback {
            time: 300.0,
            duration: 3000.0,
            paused: false,
            ended: false,
        }));
        let fx = r.c.handle(Input::EngineError("demux".to_owned()));
        assert!(fx.iter().any(
            |e| matches!(e, Effect::Sniff { page_url, .. } if page_url.ends_with("startAt=300"))
        ));
        assert_eq!(r.hint().as_deref(), Some("RELOADING PLAYER"));
        // a manual reload keeps the library out
        let fx = r.key(Key::Reload);
        assert!(fx.iter().any(|e| matches!(e, Effect::Sniff { .. })));
    }

    #[test]
    fn synchronous_library_load_failure_falls_back() {
        let mut r = rig(movie_meta(), 0);
        r.prints.ready.set(true);
        r.prints
            .entries
            .borrow_mut()
            .push(print(EpisodeKey::movie(550), "1080p", "h264"));
        r.c.engine_mut().fail_loads = true;
        let fx = r.c.start();
        let fx = r.c.handle(Input::LibraryPrepared {
            seq: Rig::seq(&fx).unwrap_or(0),
            subtitle: None,
        });
        assert!(fx.iter().any(|e| matches!(e, Effect::Sniff { .. })));
    }

    #[test]
    fn native_error_moves_to_the_page_player_for_the_session() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.c.handle(Input::EngineError("http 403".to_owned()));
        assert!(r.calls().contains(&Call::PageLoad(
            "https://vidlink.pro/movie/550?autoplay=true&primaryColor=fafafa&nextbutton=false&startAt=100".to_owned()
        )));
        assert_eq!(r.c.phase(), Phase::Page);
        // a reload stays on the page
        r.clear();
        r.key(Key::Reload);
        assert!(matches!(r.calls().last(), Some(Call::PageLoad(_))));
    }

    #[test]
    fn native_error_before_the_first_frame_also_uses_the_page() {
        let mut r = rig(movie_meta(), 0);
        let fx = r.c.start();
        r.sniff_ok(Rig::seq(&fx).unwrap_or(0));
        r.c.handle(Input::EngineError("codec".to_owned()));
        assert_eq!(r.c.phase(), Phase::Page);
        assert!(r.hint().is_none());
    }

    #[test]
    fn late_engine_errors_after_stop_are_ignored() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.key(Key::Reload);
        r.clear();
        let fx = r.c.handle(Input::EngineError("late".to_owned()));
        assert!(fx.is_empty());
        assert!(r.calls().is_empty());
    }

    // ---- watchdog, reload and failure ----

    #[test]
    fn watchdog_reloads_once_then_fails_and_center_retries() {
        let mut r = rig(movie_meta(), 0);
        r.c.start();
        assert!(r.tick(WATCHDOG_MS - 1).is_empty() || r.c.phase() == Phase::Loading);
        let fx = r.tick(1);
        assert!(fx.iter().any(|e| matches!(e, Effect::Sniff { .. })));
        assert_eq!(r.hint().as_deref(), Some("RELOADING PLAYER"));
        r.tick(WATCHDOG_MS);
        assert_eq!(r.c.phase(), Phase::Failed);
        let fx = r.key(Key::Center);
        assert_eq!(r.c.phase(), Phase::Loading);
        assert!(fx.iter().any(|e| matches!(e, Effect::Sniff { .. })));
        // the retry gets its own automatic reload again
        let fx = r.tick(WATCHDOG_MS);
        assert!(fx.iter().any(|e| matches!(e, Effect::Sniff { .. })));
    }

    #[test]
    fn watchdog_is_quiet_once_something_plays() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.tick(WATCHDOG_MS + 10);
        assert_eq!(r.c.phase(), Phase::Native);
        assert!(!r.calls().contains(&Call::Stop));
    }

    #[test]
    fn page_player_playback_satisfies_the_watchdog() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.c.handle(Input::EngineError("x".to_owned()));
        r.c.handle(Input::Playback(Playback {
            time: 5.0,
            duration: 3000.0,
            paused: false,
            ended: false,
        }));
        r.tick(WATCHDOG_MS + 10);
        assert_eq!(r.c.phase(), Phase::Page);
    }

    #[test]
    fn sniff_failure_reloads_once_then_fails() {
        let mut r = rig(movie_meta(), 0);
        let fx = r.c.start();
        let fx = r.c.handle(Input::SniffFailed {
            seq: Rig::seq(&fx).unwrap_or(0),
        });
        let seq = Rig::seq(&fx).unwrap_or(0);
        r.c.handle(Input::SniffFailed { seq });
        assert_eq!(r.c.phase(), Phase::Failed);
    }

    // ---- start position ----

    #[test]
    fn start_position_is_applied_once_on_file_loaded() {
        let mut r = rig(movie_meta(), 600);
        let fx = r.c.start();
        r.sniff_ok(Rig::seq(&fx).unwrap_or(0));
        r.clear();
        r.c.handle(Input::FileLoaded { duration: 3000.0 });
        r.c.handle(Input::FileLoaded { duration: 3000.0 });
        assert_eq!(r.calls(), [Call::SeekTo(600.0)]);
    }

    #[test]
    fn start_position_near_the_end_is_ignored() {
        let mut r = rig(movie_meta(), 2996);
        let fx = r.c.start();
        r.sniff_ok(Rig::seq(&fx).unwrap_or(0));
        r.clear();
        r.c.handle(Input::FileLoaded { duration: 3000.0 });
        assert!(r.calls().is_empty());
    }

    // ---- native keys ----

    #[test]
    fn arrows_seek_with_acceleration_while_hidden() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        for repeat in [0, 4, 10] {
            r.c.handle(Input::Key {
                key: Key::Right,
                repeat,
            });
        }
        r.c.handle(Input::Key {
            key: Key::Left,
            repeat: 0,
        });
        assert_eq!(
            r.calls(),
            [
                Call::SeekBy(10),
                Call::SeekBy(30),
                Call::SeekBy(60),
                Call::SeekBy(-10)
            ]
        );
        assert_eq!(r.hint().as_deref(), Some("-10 S"));
        assert!(!r.c.view().overlay_visible);
    }

    #[test]
    fn rewind_and_fast_forward_use_three_steps() {
        let mut r = rig_with(
            movie_meta(),
            0,
            Settings {
                seek_step_seconds: 15,
                ..Settings::default()
            },
        );
        r.native_url();
        r.key(Key::FastForward);
        assert_eq!(r.hint().as_deref(), Some("+45 S"));
        r.key(Key::Rewind);
        assert_eq!(r.calls(), [Call::SeekBy(45), Call::SeekBy(-45)]);
        assert_eq!(r.hint().as_deref(), Some("-45 S"));
    }

    #[test]
    fn center_toggles_play_and_shows_the_overlay() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.key(Key::Center);
        assert_eq!(r.calls(), [Call::Paused(true)]);
        assert!(r.c.view().overlay_visible);
        assert!(r.c.view().paused);
    }

    #[test]
    fn overlay_owns_the_arrows_and_center_while_shown() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.key(Key::Up);
        let fx = r.key(Key::Left);
        assert_eq!(
            fx,
            [Effect::PassToOverlay {
                key: Key::Left,
                repeat: 0
            }]
        );
        let fx = r.key(Key::Center);
        assert!(matches!(
            fx.as_slice(),
            [Effect::PassToOverlay {
                key: Key::Center,
                ..
            }]
        ));
        assert!(r.calls().is_empty());
    }

    #[test]
    fn overlay_hides_after_the_timeout_and_interaction_restarts_it() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.key(Key::Down);
        r.tick(3_000);
        r.key(Key::Left); // passed to the overlay, restarts the timer
        r.tick(3_000);
        assert!(r.c.view().overlay_visible);
        r.tick(1_000);
        assert!(!r.c.view().overlay_visible);
    }

    #[test]
    fn media_keys_work_with_the_overlay_up() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.key(Key::Up);
        r.key(Key::Pause);
        r.key(Key::Play);
        r.key(Key::PlayPause);
        assert_eq!(
            r.calls(),
            [Call::Paused(true), Call::Paused(false), Call::Paused(true)]
        );
        assert!(r.c.view().overlay_visible);
    }

    #[test]
    fn mouse_shows_the_overlay_and_click_toggles_play() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.c.handle(Input::MouseMove);
        assert!(r.c.view().overlay_visible);
        r.c.handle(Input::VideoClick);
        r.c.handle(Input::VideoClick);
        assert_eq!(r.calls(), [Call::Paused(true), Call::Paused(false)]);
    }

    #[test]
    fn back_closes_tracks_then_overlay_then_leaves() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.c.handle(Input::Tracks {
            audio: vec![audio(&[1], "eng", "eac3", 6), audio(&[2], "spa", "ac3", 6)],
            subs: Vec::new(),
            aid: Some(1),
            sid: None,
        });
        r.c.handle(Input::MouseMove);
        r.c.handle(Input::Button(Button::Audio));
        assert!(r.c.view().tracks.is_some());
        assert!(r.key(Key::Back).is_empty());
        assert!(r.c.view().tracks.is_none());
        assert!(r.c.view().overlay_visible);
        r.key(Key::Back);
        assert!(!r.c.view().overlay_visible);
        let fx = r.key(Key::Back);
        assert_eq!(fx, [Effect::Exit]);
        assert_eq!(r.calls().last(), Some(&Call::Stop));
    }

    #[test]
    fn menu_short_cycles_subtitles_and_long_reloads() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.c.handle(Input::Tracks {
            audio: Vec::new(),
            subs: vec![sub(1, None, Some("eng"))],
            aid: None,
            sid: None,
        });
        r.key(Key::Menu);
        r.c.handle(Input::KeyUp(Key::Menu));
        assert_eq!(r.calls(), [Call::Subtitle(Some(1))]);
        assert_eq!(r.hint().as_deref(), Some("SUBTITLES · ENGLISH"));
        r.clear();
        r.key(Key::Menu);
        r.clock.advance(300);
        r.c.handle(Input::Key {
            key: Key::Menu,
            repeat: 1,
        });
        assert!(r.calls().is_empty());
        r.clock.advance(300);
        let fx = r.c.handle(Input::Key {
            key: Key::Menu,
            repeat: 2,
        });
        assert!(fx.iter().any(|e| matches!(e, Effect::Sniff { .. })));
        r.c.handle(Input::KeyUp(Key::Menu));
        assert!(!r.calls().iter().any(|c| matches!(c, Call::Subtitle(_))));
    }

    #[test]
    fn ctrl_r_reloads_at_the_current_position() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        let fx = r.key(Key::Reload);
        assert_eq!(r.c.start_at(), 100);
        assert!(fx.iter().any(
            |e| matches!(e, Effect::Sniff { page_url, .. } if page_url.ends_with("startAt=100"))
        ));
        assert_eq!(r.c.phase(), Phase::Loading);
    }

    // ---- tracks, volume, quality ----

    #[test]
    fn subtitles_cycle_off_then_each_track() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.c.handle(Input::Button(Button::Subtitles));
        assert_eq!(r.hint().as_deref(), Some("SUBTITLES OFF"));
        assert!(r.calls().is_empty());
        r.c.handle(Input::Tracks {
            audio: Vec::new(),
            subs: vec![sub(3, Some("SDH"), Some("eng")), sub(4, None, None)],
            aid: None,
            sid: None,
        });
        assert!(r.c.view().subtitles_available);
        r.c.handle(Input::Button(Button::Subtitles));
        assert_eq!(r.hint().as_deref(), Some("SUBTITLES · SDH"));
        r.c.handle(Input::Button(Button::Subtitles));
        assert_eq!(r.hint().as_deref(), Some("SUBTITLES · SUBTITLES"));
        r.c.handle(Input::Button(Button::Subtitles));
        assert_eq!(r.hint().as_deref(), Some("SUBTITLES OFF"));
        assert_eq!(
            r.calls(),
            [
                Call::Subtitle(Some(3)),
                Call::Subtitle(Some(4)),
                Call::Subtitle(None)
            ]
        );
    }

    #[test]
    fn audio_pick_selects_and_saves_the_language() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.c.handle(Input::Tracks {
            audio: vec![
                audio(&[1, 2], "eng", "eac3", 6),
                audio(&[3], "spa", "ac3", 6),
            ],
            subs: Vec::new(),
            aid: Some(2),
            sid: None,
        });
        assert!(r.c.view().audio_available);
        r.c.handle(Input::MouseMove);
        r.c.handle(Input::Button(Button::Audio));
        let panel = r.c.view().tracks;
        assert_eq!(
            panel,
            Some(TracksPanel {
                heading: "AUDIO".to_owned(),
                labels: vec![
                    "English · E-AC3 · 5.1".to_owned(),
                    "Spanish · AC3 · 5.1".to_owned()
                ],
                selected: 0,
            })
        );
        let fx = r.c.handle(Input::PickTrack(1));
        assert_eq!(fx, [Effect::SaveAudioLanguage("es".to_owned())]);
        assert_eq!(r.calls(), [Call::Audio(vec![3])]);
        assert_eq!(r.hint().as_deref(), Some("AUDIO · SPANISH · AC3 · 5.1"));
        assert!(r.c.view().tracks.is_none());
    }

    #[test]
    fn volume_steps_by_five_within_bounds() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        for _ in 0..8 {
            r.c.handle(Input::Button(Button::VolumeUp));
        }
        assert_eq!(r.c.view().volume, 130);
        assert_eq!(r.hint().as_deref(), Some("VOLUME · 130%"));
        r.c.handle(Input::Button(Button::VolumeDown));
        assert_eq!(r.hint().as_deref(), Some("VOLUME · 125%"));
        for _ in 0..30 {
            r.c.handle(Input::Button(Button::VolumeDown));
        }
        assert_eq!(r.c.view().volume, 0);
        assert_eq!(r.calls().last(), Some(&Call::Volume(0)));
    }

    #[test]
    fn quality_cycle_saves_and_restarts_at_the_position() {
        let key = EpisodeKey::movie(550);
        let mut r = rig(movie_meta(), 0);
        r.prints
            .entries
            .borrow_mut()
            .extend([print(key, "2160p DV", "hevc"), print(key, "1080p", "h264")]);
        r.native_library();
        assert!(r.c.view().quality_available);
        r.c.handle(Input::Playback(Playback {
            time: 321.0,
            duration: 3000.0,
            paused: false,
            ended: false,
        }));
        let fx = r.c.handle(Input::Button(Button::Quality));
        assert_eq!(fx[0], Effect::SaveQuality("1080p".to_owned()));
        let Effect::PrepareLibrary { seq, entry } = &fx[1] else {
            panic!("expected prepare, got {fx:?}");
        };
        assert_eq!(entry.quality, "1080p");
        assert_eq!(r.hint().as_deref(), Some("QUALITY · 1080P H264"));
        assert_eq!(r.c.phase(), Phase::Loading);
        let seq = *seq;
        r.c.handle(Input::LibraryPrepared {
            seq,
            subtitle: None,
        });
        assert!(r
            .calls()
            .contains(&Call::LoadLibrary("1080p h264".to_owned(), None, 321)));
        r.clear();
        r.c.handle(Input::FileLoaded { duration: 3000.0 });
        assert_eq!(r.calls(), [Call::SeekTo(321.0)]);
    }

    #[test]
    fn quality_is_unavailable_with_one_print_or_on_streams() {
        let mut r = rig(movie_meta(), 0);
        r.prints
            .entries
            .borrow_mut()
            .push(print(EpisodeKey::movie(550), "1080p", "h264"));
        r.native_library();
        assert!(!r.c.view().quality_available);
        assert!(r.c.handle(Input::Button(Button::Quality)).is_empty());
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        assert!(!r.c.view().quality_available);
    }

    // ---- progress ----

    #[test]
    fn progress_is_sampled_every_two_seconds_and_written_every_ten() {
        let mut r = rig(tv_meta(3), 0);
        r.native_url();
        let writes = |fx: &[Effect]| {
            fx.iter()
                .filter(|e| matches!(e, Effect::SaveProgress(_)))
                .count()
        };
        let first = r.tick(PROGRESS_TICK_MS);
        assert_eq!(writes(&first), 1);
        let Some(Effect::SaveProgress(rec)) = first.first() else {
            panic!("expected a record");
        };
        assert_eq!(
            (rec.id, rec.media, rec.watched, rec.duration),
            (1399, MediaType::Tv, 100, 3000)
        );
        assert_eq!(
            (rec.season, rec.episode, rec.poster.as_deref()),
            (1, 3, Some("/p.jpg"))
        );
        let mut total = 0;
        for _ in 0..4 {
            total += writes(&r.tick(PROGRESS_TICK_MS));
        }
        assert_eq!(total, 0);
        assert_eq!(writes(&r.tick(PROGRESS_TICK_MS)), 1);
    }

    #[test]
    fn progress_near_the_end_counts_as_ended() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.tick(PROGRESS_TICK_MS);
        r.c.handle(Input::Playback(Playback {
            time: 2999.5,
            duration: 3000.0,
            paused: false,
            ended: false,
        }));
        let fx = r.tick(PROGRESS_TICK_MS);
        assert!(matches!(
            fx.as_slice(),
            [Effect::SaveProgress(_), Effect::Exit]
        ));
    }

    // ---- end of playback and next ----

    #[test]
    fn movie_end_writes_and_exits() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        let fx = r.end();
        assert!(
            matches!(fx.as_slice(), [Effect::SaveProgress(rec), Effect::Exit] if rec.watched == 3000)
        );
    }

    #[test]
    fn tv_end_autoplays_the_next_episode() {
        let mut r = rig(tv_meta(3), 500);
        r.c.handle(Input::EpisodeCount(8));
        r.native_url();
        let fx = r.end();
        assert_eq!(r.c.meta().episode, 4);
        assert_eq!(r.c.start_at(), 0);
        assert_eq!(r.hint().as_deref(), Some("NEXT · S1 E4"));
        assert!(fx.iter().any(
            |e| matches!(e, Effect::Sniff { page_url, .. } if page_url.contains("/tv/1399/1/4?"))
        ));
    }

    #[test]
    fn tv_end_without_autoplay_offers_next_in_the_overlay() {
        let settings = Settings {
            autoplay_next: false,
            ..Settings::default()
        };
        let mut r = rig_with(tv_meta(3), 0, settings);
        r.native_url();
        r.c.handle(Input::EpisodeCount(8));
        r.end();
        let v = r.c.view();
        assert!(v.overlay_visible && v.next_available);
        assert_eq!(r.c.meta().episode, 3);
        let fx = r.c.handle(Input::Button(Button::Next));
        assert_eq!(r.c.meta().episode, 4);
        assert!(fx.iter().any(|e| matches!(e, Effect::Sniff { .. })));
    }

    #[test]
    fn tv_end_in_the_page_player_shows_a_hint() {
        let settings = Settings {
            autoplay_next: false,
            ..Settings::default()
        };
        let mut r = rig_with(tv_meta(3), 0, settings);
        r.native_url();
        r.c.handle(Input::EngineError("x".to_owned()));
        r.c.handle(Input::EpisodeCount(8));
        r.end();
        assert_eq!(r.hint().as_deref(), Some("EPISODE FINISHED"));
        assert_eq!(r.c.phase(), Phase::Page);
    }

    #[test]
    fn tv_end_of_the_last_episode_exits() {
        let mut r = rig(tv_meta(8), 0);
        r.native_url();
        r.c.handle(Input::EpisodeCount(8));
        assert!(!r.c.view().next_available);
        assert!(r.end().contains(&Effect::Exit));
    }

    #[test]
    fn tv_end_with_an_unknown_count_waits_for_it() {
        let mut r = rig(tv_meta(3), 0);
        r.native_url();
        let fx = r.end();
        assert!(!fx.contains(&Effect::Exit));
        assert_eq!(r.c.meta().episode, 3);
        r.c.handle(Input::EpisodeCount(4));
        assert_eq!(r.c.meta().episode, 4);
        // a failed count lookup ends the player, as on Android
        let mut r = rig(tv_meta(3), 0);
        r.native_url();
        r.end();
        assert_eq!(r.c.handle(Input::EpisodeCount(0)), [Effect::Exit]);
    }

    #[test]
    fn next_needs_a_later_episode_in_the_season() {
        let mut r = rig(tv_meta(8), 0);
        r.c.handle(Input::EpisodeCount(8));
        r.native_url();
        assert!(r.c.handle(Input::Button(Button::Next)).is_empty());
        assert!(r.key(Key::Next).is_empty());
        assert_eq!(r.c.meta().episode, 8);
        let mut r = rig(tv_meta(7), 0);
        r.c.handle(Input::EpisodeCount(8));
        r.native_url();
        assert!(r.c.view().next_available);
        r.key(Key::Next);
        assert_eq!(r.c.meta().episode, 8);
    }

    #[test]
    fn next_episode_retries_the_library() {
        let mut r = rig(tv_meta(1), 0);
        r.c.handle(Input::EpisodeCount(2));
        r.prints.entries.borrow_mut().extend([
            print(EpisodeKey::episode(1399, 1, 1), "1080p", "h264"),
            print(EpisodeKey::episode(1399, 1, 2), "1080p", "h264"),
        ]);
        r.native_library();
        // the library fails for episode 1, so it plays the sniffed stream
        let fx = r.c.handle(Input::EngineError("x".to_owned()));
        r.sniff_ok(Rig::seq(&fx).unwrap_or(0));
        r.c.handle(Input::FirstFrame);
        assert_eq!(r.c.phase(), Phase::Native);
        // episode 2 tries the library again
        let fx = r.key(Key::Next);
        assert_eq!(r.c.meta().episode, 2);
        assert!(fx
            .iter()
            .any(|e| matches!(e, Effect::PrepareLibrary { entry, .. } if entry.key.episode == 2)));
    }

    // ---- page player ----

    fn page_rig() -> Rig {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.c.handle(Input::EngineError("x".to_owned()));
        r.clear();
        r
    }

    #[test]
    fn page_ready_applies_start_and_speed() {
        let mut r = rig_with(
            movie_meta(),
            0,
            Settings {
                playback_speed: 1.5,
                ..Settings::default()
            },
        );
        r.native_url();
        r.c.handle(Input::EngineError("x".to_owned()));
        r.clear();
        r.c.handle(Input::PageReady);
        assert_eq!(
            r.calls(),
            [
                Call::Page(PageAction::ApplyStart(100)),
                Call::Page(PageAction::ApplySpeed(1.5))
            ]
        );
    }

    #[test]
    fn page_keys_map_to_page_actions() {
        let mut r = page_rig();
        r.key(Key::Center);
        r.c.handle(Input::KeyUp(Key::Center));
        r.key(Key::Pause);
        r.key(Key::Left);
        r.key(Key::FastForward);
        assert_eq!(
            r.calls(),
            [
                Call::Page(PageAction::Space),
                Call::Page(PageAction::Pause),
                Call::Page(PageAction::ArrowLeft),
                Call::Page(PageAction::Seek(30))
            ]
        );
    }

    #[test]
    fn page_long_center_enters_navigation_and_back_leaves_it() {
        let mut r = page_rig();
        r.key(Key::Center);
        r.clock.advance(LONG_PRESS_MS);
        r.c.handle(Input::Key {
            key: Key::Center,
            repeat: 1,
        });
        r.c.handle(Input::KeyUp(Key::Center));
        assert!(r.c.view().nav_mode);
        assert_eq!(
            r.hint().as_deref(),
            Some("NAVIGATE · CENTER SELECT · BACK EXIT")
        );
        r.key(Key::Down);
        r.key(Key::Center);
        r.c.handle(Input::KeyUp(Key::Center));
        assert!(r.key(Key::Back).is_empty());
        r.c.handle(Input::PagePanelClosed(true));
        assert!(r.c.view().nav_mode);
        r.key(Key::Back);
        r.c.handle(Input::PagePanelClosed(false));
        assert!(!r.c.view().nav_mode);
        assert_eq!(
            r.calls(),
            [
                Call::Page(PageAction::EnterNav),
                Call::Page(PageAction::Nav(Direction::Down)),
                Call::Page(PageAction::Activate),
                Call::Page(PageAction::ClosePanel),
                Call::Page(PageAction::ClosePanel),
                Call::Page(PageAction::ExitNav)
            ]
        );
        assert_eq!(r.key(Key::Back), [Effect::Exit]);
    }

    #[test]
    fn page_menu_opens_settings() {
        let mut r = page_rig();
        r.key(Key::Menu);
        r.c.handle(Input::KeyUp(Key::Menu));
        assert_eq!(
            r.calls(),
            [
                Call::Page(PageAction::EnterNav),
                Call::Page(PageAction::OpenSettings)
            ]
        );
    }

    #[test]
    fn page_load_failure_reloads_then_fails() {
        let mut r = page_rig();
        r.c.handle(Input::PageFailed);
        assert!(matches!(r.calls().last(), Some(Call::PageLoad(_))));
        r.c.handle(Input::PageFailed);
        assert_eq!(r.c.phase(), Phase::Failed);
        assert_eq!(r.calls().last(), Some(&Call::PageClose));
    }

    #[test]
    fn hints_expire() {
        let mut r = rig(movie_meta(), 0);
        r.native_url();
        r.key(Key::Right);
        r.tick(HINT_MS - 1);
        assert!(r.hint().is_some());
        r.tick(1);
        assert!(r.hint().is_none());
    }

    #[test]
    fn overlay_texts() {
        let mut r = rig(tv_meta(3), 0);
        r.native_url();
        let v = r.c.view();
        assert_eq!(
            (v.eyebrow.as_str(), v.position.as_str(), v.duration.as_str()),
            ("S1 · E3", "01:40", "50:00")
        );
        assert_eq!(rig(movie_meta(), 0).c.view().eyebrow, "MOVIE");
        assert_eq!(rig(movie_meta(), 0).c.view().duration, "--:--");
    }
}
