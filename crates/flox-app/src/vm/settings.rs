//! The Settings screen: every Android row (PLAYBACK, AUDIO, INTERFACE, LIBRARY,
//! ABOUT) plus UI SCALE, ACCOUNT (TMDB key, Telegram API id and hash, sign in or
//! out) and TOOLS (ffmpeg and yt-dlp paths).
//!
//! CENTER opens a single-choice dialog, toggles, edits text or asks to confirm;
//! LEFT/RIGHT cycle a choice or flip a toggle (Android `SettingsActivity`). Values
//! are stamped in mono uppercase, so every string here is already uppercased.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use flox_core::lang;
use flox_core::settings::{
    build_defaults, AspectMode, LibrarySort, ResumeMode, Settings, SubtitleSize,
    CONTINUE_WATCHING_LIMITS, FINISHED_THRESHOLDS, LANGUAGES, LOUDNESS_GAIN_RANGE,
    OVERLAY_HIDE_OPTIONS, PLAYBACK_SPEEDS, QUALITY_OPTIONS, SEEK_STEPS, UI_SCALES,
};
use flox_core::tools::{self, Tool};
use flox_td::auth::AuthState;

use crate::focus::{Zone, ZoneId};
use crate::router::Route;
use crate::vm::home::TelegramStatus;

/// The settings rows, one vertical list. Mirrors `SettingsZones.list`.
pub const LIST: ZoneId = ZoneId(60);
/// A choice dialog's options. Mirrors `SettingsZones.options`.
pub const OPTIONS: ZoneId = ZoneId(61);
/// A text dialog's field. Mirrors `SettingsZones.field`.
pub const FIELD: ZoneId = ZoneId(62);
/// A dialog's buttons: CANCEL (0), then OK (1) when there is one. Mirrors
/// `SettingsZones.buttons`.
pub const BUTTONS: ZoneId = ZoneId(63);

/// The dialog button indices.
pub const CANCEL: usize = 0;
pub const OK: usize = 1;

pub const EYEBROW: &str = "FLOX";
pub const TITLE: &str = "Settings";
pub const HINT: &str = "CENTER CHANGE · LEFT RIGHT CYCLE · BACK EXIT";

pub const ON: &str = "ON";
pub const OFF: &str = "OFF";
pub const HIGHEST: &str = "HIGHEST";
pub const ANY: &str = "ANY";
pub const DEVICE_LANGUAGE: &str = "DEVICE LANGUAGE";
pub const CLEARED: &str = "CLEARED";
pub const NOT_SET: &str = "NOT SET";
pub const BUILT_IN: &str = "BUILT IN";
pub const NOT_FOUND: &str = "NOT FOUND";
pub const SIGNED_IN: &str = "SIGNED IN";
pub const SIGNED_OUT: &str = "SIGNED OUT";
pub const CONNECTING: &str = "CONNECTING";
pub const SIGNING_OUT: &str = "SIGNING OUT";
pub const NOT_CONFIGURED: &str = "NOT CONFIGURED";
pub const TDLIB_NOT_FOUND: &str = "TDLIB NOT FOUND";
pub const RESTART_TO_APPLY: &str = "RESTART FLOX TO APPLY";
pub const API_ID_NOT_A_NUMBER: &str = "THE API ID IS A WHOLE NUMBER";

/// One settings row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowId {
    Quality,
    AudioLanguage,
    Subtitles,
    SubtitleLanguage,
    SubtitleSize,
    Speed,
    Aspect,
    Autoplay,
    SeekStep,
    Resume,
    Finished,
    Loudness,
    LoudnessGain,
    OverlayHide,
    UiScale,
    Sort,
    Channel,
    ContinueLimit,
    ClearHistory,
    TmdbKey,
    TelegramApiId,
    TelegramApiHash,
    Account,
    FfmpegPath,
    YtDlpPath,
    Version,
}

/// The sections in screen order, each with its rows.
pub const SECTIONS: &[(&str, &[RowId])] = &[
    (
        "PLAYBACK",
        &[
            RowId::Quality,
            RowId::AudioLanguage,
            RowId::Subtitles,
            RowId::SubtitleLanguage,
            RowId::SubtitleSize,
            RowId::Speed,
            RowId::Aspect,
            RowId::Autoplay,
            RowId::SeekStep,
            RowId::Resume,
            RowId::Finished,
        ],
    ),
    ("AUDIO", &[RowId::Loudness, RowId::LoudnessGain]),
    ("INTERFACE", &[RowId::OverlayHide, RowId::UiScale]),
    (
        "LIBRARY",
        &[
            RowId::Sort,
            RowId::Channel,
            RowId::ContinueLimit,
            RowId::ClearHistory,
        ],
    ),
    (
        "ACCOUNT",
        &[
            RowId::TmdbKey,
            RowId::TelegramApiId,
            RowId::TelegramApiHash,
            RowId::Account,
        ],
    ),
    ("TOOLS", &[RowId::FfmpegPath, RowId::YtDlpPath]),
    ("ABOUT", &[RowId::Version]),
];

/// Every row in screen order; a row's position is its focus index.
pub fn row_ids() -> Vec<RowId> {
    SECTIONS
        .iter()
        .flat_map(|(_, rows)| rows.iter().copied())
        .collect()
}

/// Where ffmpeg and yt-dlp resolve to (override, app folder, then `PATH`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolPaths {
    pub ffmpeg: Option<PathBuf>,
    pub ytdlp: Option<PathBuf>,
}

