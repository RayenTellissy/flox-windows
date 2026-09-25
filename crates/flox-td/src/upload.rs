//! Sending a document to the channel with progress and cancel.
//!
//! `sendMessage` answers at once with a temporary message; TDLib then uploads
//! the file, reporting `updateFile`, and finishes with
//! `updateMessageSendSucceeded` (temporary id → server id) or
//! `updateMessageSendFailed`.

use std::path::PathBuf;

use flox_core::error::{Error, Result};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use crate::transport::{json_i64, TdTransport};

/// One document to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRequest {
    pub chat_id: i64,
    pub path: PathBuf,
    pub caption: String,
    pub reply_to: Option<i64>,
}

impl UploadRequest {
    /// The TDLib 1.8.67 `sendMessage` request.
    pub fn to_request(&self) -> Value {
        let mut req = json!({
            "@type": "sendMessage",
            "chat_id": self.chat_id,
            "input_message_content": {
                "@type": "inputMessageDocument",
                "document": {
                    "@type": "inputDocument",
                    "document": {
                        "@type": "inputFileLocal",
                        "path": self.path.to_string_lossy(),
                    },
                    "disable_content_type_detection": true,
                },
                "caption": {"@type": "formattedText", "text": self.caption},
            },
        });
        if let (Some(to), Some(obj)) = (self.reply_to, req.as_object_mut()) {
            obj.insert(
                "reply_to".into(),
                json!({"@type": "inputMessageReplyToMessage", "message_id": to}),
            );
        }
        req
    }
}

/// The file id of a document message's file.
fn document_file_id(msg: &Value) -> Option<i64> {
    msg.get("content")?
        .get("document")?
        .get("document")?
        .get("id")
        .and_then(json_i64)
}

/// `uploaded_size / total` for an `updateFile` of `file_id`; the total is the
/// file's size, else its expected size, else `fallback_total`.
fn upload_fraction(u: &Value, file_id: i64, fallback_total: u64) -> Option<f32> {
    let f = u.get("file")?;
    if f.get("id").and_then(json_i64) != Some(file_id) {
        return None;
    }
    let uploaded = f
        .get("remote")
        .and_then(|r| r.get("uploaded_size"))
        .and_then(json_i64)?;
    let total = ["size", "expected_size"]
        .iter()
        .filter_map(|k| f.get(*k).and_then(json_i64))
        .find(|s| *s > 0)
        .or_else(|| i64::try_from(fallback_total).ok())
        .filter(|s| *s > 0)?;
    #[allow(clippy::cast_precision_loss)]
    let frac = uploaded as f64 / total as f64;
    #[allow(clippy::cast_possible_truncation)]
    Some(frac.clamp(0.0, 1.0) as f32)
}

/// Reads the TDLib error of an `updateMessageSendFailed` (1.8.67 `error`
/// object, or the older `error_code`/`error_message` pair).
fn send_error(u: &Value) -> Error {
    let (code, message) = match u.get("error") {
        Some(e) => (e.get("code"), e.get("message")),
        None => (u.get("error_code"), u.get("error_message")),
    };
    Error::Td {
        code: code
            .and_then(json_i64)
            .and_then(|c| i32::try_from(c).ok())
            .unwrap_or(0),
        message: message
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .unwrap_or("send failed")
            .to_owned(),
    }
}

