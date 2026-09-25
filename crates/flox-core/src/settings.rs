//! App settings, stored as `settings.json` with the Android key names.
//!
//! Serde names are the Android `SharedPreferences` keys and enum values are
//! the Android enum names, so an Android-shaped file round-trips. Persistence,
//! clamping and unknown-key preservation are filled in by piece P2.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::error::{Error, Result};

/// Subtitle size. `"SMALL"` | `"NORMAL"` | `"LARGE"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SubtitleSize {
    Small,
    #[default]
    Normal,
    Large,
}

/// What to do when a title has saved progress. `"ALWAYS"` | `"ASK"` | `"NEVER"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResumeMode {
    #[default]
    Always,
    Ask,
    Never,
}

/// Video scaling. `"FIT"` | `"FILL"` | `"ZOOM"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AspectMode {
    #[default]
    Fit,
    Fill,
    Zoom,
}

/// Library row order. `"TITLE"` | `"DATE_ADDED"` | `"SIZE"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LibrarySort {
    Title,
    #[default]
    DateAdded,
    Size,
}

/// Seek step choices in seconds.
pub const SEEK_STEPS: &[u32] = &[5, 10, 15, 30, 60];
/// "Hide player controls after" choices in milliseconds.
pub const OVERLAY_HIDE_OPTIONS: &[u32] = &[2000, 4000, 6000, 10000];
/// "Mark finished at" choices in percent.
pub const FINISHED_THRESHOLDS: &[u32] = &[85, 90, 95, 98];
/// Playback speed choices.
pub const PLAYBACK_SPEEDS: &[f32] = &[0.75, 1.0, 1.25, 1.5, 2.0];
/// Continue watching row length choices.
pub const CONTINUE_WATCHING_LIMITS: &[u32] = &[10, 25, 50, 100];
/// Loudness gain bounds in dB, inclusive.
pub const LOUDNESS_GAIN_RANGE: (f32, f32) = (0.0, 12.0);
/// Default quality choices. `None` (HIGHEST) comes first in the UI and is not listed here;
/// the Settings screen appends any other quality present in the library.
pub const QUALITY_OPTIONS: &[&str] = &["2160p", "1440p", "1080p", "720p", "576p", "480p"];
/// Audio and subtitle language choices (ISO 639-1). `None` (ANY / DEVICE LANGUAGE) comes first
/// in the UI and is not listed here. English names live in [`crate::lang::LANGUAGE_NAMES`].
pub const LANGUAGES: &[&str] = &[
    "en", "es", "fr", "de", "it", "pt", "ru", "ar", "hi", "ja", "ko", "zh", "tr",
];
/// UI scale choices (Windows addition).
pub const UI_SCALES: &[f32] = &[0.75, 1.0, 1.25];

pub const DEFAULT_SUBTITLES_ENABLED: bool = false;
pub const DEFAULT_AUTOPLAY_NEXT: bool = true;
pub const DEFAULT_SEEK_STEP_SECONDS: u32 = 10;
pub const DEFAULT_LOUDNESS_BOOST: bool = true;
pub const DEFAULT_LOUDNESS_GAIN_DB: f32 = 8.0;
pub const DEFAULT_OVERLAY_HIDE_MS: u32 = 4000;
pub const DEFAULT_FINISHED_THRESHOLD_PERCENT: u32 = 95;
pub const DEFAULT_PLAYBACK_SPEED: f32 = 1.0;
pub const DEFAULT_TELEGRAM_CHANNEL: &str = "Flox Library";
pub const DEFAULT_CONTINUE_WATCHING_LIMIT: u32 = 50;
pub const DEFAULT_UI_SCALE: f32 = 1.0;

/// Every setting. Optional strings are `None` when missing or empty (they are removed
/// from the file rather than stored as `""`, as on Android).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_language: Option<String>,
    pub subtitles_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle_language: Option<String>,
    pub subtitle_size: SubtitleSize,
    pub autoplay_next: bool,
    pub seek_step_seconds: u32,
    pub loudness_boost: bool,
    pub loudness_gain_db: f32,
    pub overlay_hide_ms: u32,
    pub resume_mode: ResumeMode,
    pub finished_threshold_percent: u32,
    pub playback_speed: f32,
    pub aspect_mode: AspectMode,
    pub library_sort: LibrarySort,
    pub telegram_channel: String,
    pub continue_watching_limit: u32,
    /// Preferred library quality such as `"2160p DV"`; `None` = HIGHEST.
    /// Owned by the player (Android keeps it in `flox_player`), stored here on Windows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// UI scale, one of [`UI_SCALES`].
    pub ui_scale: f32,
    /// Runtime TMDB key. When `None`, [`build_defaults`] supplies the build-time key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmdb_api_key: Option<String>,
    /// Runtime Telegram API id. When `None`, [`build_defaults`] supplies the build-time id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telegram_api_id: Option<i32>,
    /// Runtime Telegram API hash. When `None`, [`build_defaults`] supplies the build-time hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telegram_api_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ffmpeg_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ytdlp_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tdjson_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub libmpv_path: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            audio_language: None,
            subtitles_enabled: DEFAULT_SUBTITLES_ENABLED,
            subtitle_language: None,
            subtitle_size: SubtitleSize::default(),
            autoplay_next: DEFAULT_AUTOPLAY_NEXT,
            seek_step_seconds: DEFAULT_SEEK_STEP_SECONDS,
            loudness_boost: DEFAULT_LOUDNESS_BOOST,
            loudness_gain_db: DEFAULT_LOUDNESS_GAIN_DB,
            overlay_hide_ms: DEFAULT_OVERLAY_HIDE_MS,
            resume_mode: ResumeMode::default(),
            finished_threshold_percent: DEFAULT_FINISHED_THRESHOLD_PERCENT,
            playback_speed: DEFAULT_PLAYBACK_SPEED,
            aspect_mode: AspectMode::default(),
            library_sort: LibrarySort::default(),
            telegram_channel: DEFAULT_TELEGRAM_CHANNEL.to_owned(),
            continue_watching_limit: DEFAULT_CONTINUE_WATCHING_LIMIT,
            quality: None,
            ui_scale: DEFAULT_UI_SCALE,
            tmdb_api_key: None,
            telegram_api_id: None,
            telegram_api_hash: None,
            ffmpeg_path: None,
            ytdlp_path: None,
            tdjson_path: None,
            libmpv_path: None,
        }
    }
}