/// Resolves the tools the TOOLS section shows.
pub fn resolve_tools(s: &Settings, app_dir: &Path, path_env: Option<&OsStr>) -> ToolPaths {
    ToolPaths {
        ffmpeg: tools::resolve(Tool::Ffmpeg, app_dir, path_env, s.ffmpeg_path.as_deref()),
        ytdlp: tools::resolve(Tool::YtDlp, app_dir, path_env, s.ytdlp_path.as_deref()),
    }
}

/// What the rows need besides the settings themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub status: TelegramStatus,
    /// Every quality uploaded to the library (`"2160p DV"`), in any order.
    pub library_qualities: Vec<String>,
    pub tools: ToolPaths,
    /// CLEAR WATCH HISTORY ran while the screen was open.
    pub history_cleared: bool,
    /// Telegram credentials changed and the running client still uses the old ones.
    pub restart_pending: bool,
    pub version: String,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            status: TelegramStatus::NotConfigured,
            library_qualities: Vec::new(),
            tools: ToolPaths::default(),
            history_cleared: false,
            restart_pending: false,
            version: flox_core::VERSION.to_owned(),
        }
    }
}

/// A row as drawn: the section header above it (first row of a section only), the
/// label and the mono value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: RowId,
    pub header: &'static str,
    pub label: &'static str,
    pub value: String,
}

/// Every row with its current value.
pub fn rows(s: &Settings, ctx: &Context) -> Vec<Row> {
    let mut out = Vec::new();
    for (header, ids) in SECTIONS {
        for (i, id) in ids.iter().enumerate() {
            out.push(Row {
                id: *id,
                header: if i == 0 { header } else { "" },
                label: label(*id, ctx),
                value: value(*id, s, ctx),
            });
        }
    }
    out
}

/// The row's label (Caption, sentence case as on Android).
pub fn label(id: RowId, ctx: &Context) -> &'static str {
    match id {
        RowId::Quality => "Default quality",
        RowId::AudioLanguage => "Preferred audio language",
        RowId::Subtitles => "Subtitles by default",
        RowId::SubtitleLanguage => "Subtitle language",
        RowId::SubtitleSize => "Subtitle size",
        RowId::Speed => "Playback speed",
        RowId::Aspect => "Aspect mode",
        RowId::Autoplay => "Autoplay next episode",
        RowId::SeekStep => "Seek step",
        RowId::Resume => "Resume mode",
        RowId::Finished => "Mark finished at",
        RowId::Loudness => "Loudness boost",
        RowId::LoudnessGain => "Loudness gain",
        RowId::OverlayHide => "Hide player controls after",
        RowId::UiScale => "UI scale",
        RowId::Sort => "Sort order",
        RowId::Channel => "Telegram channel",
        RowId::ContinueLimit => "Continue watching limit",
        RowId::ClearHistory => "Clear watch history",
        RowId::TmdbKey => "TMDB API key",
        RowId::TelegramApiId => "Telegram API id",
        RowId::TelegramApiHash => "Telegram API hash",
        RowId::Account => {
            if ctx.status.ready() {
                "Sign out of Telegram"
            } else {
                "Sign in to Telegram"
            }
        }
        RowId::FfmpegPath => "FFmpeg path",
        RowId::YtDlpPath => "yt-dlp path",
        RowId::Version => "Version",
    }
}

/// A choice-row value.
#[derive(Clone, Debug, PartialEq)]
pub enum Opt {
    Quality(Option<String>),
    AudioLanguage(Option<String>),
    SubtitleLanguage(Option<String>),
    SubtitleSize(SubtitleSize),
    Speed(f32),
    Aspect(AspectMode),
    SeekStep(u32),
    Resume(ResumeMode),
    Finished(u32),
    Gain(f32),
    OverlayHide(u32),
    UiScale(f32),
    Sort(LibrarySort),
    ContinueLimit(u32),
}

/// The Default quality choices: HIGHEST, then the common qualities merged with the
/// library's, tallest first (a stable sort, so `2160p` stays before `2160p DV`), then
/// the saved quality when it is in neither.
pub fn qualities(library: &[String], current: Option<&str>) -> Vec<Option<String>> {
    let mut known: Vec<String> = Vec::new();
    for q in QUALITY_OPTIONS
        .iter()
        .map(|q| (*q).to_owned())
        .chain(library.iter().map(|q| q.trim().to_owned()))
    {
        if !q.is_empty() && !known.contains(&q) {
            known.push(q);
        }
    }
    known.sort_by_key(|q| std::cmp::Reverse(height(q)));
    if let Some(c) = current.filter(|c| !known.iter().any(|k| k == c)) {
        known.push(c.to_owned());
    }
    std::iter::once(None)
        .chain(known.into_iter().map(Some))
        .collect()
}

