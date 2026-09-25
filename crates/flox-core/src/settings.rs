//! App settings, stored as `settings.json` with the Android key names.
//!
//! Serde names are the Android `SharedPreferences` keys and enum values are
//! the Android enum names, so an Android-shaped file round-trips. Persistence,
//! clamping and unknown-key preservation are filled in by piece P2.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::error::{Error, Result};
use crate::paths::write_atomic;

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

/// Every key [`Settings`] owns in `settings.json`. Any other key in the file is kept on save.
pub const KNOWN_KEYS: &[&str] = &[
    "audio_language",
    "subtitles_enabled",
    "subtitle_language",
    "subtitle_size",
    "autoplay_next",
    "seek_step_seconds",
    "loudness_boost",
    "loudness_gain_db",
    "overlay_hide_ms",
    "resume_mode",
    "finished_threshold_percent",
    "playback_speed",
    "aspect_mode",
    "library_sort",
    "telegram_channel",
    "continue_watching_limit",
    "quality",
    "ui_scale",
    "tmdb_api_key",
    "telegram_api_id",
    "telegram_api_hash",
    "ffmpeg_path",
    "ytdlp_path",
    "tdjson_path",
    "libmpv_path",
];

impl Settings {
    /// Reads `settings.json`; a missing or blank file gives the defaults. Keys whose value
    /// has the wrong type or an unknown enum name fall back to their default, as Android
    /// does. The result is [`clamped`](Settings::clamped). A file that is not a JSON object
    /// is an error.
    pub fn load(path: &Path) -> Result<Settings> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Settings::default()),
            Err(e) => return Err(e.into()),
        };
        if text.trim().is_empty() {
            return Ok(Settings::default());
        }
        let file: Map<String, Value> = serde_json::from_str(&text)?;
        Ok(Settings::from_map(&file).clamped())
    }

    /// Builds settings from a parsed object, dropping any known key whose value does not
    /// deserialize so that one bad value never discards the rest.
    fn from_map(file: &Map<String, Value>) -> Settings {
        let mut accepted = Map::new();
        for (key, value) in file {
            if !KNOWN_KEYS.contains(&key.as_str()) {
                continue;
            }
            let value = coerce(key, value);
            let mut candidate = accepted.clone();
            candidate.insert(key.clone(), value.clone());
            if serde_json::from_value::<Settings>(Value::Object(candidate)).is_ok() {
                accepted.insert(key.clone(), value);
            }
        }
        serde_json::from_value(Value::Object(accepted)).unwrap_or_default()
    }

    /// Writes `settings.json` atomically (temp file, then rename). Keys this version does
    /// not know are copied from the existing file; `None` values are left out.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut out = match serde_json::to_value(self.clone().clamped())? {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(Value::Object(existing)) = serde_json::from_str::<Value>(&text) {
                for (key, value) in existing {
                    if !KNOWN_KEYS.contains(&key.as_str()) {
                        out.insert(key, value);
                    }
                }
            }
        }
        let mut bytes = serde_json::to_vec_pretty(&Value::Object(out))?;
        bytes.push(b'\n');
        write_atomic(path, &bytes)
    }

    /// Snaps every number to its nearest allowed option (the first one wins a tie, as
    /// `nearest()` in `Settings.kt`), clamps the gain to [`LOUDNESS_GAIN_RANGE`], and turns
    /// empty strings, empty paths and a zero API id into `None`. A blank Telegram channel
    /// becomes the default.
    pub fn clamped(self) -> Settings {
        let (gain_min, gain_max) = LOUDNESS_GAIN_RANGE;
        Settings {
            audio_language: opt_string(self.audio_language),
            subtitle_language: opt_string(self.subtitle_language),
            seek_step_seconds: nearest_u32(self.seek_step_seconds, SEEK_STEPS),
            loudness_gain_db: if self.loudness_gain_db.is_finite() {
                self.loudness_gain_db.clamp(gain_min, gain_max)
            } else {
                DEFAULT_LOUDNESS_GAIN_DB
            },
            overlay_hide_ms: nearest_u32(self.overlay_hide_ms, OVERLAY_HIDE_OPTIONS),
            finished_threshold_percent: nearest_u32(
                self.finished_threshold_percent,
                FINISHED_THRESHOLDS,
            ),
            playback_speed: nearest_f32(
                self.playback_speed,
                PLAYBACK_SPEEDS,
                DEFAULT_PLAYBACK_SPEED,
            ),
            telegram_channel: non_empty(Some(&self.telegram_channel))
                .unwrap_or(DEFAULT_TELEGRAM_CHANNEL)
                .to_owned(),
            continue_watching_limit: nearest_u32(
                self.continue_watching_limit,
                CONTINUE_WATCHING_LIMITS,
            ),
            quality: opt_string(self.quality),
            ui_scale: nearest_f32(self.ui_scale, UI_SCALES, DEFAULT_UI_SCALE),
            tmdb_api_key: opt_string(self.tmdb_api_key),
            telegram_api_id: self.telegram_api_id.filter(|id| *id != 0),
            telegram_api_hash: opt_string(self.telegram_api_hash),
            ffmpeg_path: opt_path(self.ffmpeg_path),
            ytdlp_path: opt_path(self.ytdlp_path),
            tdjson_path: opt_path(self.tdjson_path),
            libmpv_path: opt_path(self.libmpv_path),
            ..self
        }
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

/// Keys stored as whole numbers on Android (`putInt`).
const INT_KEYS: &[&str] = &[
    "seek_step_seconds",
    "overlay_hide_ms",
    "finished_threshold_percent",
    "continue_watching_limit",
];

/// Brings an out-of-range or fractional number for an integer key into `u32` range so it
/// still snaps to the nearest option instead of being dropped.
fn coerce(key: &str, value: &Value) -> Value {
    match value.as_f64() {
        Some(n) if INT_KEYS.contains(&key) && n.is_finite() => {
            Value::from(n.clamp(0.0, f64::from(u32::MAX)).round() as u32)
        }
        _ => value.clone(),
    }
}

fn opt_string(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.trim().is_empty())
}

