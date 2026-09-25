//! Where Flox keeps its files. The platform supplies [`Dirs`] (`flox-sys`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};

/// Platform base directories, already suffixed with the app name.
pub trait Dirs: Send + Sync {
    /// Roaming settings (`%APPDATA%\Flox`).
    fn config(&self) -> PathBuf;
    /// Machine-local data (`%LOCALAPPDATA%\Flox`).
    fn local_data(&self) -> PathBuf;
    /// Scratch space (`%TEMP%\flox`).
    fn temp(&self) -> PathBuf;
    /// The directory holding the executable (bundled DLLs and `tools\`).
    fn app_dir(&self) -> PathBuf;
}

/// Every concrete path the app uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    pub settings: PathBuf,
    pub progress: PathBuf,
    pub tdlib_db: PathBuf,
    pub tdlib_files: PathBuf,
    pub image_cache: PathBuf,
    pub temp: PathBuf,
}

impl AppPaths {
    /// Lays the files out under the platform directories.
    pub fn from_dirs(d: &dyn Dirs) -> AppPaths {
        let config = d.config();
        let local = d.local_data();
        AppPaths {
            settings: config.join("settings.json"),
            progress: config.join("progress.json"),
            tdlib_db: local.join("tdlib").join("db"),
            tdlib_files: local.join("tdlib").join("files"),
            image_cache: local.join("cache").join("images"),
            temp: d.temp(),
        }
    }
}

/// Writes `bytes` to `path` atomically: the data goes to a sibling temp file, is flushed
/// to disk, then renamed over `path` (which replaces an existing file on Windows too).
/// Missing parent directories are created.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| Error::Other(format!("not a file path: {}", path.display())))?;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir)?;
    let mut tmp_name = name.to_os_string();
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    tmp_name.push(format!(".{}-{seq}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let written = (|| -> Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl Dirs for Fake {
        fn config(&self) -> PathBuf {
            PathBuf::from("/c")
        }
        fn local_data(&self) -> PathBuf {
            PathBuf::from("/l")
        }
        fn temp(&self) -> PathBuf {
            PathBuf::from("/t")
        }
        fn app_dir(&self) -> PathBuf {
            PathBuf::from("/a")
        }
    }

    #[test]
    fn layout() {
        let p = AppPaths::from_dirs(&Fake);
        assert_eq!(p.settings, PathBuf::from("/c/settings.json"));
        assert_eq!(p.tdlib_db, PathBuf::from("/l/tdlib/db"));
        assert_eq!(p.image_cache, PathBuf::from("/l/cache/images"));
        assert_eq!(p.temp, PathBuf::from("/t"));
    }

    #[test]
    fn write_atomic_creates_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("a.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() != "a.json")
            .collect();
        assert!(leftovers.is_empty());
    }
}
