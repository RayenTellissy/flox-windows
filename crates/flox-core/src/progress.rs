//! Watch progress, stored as `progress.json` in the Android record shape: one JSON array,
//! newest first, one record per (type, id), capped at [`PROGRESS_CAP`].

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::model::{MediaType, TmdbId};
use crate::paths::write_atomic;

/// At most this many records are kept, newest first.
pub const PROGRESS_CAP: usize = 100;

/// One title's progress. `watched` and `duration` are seconds, `updated` is epoch millis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressRecord {
    pub id: TmdbId,
    #[serde(rename = "type")]
    pub media: MediaType,
    pub title: String,
    /// Left out of the file when `None`, as Android's `JSONObject.put(key, null)` does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
    pub watched: u32,
    pub duration: u32,
    pub season: u32,
    pub episode: u32,
    pub updated: i64,
}

impl ProgressRecord {
    /// Whether the title counts as watched at `threshold_pct`. Mirrors Android's
    /// `fraction * 100 >= threshold`, where an unknown (zero) duration has fraction 0.
    pub fn finished(&self, threshold_pct: u32) -> bool {
        if self.duration == 0 {
            return threshold_pct == 0;
        }
        u64::from(self.watched) * 100 >= u64::from(self.duration) * u64::from(threshold_pct)
    }

    /// Reads one record the way Android's `fromJson` does: missing numbers are 0, season and
    /// episode default to 1, an empty or `"null"` poster is `None`, and any type other than
    /// `"tv"` is a movie. A record without a usable id is skipped.
    fn from_json(v: &Value) -> Option<ProgressRecord> {
        let o = v.as_object()?;
        let int = |key: &str, default: i64| -> i64 {
            o.get(key)
                .and_then(|n| n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)))
                .unwrap_or(default)
        };
        let text =
            |key: &str| -> Option<String> { o.get(key).and_then(Value::as_str).map(str::to_owned) };
        let secs = |key: &str, default: i64| -> u32 {
            u32::try_from(int(key, default).max(0)).unwrap_or(u32::MAX)
        };
        let id = o.get("id").and_then(|n| {
            n.as_u64()
                .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
        })?;
        Some(ProgressRecord {
            id,
            media: if text("type").as_deref() == Some("tv") {
                MediaType::Tv
            } else {
                MediaType::Movie
            },
            title: text("title").unwrap_or_default(),
            poster: text("poster").filter(|p| !p.is_empty() && p != "null"),
            watched: secs("watched", 0),
            duration: secs("duration", 0),
            season: secs("season", 1),
            episode: secs("episode", 1),
            updated: int("updated", 0),
        })
    }
}

/// The progress list, one record per (type, id), newest first, capped at [`PROGRESS_CAP`].
pub struct ProgressStore {
    path: PathBuf,
    items: RwLock<Vec<ProgressRecord>>,
}

impl ProgressStore {
    /// Loads `progress.json`. A missing file, or one that is not a JSON array, is empty
    /// (Android's `getOrDefault(emptyList())`); I/O errors are returned. Duplicate
    /// (type, id) records keep the first (newest) one.
    pub fn open(path: &Path) -> Result<Self> {
        let items = match fs::read_to_string(path) {
            Ok(text) => parse(&text),
            Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path: path.to_path_buf(),
            items: RwLock::new(items),
        })
    }

    /// The file backing this store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Upserts by (type, id), moves the record to the front and saves. The list in memory
    /// is only replaced once the save succeeds.
    pub fn put(&self, r: ProgressRecord) -> Result<()> {
        let mut items = self.items.write();
        let (media, id) = (r.media, r.id);
        let mut next = Vec::with_capacity(items.len() + 1);
        next.push(r);
        next.extend(
            items
                .iter()
                .filter(|o| !(o.media == media && o.id == id))
                .cloned(),
        );
        next.truncate(PROGRESS_CAP);
        save(&self.path, &next)?;
        *items = next;
        Ok(())
    }

    /// The record for a title, finished or not.
    pub fn get(&self, media: MediaType, id: TmdbId) -> Option<ProgressRecord> {
        self.items
            .read()
            .iter()
            .find(|r| r.media == media && r.id == id)
            .cloned()
    }

    /// Every record, newest first, finished ones included.
    pub fn all(&self) -> Vec<ProgressRecord> {
        self.items.read().clone()
    }

    /// Unfinished records (`watched * 100 < duration * finished_pct`), newest first,
    /// at most `limit`.
    pub fn continue_watching(&self, finished_pct: u32, limit: usize) -> Vec<ProgressRecord> {
        self.items
            .read()
            .iter()
            .filter(|r| !r.finished(finished_pct))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Removes every record and saves an empty list.
    pub fn clear(&self) -> Result<()> {
        let mut items = self.items.write();
        save(&self.path, &[])?;
        items.clear();
        Ok(())
    }
}

fn parse(text: &str) -> Vec<ProgressRecord> {
    let Ok(Value::Array(raw)) = serde_json::from_str::<Value>(text) else {
        if !text.trim().is_empty() {
            tracing::warn!("progress file is not a JSON array, starting empty");
        }
        return Vec::new();
    };
    let mut out: Vec<ProgressRecord> = Vec::new();
    for r in raw.iter().filter_map(ProgressRecord::from_json) {
        if out.len() == PROGRESS_CAP {
            break;
        }
        if !out.iter().any(|o| o.media == r.media && o.id == r.id) {
            out.push(r);
        }
    }
    out
}

fn save(path: &Path, items: &[ProgressRecord]) -> Result<()> {
    let bytes = serde_json::to_vec(items)?;
    write_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: TmdbId, watched: u32, duration: u32) -> ProgressRecord {
        ProgressRecord {
            id,
            media: MediaType::Movie,
            title: format!("t{id}"),
            poster: None,
            watched,
            duration,
            season: 0,
            episode: 0,
            updated: 0,
        }
    }

    #[test]
    fn finished_matches_android_fraction() {
        assert!(rec(1, 95, 100).finished(95));
        assert!(!rec(1, 94, 100).finished(95));
        assert!(!rec(1, 0, 0).finished(95));
        assert!(rec(1, 0, 0).finished(0));
        assert!(rec(1, u32::MAX, u32::MAX).finished(98));
    }

    #[test]
    fn from_json_is_lenient() {
        let v: Value = serde_json::from_str(
            r#"{"id":7,"type":"weird","title":"X","poster":"null","watched":-3,"updated":12}"#,
        )
        .unwrap();
        let r = ProgressRecord::from_json(&v).unwrap();
        assert_eq!(r.media, MediaType::Movie);
        assert_eq!(r.poster, None);
        assert_eq!((r.watched, r.duration, r.season, r.episode), (0, 0, 1, 1));
        assert_eq!(r.updated, 12);
        assert!(ProgressRecord::from_json(&serde_json::json!({"title": "no id"})).is_none());
    }
}
