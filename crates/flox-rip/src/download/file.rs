//! Resumable single-file download with is-a-file detection. Filled in by piece P8.

use std::path::Path;

use flox_core::error::{Error, Result};
use tokio_util::sync::CancellationToken;

/// The URL served a web page (or other non-media content), not a file.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[error("not a plain file")]
pub struct NotAFile;

/// Downloads `url` to `out` with Range resume. `progress` gets (bytes, total).
pub async fn download(
    _url: &str,
    _headers: &[(String, String)],
    _out: &Path,
    _progress: impl Fn(u64, Option<u64>) + Send,
    _cancel: CancellationToken,
) -> Result<()> {
    Err(Error::NotImplemented("flox_rip::download::file::download"))
}
