//! WebView2 runtime detection (`GetAvailableCoreWebView2BrowserVersionString`).

/// The installed Evergreen runtime version, or `None` when it is missing.
/// Always `None` off Windows. The Windows lookup is filled in by piece P14.
#[cfg(windows)]
#[allow(clippy::unimplemented)]
pub fn runtime_version() -> Option<String> {
    unimplemented!("flox_sys::webview2::runtime_version (P14)")
}

/// The installed Evergreen runtime version, or `None` when it is missing.
/// Always `None` off Windows.
#[cfg(not(windows))]
pub fn runtime_version() -> Option<String> {
    None
}