/// The number before the first `p` (`"2160p DV"` → 2160), else 0.
fn height(quality: &str) -> u32 {
    quality
        .split('p')
        .next()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

fn languages() -> impl Iterator<Item = Option<String>> {
    std::iter::once(None).chain(LANGUAGES.iter().map(|l| Some((*l).to_owned())))
}

/// The row's choices, in dialog order. Empty for rows that are not choices.
pub fn options(id: RowId, s: &Settings, ctx: &Context) -> Vec<Opt> {
    match id {
        RowId::Quality => qualities(&ctx.library_qualities, s.quality.as_deref())
            .into_iter()
            .map(Opt::Quality)
            .collect(),
        RowId::AudioLanguage => languages().map(Opt::AudioLanguage).collect(),
        RowId::SubtitleLanguage => languages().map(Opt::SubtitleLanguage).collect(),
        RowId::SubtitleSize => [
            SubtitleSize::Small,
            SubtitleSize::Normal,
            SubtitleSize::Large,
        ]
        .into_iter()
        .map(Opt::SubtitleSize)
        .collect(),
        RowId::Speed => PLAYBACK_SPEEDS.iter().copied().map(Opt::Speed).collect(),
        RowId::Aspect => [AspectMode::Fit, AspectMode::Fill, AspectMode::Zoom]
            .into_iter()
            .map(Opt::Aspect)
            .collect(),
        RowId::SeekStep => SEEK_STEPS.iter().copied().map(Opt::SeekStep).collect(),
        RowId::Resume => [ResumeMode::Always, ResumeMode::Ask, ResumeMode::Never]
            .into_iter()
            .map(Opt::Resume)
            .collect(),
        RowId::Finished => FINISHED_THRESHOLDS
            .iter()
            .copied()
            .map(Opt::Finished)
            .collect(),
        RowId::LoudnessGain => {
            let (min, max) = LOUDNESS_GAIN_RANGE;
            // Whole dB steps, +0 to +12.
            (min as u32..=max as u32)
                .map(|db| Opt::Gain(db as f32))
                .collect()
        }
        RowId::OverlayHide => OVERLAY_HIDE_OPTIONS
            .iter()
            .copied()
            .map(Opt::OverlayHide)
            .collect(),
        RowId::UiScale => UI_SCALES.iter().copied().map(Opt::UiScale).collect(),
        RowId::Sort => [
            LibrarySort::Title,
            LibrarySort::DateAdded,
            LibrarySort::Size,
        ]
        .into_iter()
        .map(Opt::Sort)
        .collect(),
        RowId::ContinueLimit => CONTINUE_WATCHING_LIMITS
            .iter()
            .copied()
            .map(Opt::ContinueLimit)
            .collect(),
        _ => Vec::new(),
    }
}

/// The row's current choice, for choice rows.
pub fn current(id: RowId, s: &Settings) -> Option<Opt> {
    Some(match id {
        RowId::Quality => Opt::Quality(s.quality.clone()),
        RowId::AudioLanguage => Opt::AudioLanguage(s.audio_language.clone()),
        RowId::SubtitleLanguage => Opt::SubtitleLanguage(s.subtitle_language.clone()),
        RowId::SubtitleSize => Opt::SubtitleSize(s.subtitle_size),
        RowId::Speed => Opt::Speed(s.playback_speed),
        RowId::Aspect => Opt::Aspect(s.aspect_mode),
        RowId::SeekStep => Opt::SeekStep(s.seek_step_seconds),
        RowId::Resume => Opt::Resume(s.resume_mode),
        RowId::Finished => Opt::Finished(s.finished_threshold_percent),
        RowId::LoudnessGain => Opt::Gain(s.loudness_gain_db.round()),
        RowId::OverlayHide => Opt::OverlayHide(s.overlay_hide_ms),
        RowId::UiScale => Opt::UiScale(s.ui_scale),
        RowId::Sort => Opt::Sort(s.library_sort),
        RowId::ContinueLimit => Opt::ContinueLimit(s.continue_watching_limit),
        _ => return None,
    })
}

/// The index of the current choice in `options` (0 when it is not listed, as on Android).
pub fn current_index(options: &[Opt], current: Option<&Opt>) -> usize {
    current
        .and_then(|c| options.iter().position(|o| o == c))
        .unwrap_or(0)
}

fn language_label(code: Option<&str>, none: &str) -> String {
    match code {
        None => none.to_owned(),
        Some(c) => lang::display_name(c).unwrap_or(c).to_uppercase(),
    }
}

/// `1×`, `0.75×`, `1.5×`.
pub fn speed_label(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}×", v as i32)
    } else {
        format!("{v}×")
    }
}

/// How a choice reads in the row and the dialog.
pub fn option_label(o: &Opt) -> String {
    match o {
        Opt::Quality(q) => q
            .as_deref()
            .map_or_else(|| HIGHEST.to_owned(), str::to_uppercase),
        Opt::AudioLanguage(l) => language_label(l.as_deref(), ANY),
        Opt::SubtitleLanguage(l) => language_label(l.as_deref(), DEVICE_LANGUAGE),
        Opt::SubtitleSize(z) => match z {
            SubtitleSize::Small => "SMALL",
            SubtitleSize::Normal => "NORMAL",
            SubtitleSize::Large => "LARGE",
        }
        .to_owned(),
        Opt::Speed(v) => speed_label(*v),
        Opt::Aspect(a) => match a {
            AspectMode::Fit => "FIT",
            AspectMode::Fill => "FILL",
            AspectMode::Zoom => "ZOOM",
        }
        .to_owned(),
        Opt::SeekStep(s) => format!("{s} S"),
        Opt::Resume(r) => match r {
            ResumeMode::Always => "ALWAYS",
            ResumeMode::Ask => "ASK",
            ResumeMode::Never => "NEVER",
        }
        .to_owned(),
        Opt::Finished(p) => format!("{p}%"),
        Opt::Gain(db) => format!("+{} DB", db.round() as i32),
        Opt::OverlayHide(ms) => format!("{} S", ms / 1000),
        Opt::UiScale(f) => format!("{}%", (f * 100.0).round() as i32),
        Opt::Sort(s) => match s {
            LibrarySort::Title => "TITLE",
            LibrarySort::DateAdded => "DATE ADDED",
            LibrarySort::Size => "SIZE",
        }
        .to_owned(),
        Opt::ContinueLimit(n) => n.to_string(),
    }
}

