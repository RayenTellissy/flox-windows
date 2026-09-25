//! The request/update seam every TDLib consumer uses, so tests can script a fake.

use std::sync::Arc;

use async_trait::async_trait;
use flox_core::error::Result;

/// A TDLib connection.
#[async_trait]
pub trait TdTransport: Send + Sync {
    /// Sends `req` with a fresh `@extra` and waits for the matching response.
    /// A `{"@type":"error"}` response becomes `Err(Error::Td { code, message })`.
    async fn request(&self, req: serde_json::Value) -> Result<serde_json::Value>;

    /// Every update (anything without an `@extra`).
    fn updates(&self) -> tokio::sync::broadcast::Receiver<Arc<serde_json::Value>>;
}
