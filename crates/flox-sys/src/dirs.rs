//! Base folders.
//!
//! - Windows: `FOLDERID_RoamingAppData\Flox`, `FOLDERID_LocalAppData\Flox`,
//!   `GetTempPath2W\flox` (falling back to the std temp folder on builds
//!   without that export) and the exe folder.
//! - Elsewhere (macOS development builds): `~/Library/Application Support/Flox-dev`,
//!   `~/Library/Caches/Flox-dev` and `$TMPDIR/flox`. Setting `FLOX_HOME` moves
//!   all three under that folder (`config`, `data`, `temp`).

use std::path::{Path, PathBuf};

use flox_core::paths::Dirs;

/// Development override for the base folders (ignored on Windows).
pub const HOME_ENV: &str = "FLOX_HOME";

/// The system folders for this platform. Resolved on every call, so they
/// follow `FLOX_HOME`; the app resolves them once through `AppPaths::from_dirs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemDirs;

impl Dirs for SystemDirs {
    fn config(&self) -> PathBuf {
        platform::config()
    }

    fn local_data(&self) -> PathBuf {
        platform::local_data()
    }

    fn temp(&self) -> PathBuf {
        platform::temp()
    }

    fn app_dir(&self) -> PathBuf {
        exe_dir()
    }
}

/// The folder holding the running executable, or `.` when it cannot be found.
pub(crate) fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(windows)]
pub(crate) mod platform {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use windows::core::{s, w, GUID};
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::UI::Shell::{
        FOLDERID_LocalAppData, FOLDERID_RoamingAppData, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG,
    };

    const APP: &str = "Flox";

    pub(crate) fn config() -> PathBuf {
        known_folder(&FOLDERID_RoamingAppData)
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .unwrap_or_else(std::env::temp_dir)
            .join(APP)
    }

    pub(crate) fn local_data() -> PathBuf {
        known_folder(&FOLDERID_LocalAppData)
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
            .unwrap_or_else(std::env::temp_dir)
            .join(APP)
    }

    pub(crate) fn temp() -> PathBuf {
        temp_path2().unwrap_or_else(std::env::temp_dir).join("flox")
    }

    /// `SHGetKnownFolderPath` for the current user, without creating the folder.
    pub(crate) fn known_folder(id: &GUID) -> Option<PathBuf> {
        // SAFETY: `id` points to a valid KNOWNFOLDERID for the duration of the
        // call and no access token is passed (current user).
        let raw = match unsafe { SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None) } {
            Ok(raw) => raw,
            Err(err) => {
                tracing::warn!(?id, %err, "SHGetKnownFolderPath failed");
                return None;
            }
        };
        if raw.is_null() {
            return None;
        }
        // SAFETY: on success `raw` is a NUL-terminated wide string allocated by
        // the shell; it is copied here before being freed below.
        let path = PathBuf::from(OsString::from_wide(unsafe { raw.as_wide() }));
        // SAFETY: the buffer came from SHGetKnownFolderPath, which requires the
        // caller to release it with CoTaskMemFree; it is not used afterwards.
        unsafe { CoTaskMemFree(Some(raw.0 as *const c_void)) };
        (!path.as_os_str().is_empty()).then_some(path)
    }

    type GetTempPath2Fn = unsafe extern "system" fn(u32, *mut u16) -> u32;

    /// `GetTempPath2W`, looked up at run time because older Windows 10 builds
    /// lack the export and a static import would stop the exe from starting.
    fn temp_path2() -> Option<PathBuf> {
        // SAFETY: kernel32 is always loaded in a Win32 process; the name is a
        // static NUL-terminated wide string.
        let kernel32 = unsafe { GetModuleHandleW(w!("kernel32.dll")) }.ok()?;
        // SAFETY: a valid module handle and a static NUL-terminated ANSI name.
        let proc = unsafe { GetProcAddress(kernel32, s!("GetTempPath2W")) }?;
        // SAFETY: GetTempPath2W is `DWORD WINAPI GetTempPath2W(DWORD, LPWSTR)`,
        // which is exactly `GetTempPath2Fn`.
        let get: GetTempPath2Fn = unsafe { std::mem::transmute(proc) };
        let mut buf = vec![0u16; 261];
        loop {
            let cap = u32::try_from(buf.len()).ok()?;
            // SAFETY: `buf` is a writable buffer of exactly `cap` u16 units.
            let len = unsafe { get(cap, buf.as_mut_ptr()) } as usize;
            if len == 0 {
                return None;
            }
            if len < buf.len() {
                buf.truncate(len);
                return Some(PathBuf::from(OsString::from_wide(&buf)));
            }
            // Too small: `len` is the size needed, including the terminator.
            buf = vec![0u16; len + 1];
        }
    }
}

