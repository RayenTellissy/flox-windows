//! Poster and still cache: memory LRU of decoded RGBA plus a disk cache of the
//! encoded bytes named by `sha1(url)`. Filled in by piece P3.

use std::path::PathBuf;
use std::sync::Arc;

use crate::error::{Error, Result};

/// Default memory budget for decoded images (desktop; Android used min(maxMem/8, 24 MB)).
pub const MEM_BYTES: usize = 96 * 1024 * 1024;
/// Default disk budget.
pub const DISK_BYTES: u64 = 40 * 1024 * 1024;
/// The disk cache is trimmed to this percentage of its budget, oldest mtime first.
pub const TRIM_TO_PERCENT: u64 = 75;
/// The disk trim runs every this many writes.
pub const TRIM_EVERY_WRITES: u32 = 16;
/// At most this many downloads run at once.
pub const MAX_CONCURRENT_DOWNLOADS: usize = 4;

/// A decoded image, 8-bit RGBA, row-major, no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The image cache.
pub struct ImageCache {
    #[allow(dead_code)]
    dir: PathBuf,
    #[allow(dead_code)]
    mem_bytes: usize,
    #[allow(dead_code)]
    disk_bytes: u64,
}

impl ImageCache {
    /// A cache rooted at `dir` with the given budgets. Nothing touches the disk until `get`.
    pub fn new(dir: PathBuf, mem_bytes: usize, disk_bytes: u64) -> Self {
        Self {
            dir,
            mem_bytes,
            disk_bytes,
        }
    }

    /// Memory, then disk, then network; decoded off the calling task. Filled in by P3.
    pub async fn get(&self, _url: &str) -> Result<Arc<Rgba>> {
        Err(Error::NotImplemented("flox_core::images::ImageCache::get"))
    }
}