/// Stores a choice.
pub fn set(s: &mut Settings, o: &Opt) {
    match o.clone() {
        Opt::Quality(q) => s.quality = q,
        Opt::AudioLanguage(l) => s.audio_language = l,
        Opt::SubtitleLanguage(l) => s.subtitle_language = l,
        Opt::SubtitleSize(z) => s.subtitle_size = z,
        Opt::Speed(v) => s.playback_speed = v,
        Opt::Aspect(a) => s.aspect_mode = a,
        Opt::SeekStep(v) => s.seek_step_seconds = v,
        Opt::Resume(r) => s.resume_mode = r,
        Opt::Finished(v) => s.finished_threshold_percent = v,
        Opt::Gain(db) => s.loudness_gain_db = db,
        Opt::OverlayHide(v) => s.overlay_hide_ms = v,
        Opt::UiScale(f) => s.ui_scale = f,
        Opt::Sort(v) => s.library_sort = v,
        Opt::ContinueLimit(v) => s.continue_watching_limit = v,
    }
}

fn toggle_value(id: RowId, s: &Settings) -> Option<bool> {
    match id {
        RowId::Subtitles => Some(s.subtitles_enabled),
        RowId::Autoplay => Some(s.autoplay_next),
        RowId::Loudness => Some(s.loudness_boost),
        _ => None,
    }
}

/// Flips a toggle row. False for other rows.
pub fn toggle(id: RowId, s: &mut Settings) -> bool {
    match id {
        RowId::Subtitles => s.subtitles_enabled = !s.subtitles_enabled,
        RowId::Autoplay => s.autoplay_next = !s.autoplay_next,
        RowId::Loudness => s.loudness_boost = !s.loudness_boost,
        _ => return false,
    }
    true
}

/// LEFT (`delta = -1`) or RIGHT (`+1`): a toggle flips, a choice moves to the
/// neighbouring option and wraps around. False when the row does neither.
pub fn cycle(id: RowId, s: &mut Settings, ctx: &Context, delta: isize) -> bool {
    if toggle(id, s) {
        return true;
    }
    let opts = options(id, s, ctx);
    if opts.is_empty() {
        return false;
    }
    let index = current_index(&opts, current(id, s).as_ref());
    let len = opts.len() as isize;
    let next = (index as isize + delta).rem_euclid(len) as usize;
    set(s, &opts[next]);
    true
}

/// `••••••••` plus the last four characters of a long secret.
pub fn mask(secret: &str) -> String {
    let chars: Vec<char> = secret.trim().chars().collect();
    let mut out = "•".repeat(8);
    if chars.len() >= 12 {
        out.extend(&chars[chars.len() - 4..]);
    }
    out.to_uppercase()
}

fn credential(saved: Option<String>, built_in: bool, secret: bool) -> String {
    match saved {
        Some(v) if secret => mask(&v),
        Some(v) => v.to_uppercase(),
        None if built_in => BUILT_IN.to_owned(),
        None => NOT_SET.to_owned(),
    }
}

fn tool_value(path: Option<&PathBuf>) -> String {
    path.map_or_else(|| NOT_FOUND.to_owned(), |p| p.display().to_string())
}

/// The sign-in row's value.
pub fn account_value(ctx: &Context) -> &'static str {
    if ctx.restart_pending {
        return RESTART_TO_APPLY;
    }
    match &ctx.status {
        TelegramStatus::NotConfigured => NOT_CONFIGURED,
        TelegramStatus::Unavailable => TDLIB_NOT_FOUND,
        TelegramStatus::Auth(AuthState::Ready { .. }) => SIGNED_IN,
        TelegramStatus::Auth(AuthState::Idle | AuthState::Connecting) => CONNECTING,
        TelegramStatus::Auth(AuthState::LoggingOut) => SIGNING_OUT,
        TelegramStatus::Auth(_) => SIGNED_OUT,
    }
}

/// The row's value as stamped on the right.
pub fn value(id: RowId, s: &Settings, ctx: &Context) -> String {
    if let Some(on) = toggle_value(id, s) {
        return if on { ON } else { OFF }.to_owned();
    }
    if let Some(c) = current(id, s) {
        return option_label(&c);
    }
    let built = build_defaults();
    match id {
        RowId::Channel => s.telegram_channel.to_uppercase(),
        RowId::ClearHistory => {
            if ctx.history_cleared {
                CLEARED.to_owned()
            } else {
                String::new()
            }
        }
        RowId::TmdbKey => credential(s.tmdb_api_key.clone(), built.tmdb_api_key.is_some(), true),
        RowId::TelegramApiId => credential(
            s.telegram_api_id.map(|id| id.to_string()),
            built.telegram_api_id.is_some(),
            false,
        ),
        RowId::TelegramApiHash => credential(
            s.telegram_api_hash.clone(),
            built.telegram_api_hash.is_some(),
            true,
        ),
        RowId::Account => account_value(ctx).to_owned(),
        RowId::FfmpegPath => tool_value(ctx.tools.ffmpeg.as_ref()),
        RowId::YtDlpPath => tool_value(ctx.tools.ytdlp.as_ref()),
        RowId::Version => ctx.version.clone(),
        _ => String::new(),
    }
}

/// A text setting edited in a dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextField {
    Channel,
    TmdbKey,
    TelegramApiId,
    TelegramApiHash,
    FfmpegPath,
    YtDlpPath,
}

/// What a confirm dialog asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Confirm {
    ClearHistory,
    SignOut,
}

/// The confirm dialog's title and message.
pub fn confirm_text(c: Confirm) -> (&'static str, &'static str) {
    match c {
        Confirm::ClearHistory => (
            "Clear watch history",
            "Remove every title from continue watching and forget saved positions?",
        ),
        Confirm::SignOut => (
            "Sign out of Telegram",
            "Sign out of Telegram on this device? The library will be hidden until you connect again.",
        ),
    }
}

