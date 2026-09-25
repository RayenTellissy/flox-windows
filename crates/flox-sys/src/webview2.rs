//! WebView2 runtime detection (`GetAvailableCoreWebView2BrowserVersionString`).

/// The installed Evergreen runtime version (for example `128.0.2739.42`), or
/// `None` when it is missing.
#[cfg(windows)]
pub fn runtime_version() -> Option<String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::GetAvailableCoreWebView2BrowserVersionString;
    use windows::core::{PCWSTR, PWSTR};

    let mut raw = PWSTR::null();
    // SAFETY: a null folder asks for the installed Evergreen runtime and `raw`
    // is a valid out pointer for the call.
    let result = unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut raw) };
    // `take_pwstr` frees the CoTaskMem buffer (also on the error path, if any).
    let version = (!raw.is_null()).then(|| webview2_com::take_pwstr(raw));
    if let Err(err) = result {
        tracing::debug!(%err, "WebView2 runtime lookup failed");
        return None;
    }
    version.filter(|v| !v.is_empty())
}

/// The installed Evergreen runtime version, or `None` when it is missing.
/// Always `None` off Windows.
#[cfg(not(windows))]
pub fn runtime_version() -> Option<String> {
    None
}

#[cfg(all(test, not(windows)))]
mod tests {
    #[test]
    fn none_off_windows() {
        assert_eq!(super::runtime_version(), None);
    }
}
