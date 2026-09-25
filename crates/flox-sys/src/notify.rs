//! Toast notifications (WinRT on Windows). Filled in by piece P14.

use flox_core::error::{Error, Result};

/// Shows a toast such as "Queue finished".
pub fn toast(_title: &str, _body: &str) -> Result<()> {
    Err(Error::NotImplemented("flox_sys::notify::toast"))
}