/// What CENTER on a row does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// A toggle row: flip it.
    Toggle,
    /// Open the single-choice dialog.
    Choose {
        title: &'static str,
        options: Vec<Opt>,
        current: usize,
    },
    /// Open the text dialog.
    Edit(TextField),
    Confirm(Confirm),
    Open(Route),
    Nothing,
}

/// CENTER on row `id`.
pub fn action(id: RowId, s: &Settings, ctx: &Context) -> Action {
    if toggle_value(id, s).is_some() {
        return Action::Toggle;
    }
    let opts = options(id, s, ctx);
    if !opts.is_empty() {
        let current = current_index(&opts, current(id, s).as_ref());
        return Action::Choose {
            title: label(id, ctx),
            options: opts,
            current,
        };
    }
    match id {
        RowId::Channel => Action::Edit(TextField::Channel),
        RowId::TmdbKey => Action::Edit(TextField::TmdbKey),
        RowId::TelegramApiId => Action::Edit(TextField::TelegramApiId),
        RowId::TelegramApiHash => Action::Edit(TextField::TelegramApiHash),
        RowId::FfmpegPath => Action::Edit(TextField::FfmpegPath),
        RowId::YtDlpPath => Action::Edit(TextField::YtDlpPath),
        RowId::ClearHistory => Action::Confirm(Confirm::ClearHistory),
        RowId::Account if ctx.status.ready() => Action::Confirm(Confirm::SignOut),
        RowId::Account => Action::Open(Route::Login),
        _ => Action::Nothing,
    }
}

/// A text dialog's contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub title: &'static str,
    pub message: String,
    pub text: String,
    pub placeholder: String,
    /// Typed characters are masked.
    pub secret: bool,
}

/// The dialog for `field`, prefilled with the saved value.
pub fn text_edit(field: TextField, s: &Settings) -> TextEdit {
    let path = |p: &Option<PathBuf>| {
        p.as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    };
    let tool_hint = |tool: Tool| {
        format!(
            "Full path to {}, or the folder that holds it. Leave empty to use the one next to Flox or on PATH.",
            tool.file_name()
        )
    };
    let telegram_hint =
        "Create an app at my.telegram.org/apps. Leave empty to use the built-in value.";
    match field {
        TextField::Channel => TextEdit {
            title: "Telegram channel",
            message: String::new(),
            text: s.telegram_channel.clone(),
            placeholder: "Channel name".into(),
            secret: false,
        },
        TextField::TmdbKey => TextEdit {
            title: "TMDB API key",
            message: "The v3 API key from themoviedb.org. Leave empty to use the built-in key."
                .into(),
            text: s.tmdb_api_key.clone().unwrap_or_default(),
            placeholder: "API key".into(),
            secret: true,
        },
        TextField::TelegramApiId => TextEdit {
            title: "Telegram API id",
            message: telegram_hint.into(),
            text: s
                .telegram_api_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            placeholder: "api_id".into(),
            secret: false,
        },
        TextField::TelegramApiHash => TextEdit {
            title: "Telegram API hash",
            message: telegram_hint.into(),
            text: s.telegram_api_hash.clone().unwrap_or_default(),
            placeholder: "api_hash".into(),
            secret: true,
        },
        TextField::FfmpegPath => TextEdit {
            title: "FFmpeg path",
            message: tool_hint(Tool::Ffmpeg),
            text: path(&s.ffmpeg_path),
            placeholder: Tool::Ffmpeg.file_name().into(),
            secret: false,
        },
        TextField::YtDlpPath => TextEdit {
            title: "yt-dlp path",
            message: tool_hint(Tool::YtDlp),
            text: path(&s.ytdlp_path),
            placeholder: Tool::YtDlp.file_name().into(),
            secret: false,
        },
    }
}

/// Removes surrounding whitespace and one pair of quotes (Explorer's "Copy as path").
fn unquote(text: &str) -> &str {
    let t = text.trim();
    t.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(t)
        .trim()
}

fn opt(text: &str) -> Option<String> {
    Some(text.trim().to_owned()).filter(|t| !t.is_empty())
}

/// Stores what was typed into `field`'s dialog. An empty value clears the setting
/// (the channel falls back to its default). The error is shown in the dialog.
pub fn apply_text(field: TextField, s: &mut Settings, text: &str) -> Result<(), &'static str> {
    match field {
        TextField::Channel => s.telegram_channel = text.trim().to_owned(),
        TextField::TmdbKey => s.tmdb_api_key = opt(text),
        TextField::TelegramApiId => {
            s.telegram_api_id = match opt(text) {
                None => None,
                Some(t) => Some(
                    t.parse::<i32>()
                        .ok()
                        .filter(|id| *id > 0)
                        .ok_or(API_ID_NOT_A_NUMBER)?,
                ),
            }
        }
        TextField::TelegramApiHash => s.telegram_api_hash = opt(text),
        TextField::FfmpegPath => s.ffmpeg_path = opt(unquote(text)).map(PathBuf::from),
        TextField::YtDlpPath => s.ytdlp_path = opt(unquote(text)).map(PathBuf::from),
    }
    Ok(())
}

/// True when the Telegram API id or hash in effect changed, so the TDLib client has
/// to be rebuilt with the new parameters.
pub fn telegram_restart_needed(previous: &Settings, next: &Settings) -> bool {
    previous.effective_telegram_api_id() != next.effective_telegram_api_id()
        || previous.effective_telegram_api_hash() != next.effective_telegram_api_hash()
}

