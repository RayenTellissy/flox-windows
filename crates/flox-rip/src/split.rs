//! Splitting a file into Telegram-sized parts in place. Filled in by piece P7.

use std::path::{Path, PathBuf};

use flox_core::error::{Error, Result};

/// Telegram's 2 GB document limit (decimal).
pub const PART_SIZE: u64 = 2_000_000_000;

/// Carves parts from the end, truncating the source; never two copies on disk.
/// Returns `media.part1.mp4`… in order, or the file unchanged when it fits.
pub fn split(_file: &Path, _part_size: u64) -> Result<Vec<PathBuf>> {
    Err(Error::NotImplemented("flox_rip::split::split"))
}
