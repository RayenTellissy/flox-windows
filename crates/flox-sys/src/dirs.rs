//! Base folders. Windows: `FOLDERID_RoamingAppData\Flox`, `FOLDERID_LocalAppData\Flox`,
//! `GetTempPath2W\flox` and the exe folder. macOS dev: `~/Library/Application Support/Flox-dev`,
//! `~/Library/Caches/Flox-dev`, `$TMPDIR/flox`. Filled in by piece P14.

use std::path::PathBuf;

use flox_core::paths::Dirs;

/// The system folders for this platform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemDirs;

#[allow(clippy::unimplemented)]
impl Dirs for SystemDirs {
    fn config(&self) -> PathBuf {
        unimplemented!("flox_sys::dirs::SystemDirs::config (P14)")
    }

    fn local_data(&self) -> PathBuf {
        unimplemented!("flox_sys::dirs::SystemDirs::local_data (P14)")
    }

    fn temp(&self) -> PathBuf {
        unimplemented!("flox_sys::dirs::SystemDirs::temp (P14)")
    }

    fn app_dir(&self) -> PathBuf {
        unimplemented!("flox_sys::dirs::SystemDirs::app_dir (P14)")
    }
}