/// The screen's zones.
pub fn zones(rows: usize) -> Vec<Zone> {
    vec![Zone::list(LIST, rows)]
}

/// A single-choice dialog: the options (entered at the current one), then CANCEL.
pub fn choice_zones(options: usize, current: usize) -> Vec<Zone> {
    vec![
        Zone::list(OPTIONS, options).prefer(current),
        Zone::row(BUTTONS, 1),
    ]
}

/// A text dialog: the field, then CANCEL and OK (entered at OK).
pub fn text_zones() -> Vec<Zone> {
    vec![Zone::row(FIELD, 1).text(), Zone::row(BUTTONS, 2).prefer(OK)]
}

/// A confirm dialog: CANCEL and OK, entered at CANCEL since both confirms are destructive.
pub fn confirm_zones() -> Vec<Zone> {
    vec![Zone::row(BUTTONS, 2).prefer(CANCEL)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context::default()
    }

    fn row(id: RowId) -> usize {
        row_ids()
            .iter()
            .position(|r| *r == id)
            .unwrap_or(usize::MAX)
    }

    #[test]
    fn every_android_row_and_section_is_present() {
        let rows = rows(&Settings::default(), &ctx());
        let headers: Vec<&str> = rows
            .iter()
            .map(|r| r.header)
            .filter(|h| !h.is_empty())
            .collect();
        assert_eq!(
            headers,
            [
                "PLAYBACK",
                "AUDIO",
                "INTERFACE",
                "LIBRARY",
                "ACCOUNT",
                "TOOLS",
                "ABOUT"
            ]
        );
        assert_eq!(rows.len(), 26);
        assert_eq!(rows[0].label, "Default quality");
        assert_eq!(rows[0].value, "HIGHEST");
        let version = &rows[rows.len() - 1];
        assert_eq!(
            (version.label, version.value.as_str()),
            ("Version", "1.0.0")
        );
    }

    #[test]
    fn default_values_read_like_android() {
        let s = Settings::default();
        let c = ctx();
        let v = |id| value(id, &s, &c);
        assert_eq!(v(RowId::AudioLanguage), "ANY");
        assert_eq!(v(RowId::Subtitles), "OFF");
        assert_eq!(v(RowId::SubtitleLanguage), "DEVICE LANGUAGE");
        assert_eq!(v(RowId::SubtitleSize), "NORMAL");
        assert_eq!(v(RowId::Speed), "1×");
        assert_eq!(v(RowId::Aspect), "FIT");
        assert_eq!(v(RowId::Autoplay), "ON");
        assert_eq!(v(RowId::SeekStep), "10 S");
        assert_eq!(v(RowId::Resume), "ALWAYS");
        assert_eq!(v(RowId::Finished), "95%");
        assert_eq!(v(RowId::Loudness), "ON");
        assert_eq!(v(RowId::LoudnessGain), "+8 DB");
        assert_eq!(v(RowId::OverlayHide), "4 S");
        assert_eq!(v(RowId::UiScale), "100%");
        assert_eq!(v(RowId::Sort), "DATE ADDED");
        assert_eq!(v(RowId::Channel), "FLOX LIBRARY");
        assert_eq!(v(RowId::ContinueLimit), "50");
        assert_eq!(v(RowId::ClearHistory), "");
        assert_eq!(v(RowId::Account), "NOT CONFIGURED");
        assert_eq!(v(RowId::FfmpegPath), "NOT FOUND");
    }

    #[test]
    fn speed_labels_drop_trailing_zeros() {
        let labels: Vec<String> = PLAYBACK_SPEEDS.iter().map(|v| speed_label(*v)).collect();
        assert_eq!(labels, ["0.75×", "1×", "1.25×", "1.5×", "2×"]);
    }

    #[test]
    fn right_cycles_and_wraps() {
        let mut s = Settings::default();
        let c = ctx();
        assert!(cycle(RowId::SeekStep, &mut s, &c, 1));
        assert_eq!(s.seek_step_seconds, 15);
        s.seek_step_seconds = 60;
        assert!(cycle(RowId::SeekStep, &mut s, &c, 1));
        assert_eq!(s.seek_step_seconds, 5);
        assert!(cycle(RowId::SeekStep, &mut s, &c, -1));
        assert_eq!(s.seek_step_seconds, 60);

        s.ui_scale = 1.25;
        assert!(cycle(RowId::UiScale, &mut s, &c, 1));
        assert_eq!(s.ui_scale, 0.75);

        assert!(cycle(RowId::Resume, &mut s, &c, -1));
        assert_eq!(s.resume_mode, ResumeMode::Never);
    }

    #[test]
    fn left_right_flip_toggles() {
        let mut s = Settings::default();
        assert!(cycle(RowId::Subtitles, &mut s, &ctx(), -1));
        assert!(s.subtitles_enabled);
        assert!(cycle(RowId::Loudness, &mut s, &ctx(), 1));
        assert!(!s.loudness_boost);
    }

    #[test]
    fn rows_without_values_do_not_cycle() {
        let mut s = Settings::default();
        let before = s.clone();
        for id in [
            RowId::Channel,
            RowId::ClearHistory,
            RowId::TmdbKey,
            RowId::Account,
            RowId::FfmpegPath,
            RowId::Version,
        ] {
            assert!(!cycle(id, &mut s, &ctx(), 1), "{id:?}");
        }
        assert_eq!(s, before);
    }

    #[test]
    fn unknown_current_value_cycles_from_the_first_option() {
        // Android: indexOf(get()).coerceAtLeast(0).
        let mut s = Settings {
            loudness_gain_db: 7.6,
            ..Settings::default()
        };
        assert!(cycle(RowId::LoudnessGain, &mut s, &ctx(), 1));
        assert_eq!(s.loudness_gain_db, 9.0, "rounded to +8, then one step up");
        s.continue_watching_limit = 33;
        assert!(cycle(RowId::ContinueLimit, &mut s, &ctx(), 1));
        assert_eq!(s.continue_watching_limit, 25);
    }

    #[test]
    fn gain_choices_are_whole_db_from_0_to_12() {
        let opts = options(RowId::LoudnessGain, &Settings::default(), &ctx());
        assert_eq!(opts.len(), 13);
        assert_eq!(option_label(&opts[0]), "+0 DB");
        assert_eq!(option_label(&opts[12]), "+12 DB");
    }

    #[test]
    fn quality_choices_include_the_library_tallest_first() {
        let library = vec![
            "1080p".to_owned(),
            "2160p DV".to_owned(),
            "720p HDR".to_owned(),
            "2160p DV".to_owned(),
        ];
        let q = qualities(&library, None);
        let labels: Vec<Option<&str>> = q.iter().map(|q| q.as_deref()).collect();
        assert_eq!(
            labels,
            [
                None,
                Some("2160p"),
                Some("2160p DV"),
                Some("1440p"),
                Some("1080p"),
                Some("720p"),
                Some("720p HDR"),
                Some("576p"),
                Some("480p"),
            ]
        );
    }

    #[test]
    fn a_saved_quality_missing_from_the_list_is_kept_last() {
        let q = qualities(&[], Some("4320p"));
        assert_eq!(q.last(), Some(&Some("4320p".to_owned())));
        assert_eq!(q.len(), 8);
    }

    #[test]
    fn quality_dialog_marks_the_saved_quality() {
        let s = Settings {
            quality: Some("2160p DV".into()),
            ..Settings::default()
        };
        let c = Context {
            library_qualities: vec!["2160p DV".into()],
            ..ctx()
        };
        let Action::Choose {
            title,
            options,
            current,
        } = action(RowId::Quality, &s, &c)
        else {
            panic!("quality is a choice");
        };
        assert_eq!(title, "Default quality");
        assert_eq!(option_label(&options[current]), "2160P DV");
        assert_eq!(value(RowId::Quality, &s, &c), "2160P DV");
    }

    #[test]
    fn language_choices_start_with_any_or_device_language() {
        let s = Settings::default();
        let audio = options(RowId::AudioLanguage, &s, &ctx());
        assert_eq!(audio.len(), 14);
        assert_eq!(option_label(&audio[0]), "ANY");
        assert_eq!(option_label(&audio[1]), "ENGLISH");
        let subs = options(RowId::SubtitleLanguage, &s, &ctx());
        assert_eq!(option_label(&subs[0]), "DEVICE LANGUAGE");
        assert_eq!(option_label(&subs[13]), "TURKISH");
    }

    #[test]
    fn choosing_stores_the_value() {
        let mut s = Settings::default();
        let opts = options(RowId::SubtitleLanguage, &s, &ctx());
        set(&mut s, &opts[3]);
        assert_eq!(s.subtitle_language.as_deref(), Some("fr"));
        let opts = options(RowId::UiScale, &s, &ctx());
        assert_eq!(
            opts.iter().map(option_label).collect::<Vec<_>>(),
            ["75%", "100%", "125%"]
        );
        set(&mut s, &opts[0]);
        assert_eq!(s.ui_scale, 0.75);
    }

    #[test]
    fn center_actions() {
        let s = Settings::default();
        let c = ctx();
        assert_eq!(action(RowId::Subtitles, &s, &c), Action::Toggle);
        assert_eq!(
            action(RowId::Channel, &s, &c),
            Action::Edit(TextField::Channel)
        );
        assert_eq!(
            action(RowId::ClearHistory, &s, &c),
            Action::Confirm(Confirm::ClearHistory)
        );
        assert_eq!(action(RowId::Account, &s, &c), Action::Open(Route::Login));
        assert_eq!(action(RowId::Version, &s, &c), Action::Nothing);
        let ready = Context {
            status: TelegramStatus::Auth(AuthState::Ready { user: "a".into() }),
            ..ctx()
        };
        assert_eq!(
            action(RowId::Account, &s, &ready),
            Action::Confirm(Confirm::SignOut)
        );
        assert_eq!(label(RowId::Account, &ready), "Sign out of Telegram");
        assert_eq!(value(RowId::Account, &s, &ready), "SIGNED IN");
        assert_eq!(label(RowId::Account, &c), "Sign in to Telegram");
    }

    #[test]
    fn account_value_follows_the_auth_state() {
        let with = |status| Context { status, ..ctx() };
        assert_eq!(
            account_value(&with(TelegramStatus::Unavailable)),
            "TDLIB NOT FOUND"
        );
        assert_eq!(
            account_value(&with(TelegramStatus::Auth(AuthState::WaitPhone))),
            "SIGNED OUT"
        );
        assert_eq!(
            account_value(&with(TelegramStatus::Auth(AuthState::Connecting))),
            "CONNECTING"
        );
        let pending = Context {
            restart_pending: true,
            ..with(TelegramStatus::Auth(AuthState::WaitPhone))
        };
        assert_eq!(account_value(&pending), "RESTART FLOX TO APPLY");
    }

    #[test]
    fn secrets_are_masked() {
        assert_eq!(mask("abc"), "••••••••");
        assert_eq!(mask("0123456789abcdef"), "••••••••CDEF");
        let s = Settings {
            tmdb_api_key: Some("0123456789abcdef".into()),
            telegram_api_id: Some(12345),
            telegram_api_hash: Some("feedfacefeedface".into()),
            ..Settings::default()
        };
        let c = ctx();
        assert_eq!(value(RowId::TmdbKey, &s, &c), "••••••••CDEF");
        assert_eq!(value(RowId::TelegramApiId, &s, &c), "12345");
        assert_eq!(value(RowId::TelegramApiHash, &s, &c), "••••••••FACE");
        assert!(text_edit(TextField::TmdbKey, &s).secret);
        assert!(text_edit(TextField::TelegramApiHash, &s).secret);
        assert!(!text_edit(TextField::TelegramApiId, &s).secret);
    }

    #[test]
    fn unset_credentials_read_not_set_or_built_in() {
        let s = Settings::default();
        let expected = |built: bool| if built { BUILT_IN } else { NOT_SET };
        let b = build_defaults();
        assert_eq!(
            value(RowId::TmdbKey, &s, &ctx()),
            expected(b.tmdb_api_key.is_some())
        );
        assert_eq!(
            value(RowId::TelegramApiId, &s, &ctx()),
            expected(b.telegram_api_id.is_some())
        );
    }

    #[test]
    fn text_edits_apply_and_clear() {
        let mut s = Settings::default();
        assert_eq!(
            apply_text(TextField::TelegramApiId, &mut s, " 12345 "),
            Ok(())
        );
        assert_eq!(s.telegram_api_id, Some(12345));
        assert_eq!(
            apply_text(TextField::TelegramApiId, &mut s, "12a"),
            Err(API_ID_NOT_A_NUMBER)
        );
        assert_eq!(
            apply_text(TextField::TelegramApiId, &mut s, "-4"),
            Err(API_ID_NOT_A_NUMBER)
        );
        assert_eq!(s.telegram_api_id, Some(12345), "unchanged on error");
        assert_eq!(apply_text(TextField::TelegramApiId, &mut s, "  "), Ok(()));
        assert_eq!(s.telegram_api_id, None);

        apply_text(TextField::FfmpegPath, &mut s, r#" "C:\tools\ffmpeg.exe" "#).ok();
        assert_eq!(s.ffmpeg_path, Some(PathBuf::from(r"C:\tools\ffmpeg.exe")));
        apply_text(TextField::FfmpegPath, &mut s, "").ok();
        assert_eq!(s.ffmpeg_path, None);

        apply_text(TextField::TmdbKey, &mut s, " key ").ok();
        assert_eq!(s.tmdb_api_key.as_deref(), Some("key"));

        apply_text(TextField::Channel, &mut s, "  ").ok();
        assert_eq!(s.clone().clamped().telegram_channel, "Flox Library");
    }

    #[test]
    fn text_dialogs_prefill_the_saved_value() {
        let s = Settings {
            telegram_api_id: Some(42),
            ytdlp_path: Some(PathBuf::from("/opt/yt-dlp")),
            ..Settings::default()
        };
        assert_eq!(text_edit(TextField::TelegramApiId, &s).text, "42");
        assert_eq!(text_edit(TextField::YtDlpPath, &s).text, "/opt/yt-dlp");
        assert_eq!(
            text_edit(TextField::Channel, &s).text,
            "Flox Library".to_owned()
        );
    }

    #[test]
    fn only_telegram_credentials_trigger_a_restart() {
        let base = Settings::default();
        let mut next = base.clone();
        next.tmdb_api_key = Some("k".into());
        next.seek_step_seconds = 30;
        next.telegram_channel = "Other".into();
        assert!(!telegram_restart_needed(&base, &next));

        let mut id = base.clone();
        id.telegram_api_id = Some(9);
        assert!(telegram_restart_needed(&base, &id));
        let mut hash = base.clone();
        hash.telegram_api_hash = Some("h".into());
        assert!(telegram_restart_needed(&base, &hash));
        // Same value saved again: nothing to do.
        assert!(!telegram_restart_needed(&id, &id.clone()));
    }

    #[test]
    fn tools_resolve_from_the_override() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let ffmpeg = dir.path().join(Tool::Ffmpeg.file_name());
        std::fs::write(&ffmpeg, b"").unwrap_or_else(|e| panic!("{e}"));
        let s = Settings {
            ffmpeg_path: Some(dir.path().to_path_buf()),
            ..Settings::default()
        };
        let empty = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let t = resolve_tools(&s, empty.path(), Some(OsStr::new("")));
        assert_eq!(t.ffmpeg.as_deref(), Some(ffmpeg.as_path()));
        assert_eq!(t.ytdlp, None);
        let c = Context { tools: t, ..ctx() };
        assert_eq!(
            value(RowId::FfmpegPath, &s, &c),
            ffmpeg.display().to_string()
        );
        assert_eq!(value(RowId::YtDlpPath, &s, &c), "NOT FOUND");
    }

    #[test]
    fn dialog_zones_enter_where_expected() {
        use crate::focus::{Focus, FocusGraph};
        let mut g = FocusGraph::with_zones(zones(row_ids().len()));
        g.focus_first();
        g.push_modal(choice_zones(5, 3));
        assert_eq!(g.focus(), Some(Focus::new(OPTIONS, 3)));
        g.pop_modal();
        g.push_modal(text_zones());
        assert_eq!(g.focus(), Some(Focus::new(FIELD, 0)));
        assert!(g.editing());
        g.pop_modal();
        g.push_modal(confirm_zones());
        assert_eq!(g.focus(), Some(Focus::new(BUTTONS, CANCEL)));
        g.pop_modal();
        assert_eq!(g.focus(), Some(Focus::new(LIST, 0)));
        assert_eq!(row(RowId::Version), row_ids().len() - 1);
    }
}
