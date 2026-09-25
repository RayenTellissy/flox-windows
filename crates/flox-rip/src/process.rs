//! Running a tool with merged, line-split output and kill-on-cancel
//! (`CREATE_NO_WINDOW` on Windows). Filled in by piece P7.

use std::ffi::OsString;
use std::path::Path;

use flox_core::error::{Error, Result};
use tokio_util::sync::CancellationToken;

/// Runs `cmd args`, calling `on_line` for each stdout/stderr line (`\r` also splits).
/// A non-zero exit is an error carrying the last lines.
pub async fn run(
    _cmd: &Path,
    _args: &[OsString],
    _on_line: impl FnMut(&str) + Send,
    _cancel: CancellationToken,
) -> Result<()> {
    Err(Error::NotImplemented("flox_rip::process::run"))
}
