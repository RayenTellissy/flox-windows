//! Watch progress, stored as `progress.json` in the Android record shape.
//! Filled in by piece P2.

use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{MediaType, TmdbId};

/// At most this many records are kept, newest first.
pub const PROGRESS_CAP: usize = 100;

/// One title's progress. `watched` and `duration` are seconds, `updated` is epoch millis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressRecord {
    pub id: TmdbId,
    #[serde(rename = "type")]
    pub media: MediaType,
    pub title: String,
    pub poster: Option<String>,
    pub watched: u32,
    pub duration: u32,
    pub season: u32,
    pub episode: u32,
    pub updated: i64,
}

/// The progress list, one record per (type, id), newest first, capped at [`PROGRESS_CAP`].
pub struct ProgressStore {
    #[allow(dead_code)]
    path: PathBuf,
    #[allow(dead_code)]
    items: RwLock<Vec<ProgressRecord>>,
}

impl ProgressStore {
    /// Loads `progress.json` (missing file = empty). Filled in by P2.
    pub fn open(_path: &Path) -> Result<Self> {
        Err(Error::NotImplemented(
            "flox_core::progress::ProgressStore::open",
        ))
    }

    /// Upserts by (type, id), moves the record to the front and saves. Filled in by P2.
    pub fn put(&self, _r: ProgressRecord) -> Result<()> {
        Err(Error::NotImplemented(
            "flox_core::progress::ProgressStore::put",
        ))
    }

    /// The record for a title. Filled in by P2.
    #[allow(clippy::unimplemented)]
    pub fn get(&self, _media: MediaType, _id: TmdbId) -> Option<ProgressRecord> {
        unimplemented!("flox_core::progress::ProgressStore::get (P2)")
    }

    /// Unfinished records (`watched * 100 < duration * finished_pct`), newest first,
    /// at most `limit`. Filled in by P2.
    #[allow(clippy::unimplemented)]
    pub fn continue_watching(&self, _finished_pct: u32, _limit: usize) -> Vec<ProgressRecord> {
        unimplemented!("flox_core::progress::ProgressStore::continue_watching (P2)")
    }

    /// Removes every record and saves. Filled in by P2.
    pub fn clear(&self) -> Result<()> {
        Err(Error::NotImplemented(
            "flox_core::progress::ProgressStore::clear",
        ))
    }
}
