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

#[cfg(windows)]
pub mod host;
#[cfg(windows)]
pub mod page;
#[cfg(windows)]
pub mod sniffer;

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
}