fn opt_path(p: Option<PathBuf>) -> Option<PathBuf> {
    p.filter(|p| !p.as_os_str().to_string_lossy().trim().is_empty())
}

/// The allowed option closest to `value`; the first one wins a tie.
pub fn nearest_u32(value: u32, allowed: &[u32]) -> u32 {
    allowed
        .iter()
        .copied()
        .min_by_key(|a| a.abs_diff(value))
        .unwrap_or(value)
}

/// The allowed option closest to `value`; the first one wins a tie. A non-finite
/// `value` gives `fallback`.
pub fn nearest_f32(value: f32, allowed: &[f32], fallback: f32) -> f32 {
    if !value.is_finite() {
        return fallback;
    }
    let mut best: Option<f32> = None;
    for a in allowed.iter().copied() {
        if best.is_none_or(|b| (a - value).abs() < (b - value).abs()) {
            best = Some(a);
        }
    }
    best.unwrap_or(value)
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

    /// Loads (clamped) from `path`. A file that is not valid JSON is logged and replaced
    /// by the defaults on the next save; I/O errors are returned.
    pub fn open(path: PathBuf) -> Result<Self> {
        let settings = match Settings::load(&path) {
            Ok(s) => s,
            Err(Error::Json(e)) => {
                tracing::warn!(
                    "settings file {} is unreadable, using defaults: {e}",
                    path.display()
                );
                Settings::default()
            }
            Err(e) => return Err(e),
        };
        Ok(Self::new(path, settings))
    }

    /// The file backing this store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A snapshot of the current settings.
    pub fn get(&self) -> Settings {
        self.current.read().clone()
    }

    /// Applies `f`, clamps, saves and notifies. Nothing changes in memory when the save
    /// fails, and subscribers are only notified when the value actually changed.
    pub fn update(&self, f: impl FnOnce(&mut Settings)) -> Result<()> {
        let mut current = self.current.write();
        let mut next = current.clone();
        f(&mut next);
        let next = next.clamped();
        if next == *current {
            return Ok(());
        }
        next.save(&self.path)?;
        *current = next.clone();
        drop(current);
        self.changes.send_replace(next);
        Ok(())
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
