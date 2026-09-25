//! Sending a document to the channel with progress and cancel. Filled in by piece P5.

use std::path::PathBuf;

use flox_core::error::{Error, Result};
use tokio_util::sync::CancellationToken;

use crate::transport::TdTransport;

/// One document to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRequest {
    pub chat_id: i64,
    pub path: PathBuf,
    pub caption: String,
    pub reply_to: Option<i64>,
}

/// Sends `req` and returns the final (server) message id. `progress` gets 0.0..=1.0.
pub async fn send_document(
    _t: &dyn TdTransport,
    _req: UploadRequest,
    _progress: impl Fn(f32) + Send + Sync,
    _cancel: CancellationToken,
) -> Result<i64> {
    Err(Error::NotImplemented("flox_td::upload::send_document"))
}
