//! The TDLib client: a receive thread routing `@extra` responses and broadcasting
//! updates. Filled in by piece P4.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use flox_core::error::{Error, Result};
use tokio::sync::broadcast;

use crate::ffi::TdJson;
use crate::transport::TdTransport;

/// Capacity of the update broadcast channel.
pub const UPDATE_CHANNEL_CAPACITY: usize = 1024;

/// `setTdlibParameters` inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdParams {
    pub api_id: i32,
    pub api_hash: String,
    pub db_dir: PathBuf,
    pub files_dir: PathBuf,
    pub device_model: String,
    pub app_version: String,
}

/// One TDLib client for the whole app.
pub struct TdClient {
    updates: broadcast::Sender<Arc<serde_json::Value>>,
}

impl TdClient {
    /// Creates the client id and starts the receive thread. Filled in by P4.
    pub fn start(_lib: TdJson, _params: TdParams) -> Result<Arc<TdClient>> {
        Err(Error::NotImplemented("flox_td::client::TdClient::start"))
    }

    /// Sends `close` and waits for `authorizationStateClosed`. Filled in by P4.
    pub async fn close(&self) -> Result<()> {
        Err(Error::NotImplemented("flox_td::client::TdClient::close"))
    }
}

#[async_trait]
impl TdTransport for TdClient {
    async fn request(&self, _req: serde_json::Value) -> Result<serde_json::Value> {
        Err(Error::NotImplemented("flox_td::client::TdClient::request"))
    }

    fn updates(&self) -> broadcast::Receiver<Arc<serde_json::Value>> {
        self.updates.subscribe()
    }
}
