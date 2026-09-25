//! Where Flox keeps its files. The platform supplies [`Dirs`] (`flox-sys`).

use std::path::PathBuf;

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
}
