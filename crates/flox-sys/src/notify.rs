//! Toast notifications (WinRT on Windows).
//!
//! Identity: an unpackaged (zip) app has no AppUserModelID the toast platform
//! knows about. Flox uses the AUMID `Flox`: [`ensure_app_identity`] sets it as
//! the process AUMID and, when missing, creates
//! `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Flox.lnk` pointing at the
//! running exe with `System.AppUserModel.ID = Flox`. Toasts are then shown as
//! "Flox". Until the identity is in place (never called, or it failed) toasts
//! fall back to the PowerShell identity so they still appear.
//!
//! Off Windows both functions are no-ops logged at debug.

use std::sync::atomic::{AtomicBool, Ordering};

use flox_core::error::Result;

/// The AppUserModelID toasts and the taskbar use.
pub const APP_ID: &str = "Flox";

/// Set once the process AUMID and the Start menu shortcut are both in place.
static IDENTITY_READY: AtomicBool = AtomicBool::new(false);

/// Registers the `Flox` identity. Call once at startup, before any window is
/// created (the process AUMID also groups the taskbar button).
pub fn ensure_app_identity() -> Result<()> {
    platform::ensure_app_identity()?;
    IDENTITY_READY.store(true, Ordering::Release);
    Ok(())
}

/// Shows a toast such as "Queue finished".
pub fn toast(title: &str, body: &str) -> Result<()> {
    platform::toast(IDENTITY_READY.load(Ordering::Acquire), title, body)
}

#[cfg(windows)]
mod platform {
    use std::path::Path;

    use flox_core::error::{Error, Result};
    use tauri_winrt_notification::{Duration, Sound, Toast};
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{
        FOLDERID_Programs, IShellLinkW, SetCurrentProcessExplicitAppUserModelID, ShellLink,
    };

    use super::APP_ID;
    use crate::dirs;

    fn win(context: &str, err: windows::core::Error) -> Error {
        Error::Other(format!("{context}: {err}"))
    }

    pub(super) fn ensure_app_identity() -> Result<()> {
        let id = HSTRING::from(APP_ID);
        // SAFETY: `id` is a valid NUL-terminated wide string for the call.
        unsafe { SetCurrentProcessExplicitAppUserModelID(&id) }
            .map_err(|e| win("SetCurrentProcessExplicitAppUserModelID", e))?;

        let programs = dirs::platform::known_folder(&FOLDERID_Programs)
            .ok_or_else(|| Error::Other("Start menu Programs folder not found".into()))?;
        let link = programs.join(format!("{APP_ID}.lnk"));
        if link.exists() {
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        // COM is set up on a short-lived thread of its own so the caller's
        // apartment (the UI thread, a tokio worker) is left untouched.
        let worker = std::thread::Builder::new()
            .name("flox-shortcut".into())
            .spawn(move || create_shortcut(&link, &exe))?;
        worker
            .join()
            .map_err(|_| Error::Other("shortcut thread panicked".into()))??;
        tracing::info!("created the Start menu shortcut for toast identity");
        Ok(())
    }

    /// Balances a successful `CoInitializeEx` on this thread.
    struct ComApartment;

    impl Drop for ComApartment {
        fn drop(&mut self) {
            // SAFETY: only constructed after CoInitializeEx succeeded on this
            // same thread, so this call balances it.
            unsafe { CoUninitialize() };
        }
    }

    fn create_shortcut(link: &Path, exe: &Path) -> Result<()> {
        // SAFETY: first COM call on a fresh thread; balanced by `ComApartment`.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|e| win("CoInitializeEx", e))?;
        let _apartment = ComApartment;

        // SAFETY: COM is initialised on this thread; ShellLink is the in-proc
        // CLSID that implements IShellLinkW.
        let shell_link: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| win("CoCreateInstance(ShellLink)", e))?;
        let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        // SAFETY: every argument is an HSTRING that outlives its call.
        unsafe {
            shell_link
                .SetPath(&HSTRING::from(exe))
                .map_err(|e| win("SetPath", e))?;
            shell_link
                .SetWorkingDirectory(&HSTRING::from(exe_dir.as_path()))
                .map_err(|e| win("SetWorkingDirectory", e))?;
            shell_link
                .SetDescription(&HSTRING::from(APP_ID))
                .map_err(|e| win("SetDescription", e))?;
        }

        let store: IPropertyStore = shell_link.cast().map_err(|e| win("IPropertyStore", e))?;
        let value = PROPVARIANT::from(APP_ID);
        // SAFETY: the key and value are valid for the calls; the store copies
        // the value, and `value` frees its own string on drop.
        unsafe {
            store
                .SetValue(&PKEY_AppUserModel_ID, &value)
                .map_err(|e| win("SetValue(AppUserModel.ID)", e))?;
            store
                .Commit()
                .map_err(|e| win("IPropertyStore::Commit", e))?;
        }

        let file: IPersistFile = shell_link.cast().map_err(|e| win("IPersistFile", e))?;
        // SAFETY: the path is an HSTRING valid for the call.
        unsafe { file.Save(&HSTRING::from(link), true) }
            .map_err(|e| win("IPersistFile::Save", e))?;
        Ok(())
    }

    pub(super) fn toast(identity_ready: bool, title: &str, body: &str) -> Result<()> {
        let app_id = if identity_ready {
            APP_ID
        } else {
            Toast::POWERSHELL_APP_ID
        };
        Toast::new(app_id)
            .title(title)
            .text1(body)
            .sound(Some(Sound::Default))
            .duration(Duration::Short)
            .show()
            .map_err(|e| Error::Other(format!("toast: {e}")))
    }
}

#[cfg(not(windows))]
mod platform {
    use flox_core::error::Result;

    pub(super) fn ensure_app_identity() -> Result<()> {
        tracing::debug!("app identity: no-op on this platform");
        Ok(())
    }

    pub(super) fn toast(_identity_ready: bool, title: &str, body: &str) -> Result<()> {
        tracing::debug!(title, body, "toast (no-op on this platform)");
        Ok(())
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn no_op_off_windows() {
        ensure_app_identity().expect("identity");
        toast("Queue finished", "3 jobs").expect("toast");
    }
}
