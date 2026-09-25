//! [`Sniffer`] over the hidden WebView2 host. Filled in by piece P13.

use async_trait::async_trait;
use flox_core::error::{Error, Result};
use flox_core::sniff::{SniffMode, SniffResult, Sniffer};
use tokio_util::sync::CancellationToken;

/// The WebView2-backed sniffer.
pub struct WebView2Sniffer {
    _private: (),
}

#[async_trait]
impl Sniffer for WebView2Sniffer {
    async fn sniff(
        &self,
        _page_url: &str,
        _mode: SniffMode,
        _cancel: CancellationToken,
    ) -> Result<SniffResult> {
        Err(Error::NotImplemented(
            "flox_web::sniffer::WebView2Sniffer::sniff",
        ))
    }
}