/// Sends `req` and returns the final (server) message id. `progress` gets 0.0..=1.0.
///
/// Cancelling deletes the temporary message, which stops the upload, and
/// returns [`Error::Cancelled`].
pub async fn send_document(
    t: &dyn TdTransport,
    req: UploadRequest,
    progress: impl Fn(f32) + Send + Sync,
    cancel: CancellationToken,
) -> Result<i64> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let local_size = tokio::fs::metadata(&req.path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    // Subscribed before sending so no update about the new message is missed.
    let mut rx = t.updates();
    let sent = t.request(req.to_request()).await?;
    let temp_id = sent
        .get("id")
        .and_then(json_i64)
        .ok_or_else(|| Error::Other("sendMessage returned no message id".into()))?;
    let file_id = document_file_id(&sent);
    progress(0.0);

    let failed_at_once = sent
        .get("sending_state")
        .and_then(|s| s.get("@type"))
        .and_then(Value::as_str)
        == Some("messageSendingStateFailed");
    if failed_at_once {
        let err = sent.get("sending_state").map(send_error);
        return Err(err.unwrap_or_else(|| Error::Other("send failed".into())));
    }

    loop {
        let u = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                let del = t
                    .request(json!({
                        "@type": "deleteMessages",
                        "chat_id": req.chat_id,
                        "message_ids": [temp_id],
                        "revoke": true,
                    }))
                    .await;
                if let Err(e) = del {
                    tracing::warn!("could not delete cancelled upload {temp_id}: {e}");
                }
                return Err(Error::Cancelled);
            }
            u = rx.recv() => u,
        };
        let u = match u {
            Ok(u) => u,
            Err(RecvError::Lagged(n)) => {
                tracing::warn!("upload of {temp_id} missed {n} updates");
                continue;
            }
            Err(RecvError::Closed) => {
                return Err(Error::Other("TDLib update stream closed".into()));
            }
        };
        let old_id = || u.get("old_message_id").and_then(json_i64);
        match u.get("@type").and_then(Value::as_str) {
            Some("updateFile") => {
                if let Some(frac) = file_id.and_then(|f| upload_fraction(&u, f, local_size)) {
                    progress(frac);
                }
            }
            Some("updateMessageSendSucceeded") if old_id() == Some(temp_id) => {
                let new_id = u
                    .get("message")
                    .and_then(|m| m.get("id"))
                    .and_then(json_i64)
                    .ok_or_else(|| {
                        Error::Other("updateMessageSendSucceeded without a message id".into())
                    })?;
                progress(1.0);
                return Ok(new_id);
            }
            Some("updateMessageSendFailed") if old_id() == Some(temp_id) => {
                return Err(send_error(&u));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;

    use super::*;
    use crate::auth::tests::Fake;

    fn req(reply_to: Option<i64>) -> UploadRequest {
        UploadRequest {
            chat_id: -1001,
            path: PathBuf::from("/nonexistent/flox/S01E03.part1.mkv"),
            caption:
                r#"{"codec":"hevc","part":1,"parts":1,"quality":"1080p","tmdb":603,"type":"movie"}"#
                    .into(),
            reply_to,
        }
    }

    /// The temporary message `sendMessage` answers with.
    fn pending(id: i64, file_id: i64) -> Value {
        json!({
            "@type": "message",
            "id": id,
            "chat_id": -1001,
            "sending_state": {"@type": "messageSendingStatePending", "sending_id": 0},
            "content": {
                "@type": "messageDocument",
                "document": {
                    "@type": "document",
                    "file_name": "S01E03.part1.mkv",
                    "document": {"@type": "file", "id": file_id, "size": 1000, "expected_size": 1000}
                },
                "caption": {"@type": "formattedText", "text": "", "entities": []}
            }
        })
    }

    fn file_update(file_id: i64, uploaded: i64) -> Value {
        json!({"@type": "updateFile", "file": {
            "@type": "file", "id": file_id, "size": 1000, "expected_size": 1000,
            "remote": {"@type": "remoteFile", "id": "", "is_uploading_active": true,
                       "is_uploading_completed": false, "uploaded_size": uploaded}
        }})
    }

    fn succeeded(old: i64, new: i64) -> Value {
        json!({"@type": "updateMessageSendSucceeded", "old_message_id": old,
               "message": {"@type": "message", "id": new, "chat_id": -1001}})
    }

    type Seen = Arc<Mutex<Vec<f32>>>;

    fn recorder() -> (Seen, impl Fn(f32) + Send + Sync) {
        let seen: Seen = Arc::default();
        let s = Arc::clone(&seen);
        (seen, move |p| s.lock().push(p))
    }

    #[test]
    fn request_shape_matches_tdlib_1_8_67() {
        let r = req(Some(110)).to_request();
        assert_eq!(r["@type"], "sendMessage");
        assert_eq!(r["chat_id"], -1001);
        let c = &r["input_message_content"];
        assert_eq!(c["@type"], "inputMessageDocument");
        assert_eq!(c["document"]["@type"], "inputDocument");
        assert_eq!(c["document"]["document"]["@type"], "inputFileLocal");
        assert_eq!(
            c["document"]["document"]["path"],
            "/nonexistent/flox/S01E03.part1.mkv"
        );
        assert_eq!(c["document"]["disable_content_type_detection"], true);
        assert_eq!(c["caption"]["@type"], "formattedText");
        assert_eq!(c["caption"]["text"], req(None).caption);
        assert_eq!(r["reply_to"]["@type"], "inputMessageReplyToMessage");
        assert_eq!(r["reply_to"]["message_id"], 110);
        assert!(req(None).to_request().get("reply_to").is_none());
    }

    #[tokio::test]
    async fn reports_progress_and_returns_the_server_id() {
        let fake = Fake::new();
        fake.respond_after(
            "sendMessage",
            vec![
                file_update(77, 250),
                file_update(12, 999),
                // Another upload finishing is not ours.
                succeeded(-5, 500),
                file_update(77, 1000),
                succeeded(-9, 4242),
            ],
            Ok(pending(-9, 77)),
        );
        let (seen, progress) = recorder();
        let id = send_document(fake.as_ref(), req(None), progress, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(id, 4242);
        assert_eq!(*seen.lock(), vec![0.0, 0.25, 1.0, 1.0]);
        assert!(fake.sent("deleteMessages").is_empty());
    }

    #[tokio::test]
    async fn send_failure_carries_the_tdlib_error() {
        let fake = Fake::new();
        fake.respond_after(
            "sendMessage",
            vec![
                file_update(77, 100),
                json!({"@type": "updateMessageSendFailed", "old_message_id": -9,
                       "message": {"@type": "message", "id": -9},
                       "error": {"@type": "error", "code": 400, "message": "FILE_PARTS_INVALID"}}),
            ],
            Ok(pending(-9, 77)),
        );
        let (_, progress) = recorder();
        let r = send_document(fake.as_ref(), req(None), progress, CancellationToken::new()).await;
        match r {
            Err(Error::Td { code, message }) => {
                assert_eq!(code, 400);
                assert_eq!(message, "FILE_PARTS_INVALID");
            }
            other => panic!("expected a TDLib error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_deletes_the_temporary_message() {
        let fake = Fake::new();
        fake.respond_after(
            "sendMessage",
            vec![file_update(77, 100)],
            Ok(pending(-9, 77)),
        );
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c.cancel();
        });
        let (seen, progress) = recorder();
        let r = send_document(fake.as_ref(), req(Some(110)), progress, cancel).await;
        assert!(matches!(r, Err(Error::Cancelled)));
        assert_eq!(*seen.lock(), vec![0.0, 0.1]);
        let del = fake.sent("deleteMessages");
        assert_eq!(del.len(), 1);
        assert_eq!(del[0]["chat_id"], -1001);
        assert_eq!(del[0]["message_ids"], json!([-9]));
        assert_eq!(del[0]["revoke"], true);
    }

    #[tokio::test]
    async fn cancelled_before_start_sends_nothing() {
        let fake = Fake::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let r = send_document(fake.as_ref(), req(None), |_| {}, cancel).await;
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(fake.requests.lock().is_empty());
    }

    #[tokio::test]
    async fn request_errors_propagate() {
        let fake = Fake::new();
        fake.respond(
            "sendMessage",
            Err(Error::Td {
                code: 400,
                message: "Chat not found".into(),
            }),
        );
        let r = send_document(fake.as_ref(), req(None), |_| {}, CancellationToken::new()).await;
        assert!(matches!(r, Err(Error::Td { code: 400, .. })));
    }
}