impl Settings {
    /// Reads `settings.json`; a missing file gives the defaults. Filled in by P2.
    pub fn load(_path: &Path) -> Result<Settings> {
        Err(Error::NotImplemented("flox_core::settings::Settings::load"))
    }

    /// Writes `settings.json` atomically, keeping unknown keys. Filled in by P2.
    pub fn save(&self, _path: &Path) -> Result<()> {
        Err(Error::NotImplemented("flox_core::settings::Settings::save"))
    }

    /// Snaps every number to its nearest allowed option and clamps the gain. Filled in by P2.
    #[allow(clippy::unimplemented)]
    pub fn clamped(self) -> Settings {
        unimplemented!("flox_core::settings::Settings::clamped (P2)")
    }

    /// The TMDB key to use: the saved one, else the build-time one.
    pub fn effective_tmdb_api_key(&self) -> Option<String> {
        non_empty(self.tmdb_api_key.as_deref())
            .or(build_defaults().tmdb_api_key)
            .map(str::to_owned)
    }

    /// The Telegram API id to use: the saved one, else the build-time one.
    pub fn effective_telegram_api_id(&self) -> Option<i32> {
        self.telegram_api_id
            .filter(|id| *id != 0)
            .or(build_defaults().telegram_api_id)
    }

    /// The Telegram API hash to use: the saved one, else the build-time one.
    pub fn effective_telegram_api_hash(&self) -> Option<String> {
        non_empty(self.telegram_api_hash.as_deref())
            .or(build_defaults().telegram_api_hash)
            .map(str::to_owned)
    }
}

/// Credentials baked in at build time from `FLOX_TMDB_API_KEY`, `FLOX_TELEGRAM_API_ID`
/// and `FLOX_TELEGRAM_API_HASH`. Used when the saved value is empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildDefaults {
    pub tmdb_api_key: Option<&'static str>,
    pub telegram_api_id: Option<i32>,
    pub telegram_api_hash: Option<&'static str>,
}

/// The build-time credentials (empty or unparsable values are `None`).
pub fn build_defaults() -> BuildDefaults {
    BuildDefaults {
        tmdb_api_key: non_empty(option_env!("FLOX_TMDB_API_KEY")),
        telegram_api_id: non_empty(option_env!("FLOX_TELEGRAM_API_ID"))
            .and_then(|s| s.parse().ok())
            .filter(|id| *id != 0),
        telegram_api_hash: non_empty(option_env!("FLOX_TELEGRAM_API_HASH")),
    }
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// Shared, observable settings: the current value, its file, and a watch channel
/// that fires on every change. Cheap to clone.
#[derive(Clone)]
pub struct SettingsStore {
    current: Arc<RwLock<Settings>>,
    path: Arc<PathBuf>,
    changes: Arc<watch::Sender<Settings>>,
}

impl SettingsStore {
    /// A store over an already loaded value.
    pub fn new(path: PathBuf, settings: Settings) -> Self {
        let (tx, _rx) = watch::channel(settings.clone());
        Self {
            current: Arc::new(RwLock::new(settings)),
            path: Arc::new(path),
            changes: Arc::new(tx),
        }
    }

    /// Loads (clamped) from `path`. Filled in by P2.
    pub fn open(_path: PathBuf) -> Result<Self> {
        Err(Error::NotImplemented(
            "flox_core::settings::SettingsStore::open",
        ))
    }

    /// The file backing this store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A snapshot of the current settings.
    pub fn get(&self) -> Settings {
        self.current.read().clone()
    }

    /// Applies `f`, clamps, saves and notifies. Filled in by P2.
    pub fn update(&self, _f: impl FnOnce(&mut Settings)) -> Result<()> {
        Err(Error::NotImplemented(
            "flox_core::settings::SettingsStore::update",
        ))
    }

    /// A receiver that sees every change.
    pub fn subscribe(&self) -> watch::Receiver<Settings> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_android() {
        let s = Settings::default();
        assert_eq!(s.seek_step_seconds, 10);
        assert_eq!(s.overlay_hide_ms, 4000);
        assert_eq!(s.finished_threshold_percent, 95);
        assert_eq!(s.continue_watching_limit, 50);
        assert_eq!(s.telegram_channel, "Flox Library");
        assert_eq!(s.library_sort, LibrarySort::DateAdded);
        assert!(s.loudness_boost && s.autoplay_next && !s.subtitles_enabled);
        assert!((s.ui_scale - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn enum_names_match_android() {
        let json = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(json["library_sort"], "DATE_ADDED");
        assert_eq!(json["subtitle_size"], "NORMAL");
        assert_eq!(json["resume_mode"], "ALWAYS");
        assert_eq!(json["aspect_mode"], "FIT");
        assert!(json.get("audio_language").is_none());
    }
}
