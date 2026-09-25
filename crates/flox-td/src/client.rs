//! The TDLib client: a receive thread routing `@extra` responses and broadcasting
//! updates.
//!
//! `td_receive` returns messages for every client id in the process, so there
//! must be exactly one [`TdClient`] per process.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flox_core::error::{Error, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{broadcast, oneshot, Notify};

use crate::ffi::TdJson;
use crate::transport::{json_i64, td_error, TdTransport};

/// Capacity of the update broadcast channel.
pub const UPDATE_CHANNEL_CAPACITY: usize = 1024;

/// How long [`TdClient::close`] waits for `authorizationStateClosed`.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);

/// `td_receive` timeout in seconds; also bounds how long the thread takes to stop.
const RECEIVE_TIMEOUT: f64 = 1.0;

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

impl TdParams {
    /// The TDLib 1.8.x `setTdlibParameters` request (flattened fields, no nested
    /// `parameters` object).
    pub fn to_request(&self) -> Value {
        json!({
            "@type": "setTdlibParameters",
            "use_test_dc": false,
            "database_directory": self.db_dir.to_string_lossy(),
            "files_directory": self.files_dir.to_string_lossy(),
            "database_encryption_key": "",
            "use_file_database": true,
            "use_chat_info_database": true,
            "use_message_database": true,
            "use_secret_chats": false,
            "api_id": self.api_id,
            "api_hash": self.api_hash,
            "system_language_code": "en",
            "device_model": self.device_model,
            "system_version": "",
            "application_version": self.app_version,
        })
    }
}

/// The `@type` of an `updateAuthorizationState`'s state, if `v` is one.
pub(crate) fn auth_state_type(v: &Value) -> Option<&str> {
    if v.get("@type").and_then(Value::as_str) != Some("updateAuthorizationState") {
        return None;
    }
    v.get("authorization_state")
        .and_then(|s| s.get("@type"))
        .and_then(Value::as_str)
}

/// A request waiting for its response.
struct Pending {
    client_id: i32,
    tx: oneshot::Sender<Value>,
}

/// State shared by the handle and the receive thread.
struct Inner {
    lib: TdJson,
    params: TdParams,
    client_id: AtomicI32,
    next_extra: AtomicU64,
    pending: Mutex<HashMap<u64, Pending>>,
    updates: broadcast::Sender<Arc<Value>>,
    /// Client ids that reached `authorizationStateClosed`.
    closed: Mutex<HashSet<i32>>,
    closed_notify: Notify,
    running: AtomicBool,
}

impl Inner {
    fn current(&self) -> i32 {
        self.client_id.load(Ordering::SeqCst)
    }

    fn fresh_extra(&self) -> u64 {
        self.next_extra.fetch_add(1, Ordering::Relaxed)
    }

    /// Sends `req` to `client_id` without waiting; the response is logged if it is an error.
    fn fire(&self, client_id: i32, mut req: Value) {
        if let Some(obj) = req.as_object_mut() {
            obj.insert("@extra".into(), json!(self.fresh_extra()));
        }
        self.lib.send(client_id, &req.to_string());
    }

    /// Starts TDLib for `client_id`: it creates the instance on its first request.
    fn kick(&self, client_id: i32) {
        self.fire(client_id, json!({"@type": "getOption", "name": "version"}));
    }

    fn receive_loop(&self) {
        while self.running.load(Ordering::SeqCst) {
            let Some(raw) = self.lib.receive(RECEIVE_TIMEOUT) else {
                continue;
            };
            match serde_json::from_str::<Value>(&raw) {
                Ok(v) => self.dispatch(v),
                Err(e) => tracing::warn!("td_receive: unparsable message: {e}"),
            }
        }
        tracing::debug!("TDLib receive thread stopped");
    }

    fn dispatch(&self, mut v: Value) {
        let from = v
            .get("@client_id")
            .and_then(json_i64)
            .and_then(|c| i32::try_from(c).ok());
        let extra = v.get("@extra").and_then(Value::as_u64);
        if let Some(obj) = v.as_object_mut() {
            obj.remove("@client_id");
            obj.remove("@extra");
        }

        if let Some(extra) = extra {
            let pending = self.pending.lock().remove(&extra);
            match pending {
                Some(p) => {
                    if from.is_some_and(|c| c != p.client_id) {
                        tracing::warn!(
                            "TDLib response {extra} came from client {from:?}, expected {}",
                            p.client_id
                        );
                    }
                    // The requester may have given up; nothing to do then.
                    let _ = p.tx.send(v);
                }
                None => {
                    if let Some(e) = td_error(&v) {
                        tracing::debug!("TDLib background request {extra} failed: {e}");
                    }
                }
            }
            return;
        }

        let state = auth_state_type(&v).map(str::to_owned);
        if state.as_deref() == Some("authorizationStateClosed") {
            if let Some(c) = from {
                self.closed.lock().insert(c);
                self.closed_notify.notify_waiters();
            }
        }

        let current = self.current();
        if from.is_some_and(|c| c != current) {
            // Leftovers from a client that was closed by a restart.
            return;
        }
        if state.as_deref() == Some("authorizationStateWaitTdlibParameters") {
            self.fire(current, self.params.to_request());
        }
        // No subscribers is fine; the update is simply dropped.
        let _ = self.updates.send(Arc::new(v));
    }

