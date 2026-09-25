//! The request/update seam every TDLib consumer uses, so tests can script a fake.

use std::sync::Arc;

use async_trait::async_trait;
use flox_core::error::{Error, Result};

/// A TDLib connection.
#[async_trait]
pub trait TdTransport: Send + Sync {
    /// Sends `req` with a fresh `@extra` and waits for the matching response.
    /// A `{"@type":"error"}` response becomes `Err(Error::Td { code, message })`.
    async fn request(&self, req: serde_json::Value) -> Result<serde_json::Value>;

    /// Every update (anything without an `@extra`).
    fn updates(&self) -> tokio::sync::broadcast::Receiver<Arc<serde_json::Value>>;

    /// Closes the current TDLib instance and starts a fresh one on the same
    /// update stream, as the login screen's Try again does. Transports that
    /// cannot restart return [`Error::Unavailable`].
    async fn restart(&self) -> Result<()> {
        Err(Error::Unavailable(
            "this TDLib transport cannot restart".into(),
        ))
    }
}

/// Reads an integer TDLib field that may arrive as a JSON number or, for
/// 64-bit types, as a decimal string.
pub fn json_i64(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Maps a TDLib `{"@type":"error"}` object to [`Error::Td`], or `None` for anything else.
pub fn td_error(v: &serde_json::Value) -> Option<Error> {
    if v.get("@type").and_then(serde_json::Value::as_str) != Some("error") {
        return None;
    }
    let code = v
        .get("code")
        .and_then(json_i64)
        .and_then(|c| i32::try_from(c).ok())
        .unwrap_or(0);
    let message = v
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some(Error::Td { code, message })
}
