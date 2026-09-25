//! The VidLink web path. Policy, scripts and bridge parsing are platform-neutral;
//! the WebView2 host, sniffer and page player exist only on Windows.

use std::sync::Arc;

use async_trait::async_trait;
use flox_core::error::{Error, Result};
use flox_core::sniff::{SniffMode, SniffResult, Sniffer};
use tokio_util::sync::CancellationToken;

pub mod assets;
pub mod bridge;
pub mod policy;

// The pure parts of these (commands, sniff state, page scripts) build everywhere so they are
// tested on every platform; `WebHost`, `WebView2Sniffer` and `PagePlayer` exist only on Windows.
pub mod host;
pub mod page;
pub mod sniffer;

/// The platform sniffer: WebView2 on Windows (its host starts on the first sniff, which fails
/// with [`Error::Unavailable`] when the runtime is missing), [`unavailable_sniffer`] elsewhere.
pub fn platform_sniffer(opts: assets::ScriptOptions) -> Arc<dyn Sniffer> {
    #[cfg(windows)]
    {
        Arc::new(sniffer::WebView2Sniffer::new(opts))
    }
    #[cfg(not(windows))]
    {
        let _ = opts;
        unavailable_sniffer()
    }
}

/// A sniffer that always fails. Used on macOS dev builds and when the WebView2
/// runtime is missing on Windows.
pub fn unavailable_sniffer() -> Arc<dyn Sniffer> {
    Arc::new(Unavailable)
}

struct Unavailable;

#[async_trait]
impl Sniffer for Unavailable {
    async fn sniff(
        &self,
        _page_url: &str,
        _mode: SniffMode,
        _cancel: CancellationToken,
    ) -> Result<SniffResult> {
        Err(Error::Unavailable("WebView2 is not available".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_sniffer_fails() {
        let s = unavailable_sniffer();
        let r = s
            .sniff(
                "https://vidlink.pro/movie/1",
                SniffMode::Playback,
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(r, Err(Error::Unavailable(_))));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn platform_sniffer_is_unavailable_off_windows() {
        let s = platform_sniffer(assets::ScriptOptions::default());
        let r = s
            .sniff(
                "https://vidlink.pro/tv/1399/1/1",
                SniffMode::Rip,
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(r, Err(Error::Unavailable(_))));
    }
}
