//! Per-job scratch folders under `%TEMP%\flox`. `sweep` is filled in by piece P7.

use std::path::{Path, PathBuf};

use flox_core::error::{Error, Result};
use uuid::Uuid;

/// `<root>\<uuid>`.
pub fn job_dir(root: &Path, id: Uuid) -> PathBuf {
    root.join(id.to_string())
}

/// Deletes leftover job folders (run at launch).
pub fn sweep(_root: &Path) -> Result<()> {
    Err(Error::NotImplemented("flox_rip::temp::sweep"))
}