#[cfg(not(windows))]
pub(crate) mod platform {
    use std::path::PathBuf;

    use super::HOME_ENV;

    const APP: &str = "Flox-dev";

    /// The three base folders of a development build.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct DevLayout {
        pub(crate) config: PathBuf,
        pub(crate) local_data: PathBuf,
        pub(crate) temp: PathBuf,
    }

    /// The layout rule: `FLOX_HOME` wins, then `$HOME/Library/...`; with no
    /// home at all the Library folders live under the temp folder.
    pub(crate) fn layout(
        flox_home: Option<PathBuf>,
        home: Option<PathBuf>,
        tmp: PathBuf,
    ) -> DevLayout {
        if let Some(root) = flox_home {
            return DevLayout {
                config: root.join("config"),
                local_data: root.join("data"),
                temp: root.join("temp"),
            };
        }
        let library = home
            .unwrap_or_else(|| tmp.join("flox-home"))
            .join("Library");
        DevLayout {
            config: library.join("Application Support").join(APP),
            local_data: library.join("Caches").join(APP),
            temp: tmp.join("flox"),
        }
    }

    fn current() -> DevLayout {
        let non_empty = |key: &str| {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        layout(non_empty(HOME_ENV), non_empty("HOME"), std::env::temp_dir())
    }

    pub(crate) fn config() -> PathBuf {
        current().config
    }

    pub(crate) fn local_data() -> PathBuf {
        current().local_data
    }

    pub(crate) fn temp() -> PathBuf {
        current().temp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_dir_is_the_exe_folder() {
        let exe = std::env::current_exe().expect("current exe");
        assert_eq!(SystemDirs.app_dir(), exe.parent().expect("parent"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_folders_end_with_the_app_name() {
        assert!(SystemDirs.config().ends_with("Flox"));
        assert!(SystemDirs.local_data().ends_with("Flox"));
        assert!(SystemDirs.temp().ends_with("flox"));
    }

    #[cfg(not(windows))]
    mod dev {
        use std::path::PathBuf;

        use super::super::platform::layout;
        use super::*;

        #[test]
        fn library_layout() {
            let l = layout(
                None,
                Some(PathBuf::from("/Users/me")),
                PathBuf::from("/var/tmp/x"),
            );
            assert_eq!(
                l.config,
                PathBuf::from("/Users/me/Library/Application Support/Flox-dev")
            );
            assert_eq!(
                l.local_data,
                PathBuf::from("/Users/me/Library/Caches/Flox-dev")
            );
            assert_eq!(l.temp, PathBuf::from("/var/tmp/x/flox"));
        }

        #[test]
        fn flox_home_override_wins() {
            let l = layout(
                Some(PathBuf::from("/opt/flox")),
                Some(PathBuf::from("/Users/me")),
                PathBuf::from("/var/tmp/x"),
            );
            assert_eq!(l.config, PathBuf::from("/opt/flox/config"));
            assert_eq!(l.local_data, PathBuf::from("/opt/flox/data"));
            assert_eq!(l.temp, PathBuf::from("/opt/flox/temp"));
        }

        #[test]
        fn no_home_falls_back_to_temp() {
            let l = layout(None, None, PathBuf::from("/t"));
            assert_eq!(
                l.config,
                PathBuf::from("/t/flox-home/Library/Application Support/Flox-dev")
            );
            assert_eq!(l.temp, PathBuf::from("/t/flox"));
        }

        /// The only test in this crate that touches `FLOX_HOME`.
        #[test]
        fn system_dirs_follow_flox_home_env() {
            let root = tempfile::tempdir().expect("tempdir");
            std::env::set_var(HOME_ENV, root.path());
            let (config, local, temp) = (
                SystemDirs.config(),
                SystemDirs.local_data(),
                SystemDirs.temp(),
            );
            let paths = flox_core::paths::AppPaths::from_dirs(&SystemDirs);
            std::env::remove_var(HOME_ENV);

            assert_eq!(config, root.path().join("config"));
            assert_eq!(local, root.path().join("data"));
            assert_eq!(temp, root.path().join("temp"));
            assert_eq!(
                paths.settings,
                root.path().join("config").join("settings.json")
            );
            assert_eq!(
                paths.tdlib_db,
                root.path().join("data").join("tdlib").join("db")
            );
        }
    }
}
