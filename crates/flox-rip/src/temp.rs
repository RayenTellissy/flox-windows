//! Per-job scratch folders under `%TEMP%\flox`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use flox_core::error::Result;
use uuid::Uuid;

/// `<root>\<uuid>`.
pub fn job_dir(root: &Path, id: Uuid) -> PathBuf {
    root.join(id.to_string())
}

/// Deletes leftover job folders (run at launch).
///
/// Everything under `root` goes; `root` itself stays. A missing root is fine. An entry that
/// cannot be removed (still open elsewhere, for example) is logged and skipped.
pub fn sweep(root: &Path) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let removed = match entry.file_type() {
            Ok(t) if t.is_dir() => fs::remove_dir_all(&path),
            _ => fs::remove_file(&path),
        };
        if let Err(e) = removed {
            if e.kind() != ErrorKind::NotFound {
                tracing::warn!("temp sweep: could not remove {}: {e}", path.display());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_dir_is_root_and_uuid() {
        let id = Uuid::nil();
        assert_eq!(
            job_dir(Path::new("/t/flox"), id),
            PathBuf::from("/t/flox/00000000-0000-0000-0000-000000000000")
        );
    }

    #[test]
    fn sweep_empties_the_root() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("flox");
        sweep(&root)?;
        let job = job_dir(&root, Uuid::new_v4());
        fs::create_dir_all(job.join("nested"))?;
        fs::write(job.join("media.mp4"), b"x")?;
        fs::write(job.join("nested").join("v.m4s"), b"y")?;
        fs::write(root.join("stray.tmp"), b"z")?;
        sweep(&root)?;
        assert!(root.is_dir());
        assert_eq!(fs::read_dir(&root)?.count(), 0);
        Ok(())
    }
}