    async fn close_current(&self) -> Result<()> {
        let id = self.current();
        if self.closed.lock().contains(&id) {
            return Ok(());
        }
        self.fire(id, json!({"@type": "close"}));
        let deadline = tokio::time::Instant::now() + CLOSE_TIMEOUT;
        loop {
            let notified = self.closed_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.closed.lock().contains(&id) {
                return Ok(());
            }
            tokio::time::timeout_at(deadline, notified)
                .await
                .map_err(|_| Error::Timeout("TDLib close".into()))?;
        }
    }
}

/// One TDLib client for the whole app.
pub struct TdClient {
    inner: Arc<Inner>,
    updates: broadcast::Sender<Arc<Value>>,
}

impl std::fmt::Debug for TdClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TdClient")
            .field("client_id", &self.inner.current())
            .finish_non_exhaustive()
    }
}

impl TdClient {
    /// Creates the client id and starts the receive thread.
    ///
    /// `setTdlibParameters` is sent from the receive thread as soon as TDLib
    /// asks for it, so it is never missed by a late subscriber.
    pub fn start(lib: TdJson, params: TdParams) -> Result<Arc<TdClient>> {
        Self::start_subscribed(lib, params).map(|(c, _)| c)
    }

    /// [`TdClient::start`], plus an update receiver subscribed before TDLib is
    /// started, so it sees the very first `updateAuthorizationState`.
    pub fn start_subscribed(
        lib: TdJson,
        params: TdParams,
    ) -> Result<(Arc<TdClient>, broadcast::Receiver<Arc<Value>>)> {
        let _ = lib.execute(r#"{"@type":"setLogVerbosityLevel","new_verbosity_level":1}"#);
        match version(&lib) {
            Some(v) => tracing::info!("TDLib {v}"),
            None => tracing::warn!("TDLib version unknown"),
        }

        let (updates, rx) = broadcast::channel(UPDATE_CHANNEL_CAPACITY);
        let client_id = lib.create_client_id();
        let inner = Arc::new(Inner {
            lib,
            params,
            client_id: AtomicI32::new(client_id),
            next_extra: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            updates: updates.clone(),
            closed: Mutex::new(HashSet::new()),
            closed_notify: Notify::new(),
            running: AtomicBool::new(true),
        });

        let thread_inner = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("flox-td-receive".into())
            .spawn(move || thread_inner.receive_loop())?;

        inner.kick(client_id);
        Ok((Arc::new(TdClient { inner, updates }), rx))
    }

    /// Sends `close` and waits for `authorizationStateClosed`. Returns at once
    /// if the current instance is already closed.
    pub async fn close(&self) -> Result<()> {
        self.inner.close_current().await
    }

    /// The current TDLib client id.
    pub fn client_id(&self) -> i32 {
        self.inner.current()
    }
}

/// `getOption version` through `td_execute`, such as `"1.8.67"`.
pub fn version(lib: &TdJson) -> Option<String> {
    let raw = lib.execute(r#"{"@type":"getOption","name":"version"}"#)?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("value").and_then(Value::as_str).map(str::to_owned)
}

impl Drop for TdClient {
    fn drop(&mut self) {
        // The receive thread exits within one td_receive timeout.
        self.inner.running.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl TdTransport for TdClient {
    async fn request(&self, mut req: Value) -> Result<Value> {
        let extra = self.inner.fresh_extra();
        let Some(obj) = req.as_object_mut() else {
            return Err(Error::Other("TDLib request must be a JSON object".into()));
        };
        obj.insert("@extra".into(), json!(extra));
        let client_id = self.inner.current();
        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .insert(extra, Pending { client_id, tx });
        self.inner.lib.send(client_id, &req.to_string());
        let v = rx
            .await
            .map_err(|_| Error::Other("TDLib client stopped".into()))?;
        match td_error(&v) {
            Some(e) => Err(e),
            None => Ok(v),
        }
    }

    fn updates(&self) -> broadcast::Receiver<Arc<Value>> {
        self.updates.subscribe()
    }

    async fn restart(&self) -> Result<()> {
        self.inner.close_current().await?;
        let id = self.inner.lib.create_client_id();
        self.inner.client_id.store(id, Ordering::SeqCst);
        self.inner.kick(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tdlib_parameters_are_flattened() {
        let p = TdParams {
            api_id: 42,
            api_hash: "abc".into(),
            db_dir: PathBuf::from("/data/db"),
            files_dir: PathBuf::from("/data/files"),
            device_model: "Windows".into(),
            app_version: "1.0.0".into(),
        };
        let r = p.to_request();
        assert_eq!(r["@type"], "setTdlibParameters");
        assert!(r.get("parameters").is_none());
        assert_eq!(r["database_directory"], "/data/db");
        assert_eq!(r["files_directory"], "/data/files");
        assert_eq!(r["use_file_database"], true);
        assert_eq!(r["use_chat_info_database"], true);
        assert_eq!(r["use_message_database"], true);
        assert_eq!(r["use_secret_chats"], false);
        assert_eq!(r["api_id"], 42);
        assert_eq!(r["api_hash"], "abc");
        assert_eq!(r["system_language_code"], "en");
        assert_eq!(r["device_model"], "Windows");
        assert_eq!(r["application_version"], "1.0.0");
    }

    #[test]
    fn auth_state_type_reads_updates_only() {
        let u = json!({"@type": "updateAuthorizationState",
            "authorization_state": {"@type": "authorizationStateReady"}});
        assert_eq!(auth_state_type(&u), Some("authorizationStateReady"));
        assert_eq!(auth_state_type(&json!({"@type": "ok"})), None);
    }
}
