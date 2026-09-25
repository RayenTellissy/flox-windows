//! Native file pickers (rfd). Filled in by piece P14.

use std::path::PathBuf;

/// Video files chosen by the user; empty when cancelled. Filled in by P14.
#[allow(clippy::unimplemented)]
pub async fn pick_files(_multi: bool) -> Vec<PathBuf> {
    unimplemented!("flox_sys::dialogs::pick_files (P14)")
}
