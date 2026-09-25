//! The login state machine shared by the QR and phone flows.
//!
//! [`Auth`] follows `updateAuthorizationState` on the transport's update stream
//! and turns the user's inputs into TDLib requests. `setTdlibParameters` is
//! answered by the client itself (see [`crate::client::TdParams::to_request`]),
//! because only the client sees that state early enough and owns the
//! parameters; here that state is reported as [`AuthState::Connecting`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use flox_core::error::{Error, Result};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::transport::TdTransport;

/// Where login stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthState {
    Idle,
    Connecting,
    WaitPhone,
    /// `tg://login?token=...` to render as a QR code.
    WaitQr {
        link: String,
    },
    WaitCode,
    WaitPassword {
        hint: String,
    },
    Ready {
        user: String,
    },
    LoggingOut,
    Failed(String),
}

/// Shown when TDLib wants an email address or a new account, which Flox does not handle.
pub const SIGN_UP_UNSUPPORTED: &str =
    "Finish setting up this account in the Telegram app, then try again";

/// The `optimizeStorage` request sent once login is ready: it clears document
/// caches left behind when playback did not end cleanly.
pub fn optimize_storage_request() -> Value {
    json!({
        "@type": "optimizeStorage",
        "size": 0,
        "ttl": 0,
        "count": 0,
        "immunity_delay": 0,
        "file_types": [{"@type": "fileTypeDocument"}],
        "chat_ids": [],
        "exclude_chat_ids": [],
        "return_deleted_file_statistics": false,
        "chat_limit": 0,
    })
}

/// The name shown for the logged-in account: first and last name, else the
/// username, else the phone number.
fn display_name(me: &Value) -> String {
    let field = |k: &str| {
        me.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    };
    let name = [field("first_name"), field("last_name")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !name.is_empty() {
        return name;
    }
    let username = me
        .pointer("/usernames/active_usernames/0")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !username.is_empty() {
        return format!("@{username}");
    }
    field("phone_number")
}

/// The text shown in [`AuthState::Failed`] for `e`.
fn failure_text(e: &Error) -> String {
    match e {
        Error::Td { message, .. } if !message.is_empty() => message.clone(),
        other => other.to_string(),
    }
}

struct Inner {
    transport: Arc<dyn TdTransport>,
    state: watch::Sender<AuthState>,
    /// Set by [`Auth::log_out`]: once TDLib closes, start a fresh instance so
    /// the login screen can show again.
    restart_after_close: AtomicBool,
}

impl Inner {
    fn set(&self, s: AuthState) {
        self.state.send_replace(s);
    }

    fn fail(&self, e: &Error) {
        self.set(AuthState::Failed(failure_text(e)));
    }

    async fn call(&self, req: Value) -> Result<()> {
        match self.transport.request(req).await {
            Ok(_) => Ok(()),
            Err(e) => {
                self.fail(&e);
                Err(e)
            }
        }
    }

    async fn restart(&self) -> Result<()> {
        self.set(AuthState::Connecting);
        if let Err(e) = self.transport.restart().await {
            self.fail(&e);
            return Err(e);
        }
        Ok(())
    }

    /// Applies one `authorizationState*` object.
    async fn apply(&self, st: &Value) {
        let kind = st.get("@type").and_then(Value::as_str).unwrap_or_default();
        let text = |k: &str| {
            st.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        match kind {
            "authorizationStateWaitTdlibParameters" => self.set(AuthState::Connecting),
            "authorizationStateWaitPhoneNumber" => self.set(AuthState::WaitPhone),
            "authorizationStateWaitOtherDeviceConfirmation" => {
                self.set(AuthState::WaitQr { link: text("link") })
            }
            "authorizationStateWaitCode" => self.set(AuthState::WaitCode),
            "authorizationStateWaitPassword" => self.set(AuthState::WaitPassword {
                hint: text("password_hint"),
            }),
            "authorizationStateWaitEmailAddress"
            | "authorizationStateWaitEmailCode"
            | "authorizationStateWaitRegistration" => {
                self.set(AuthState::Failed(SIGN_UP_UNSUPPORTED.into()))
            }
            "authorizationStateReady" => {
                if matches!(*self.state.borrow(), AuthState::Ready { .. }) {
                    return;
                }
                let t = Arc::clone(&self.transport);
                tokio::spawn(async move {
                    if let Err(e) = t.request(optimize_storage_request()).await {
                        tracing::debug!("optimizeStorage failed: {e}");
                    }
                });
                let user = match self.transport.request(json!({"@type": "getMe"})).await {
                    Ok(me) => display_name(&me),
                    Err(e) => {
                        tracing::warn!("getMe failed: {e}");
                        String::new()
                    }
                };
                self.set(AuthState::Ready { user });
            }
            "authorizationStateLoggingOut" => self.set(AuthState::LoggingOut),
            "authorizationStateClosing" => {
                if *self.state.borrow() != AuthState::LoggingOut {
                    self.set(AuthState::Connecting);
                }
            }
            "authorizationStateClosed" => {
                self.set(AuthState::Idle);
                if self.restart_after_close.swap(false, Ordering::SeqCst) {
                    // A failure is already reported through the state.
                    let _ = self.restart().await;
                }
            }
            other => tracing::debug!("unhandled TDLib authorization state {other}"),
        }
    }

    async fn sync(&self) {
        match self
            .transport
            .request(json!({"@type": "getAuthorizationState"}))
            .await
        {
            Ok(st) => self.apply(&st).await,
            Err(e) => tracing::debug!("getAuthorizationState failed: {e}"),
        }
    }

    async fn drive(self: Arc<Self>, mut rx: broadcast::Receiver<Arc<Value>>) {
        // Catches a state that was broadcast before this subscriber existed.
        self.sync().await;
        loop {
            match rx.recv().await {
                Ok(u) => {
                    if u.get("@type").and_then(Value::as_str) == Some("updateAuthorizationState") {
                        if let Some(st) = u.get("authorization_state") {
                            self.apply(st).await;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("auth missed {n} TDLib updates, resyncing");
                    self.sync().await;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

/// Drives `authorizationState*` updates and the user's inputs.
///
/// Create it inside a tokio runtime: it spawns a task that follows the
/// transport's updates for as long as the `Auth` lives.
pub struct Auth {
    inner: Arc<Inner>,
    driver: Option<JoinHandle<()>>,
}

impl Auth {
    /// Starts in [`AuthState::Idle`] and moves on as soon as TDLib reports its state.
    pub fn new(t: Arc<dyn TdTransport>) -> Self {
        let (state, _rx) = watch::channel(AuthState::Idle);
        let rx = t.updates();
        let inner = Arc::new(Inner {
            transport: t,
            state,
            restart_after_close: AtomicBool::new(false),
        });
        let driver = match tokio::runtime::Handle::try_current() {
            Ok(h) => Some(h.spawn(Arc::clone(&inner).drive(rx))),
            Err(_) => {
                tracing::warn!("Auth created outside a tokio runtime; it will not follow TDLib");
                None
            }
        };
        Self { inner, driver }
    }

    /// A receiver for the current state and every change.
    pub fn state(&self) -> watch::Receiver<AuthState> {
        self.inner.state.subscribe()
    }

    /// The current state.
    pub fn current(&self) -> AuthState {
        self.inner.state.borrow().clone()
    }

    /// `requestQrCodeAuthentication`; the link arrives as [`AuthState::WaitQr`].
    pub async fn request_qr(&self) -> Result<()> {
        self.inner
            .call(json!({"@type": "requestQrCodeAuthentication", "other_user_ids": []}))
            .await
    }

    /// `setAuthenticationPhoneNumber`.
    pub async fn submit_phone(&self, phone: &str) -> Result<()> {
        self.inner
            .call(json!({"@type": "setAuthenticationPhoneNumber", "phone_number": phone.trim()}))
            .await
    }

    /// `checkAuthenticationCode`.
    pub async fn submit_code(&self, code: &str) -> Result<()> {
        self.inner
            .call(json!({"@type": "checkAuthenticationCode", "code": code.trim()}))
            .await
    }

    /// `checkAuthenticationPassword`.
    pub async fn submit_password(&self, pw: &str) -> Result<()> {
        self.inner
            .call(json!({"@type": "checkAuthenticationPassword", "password": pw}))
            .await
    }

    /// `logOut`. Once TDLib has closed, a fresh instance is started so login can begin again.
    pub async fn log_out(&self) -> Result<()> {
        self.inner.set(AuthState::LoggingOut);
        self.inner.restart_after_close.store(true, Ordering::SeqCst);
        let r = self.inner.call(json!({"@type": "logOut"})).await;
        if r.is_err() {
            self.inner
                .restart_after_close
                .store(false, Ordering::SeqCst);
        }
        r
    }

    /// Try again: `close` the current TDLib instance and start a fresh one.
    pub async fn try_again(&self) -> Result<()> {
        self.inner
            .restart_after_close
            .store(false, Ordering::SeqCst);
        self.inner.restart().await
    }
}

impl Drop for Auth {
    fn drop(&mut self) {
        if let Some(d) = self.driver.take() {
            d.abort();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::time::Duration;

    use async_trait::async_trait;
    use parking_lot::Mutex;

    use super::*;

    /// Updates to broadcast, then the response.
    type Scripted = (Vec<Value>, Result<Value>);

    /// A scripted transport: responses queued per `@type`, requests recorded,
    /// updates pushed by the test.
    pub(crate) struct Fake {
        pub updates: broadcast::Sender<Arc<Value>>,
        responses: Mutex<HashMap<String, VecDeque<Scripted>>>,
        pub requests: Mutex<Vec<Value>>,
        pub restarts: Mutex<u32>,
    }

    impl Fake {
        pub fn new() -> Arc<Fake> {
            let (updates, _) = broadcast::channel(64);
            Arc::new(Fake {
                updates,
                responses: Mutex::new(HashMap::new()),
                requests: Mutex::new(Vec::new()),
                restarts: Mutex::new(0),
            })
        }

        /// Queues the response to the next request of type `kind`.
        pub fn respond(&self, kind: &str, r: Result<Value>) {
            self.respond_after(kind, Vec::new(), r);
        }

        /// Queues a response that first broadcasts `updates`, as TDLib does
        /// before answering `loadChats`.
        pub fn respond_after(&self, kind: &str, updates: Vec<Value>, r: Result<Value>) {
            self.responses
                .lock()
                .entry(kind.to_owned())
                .or_default()
                .push_back((updates, r));
        }

        pub fn push(&self, u: Value) {
            let _ = self.updates.send(Arc::new(u));
        }

        pub fn push_state(&self, st: Value) {
            self.push(json!({"@type": "updateAuthorizationState", "authorization_state": st}));
        }

        pub fn sent(&self, kind: &str) -> Vec<Value> {
            self.requests
                .lock()
                .iter()
                .filter(|r| r["@type"] == kind)
                .cloned()
                .collect()
        }
    }

    #[async_trait]
    impl TdTransport for Fake {
        async fn request(&self, req: Value) -> Result<Value> {
            let kind = req["@type"].as_str().unwrap_or_default().to_owned();
            self.requests.lock().push(req);
            let queued = self
                .responses
                .lock()
                .get_mut(&kind)
                .and_then(VecDeque::pop_front);
            match queued {
                Some((updates, r)) => {
                    for u in updates {
                        self.push(u);
                    }
                    r
                }
                None if kind == "getAuthorizationState" => Err(Error::Td {
                    code: 400,
                    message: "no state scripted".into(),
                }),
                None => Ok(json!({"@type": "ok"})),
            }
        }

        fn updates(&self) -> broadcast::Receiver<Arc<Value>> {
            self.updates.subscribe()
        }

        async fn restart(&self) -> Result<()> {
            *self.restarts.lock() += 1;
            Ok(())
        }
    }

    async fn wait_for(auth: &Auth, want: AuthState) {
        let mut rx = auth.state();
        let r = tokio::time::timeout(Duration::from_secs(2), rx.wait_for(|s| *s == want)).await;
        assert!(
            matches!(r, Ok(Ok(_))),
            "state never became {want:?}, is {:?}",
            auth.current()
        );
    }

    async fn wait_until(what: &str, f: impl Fn() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    fn st(kind: &str) -> Value {
        json!({"@type": kind})
    }

    fn me() -> Value {
        json!({"@type": "user", "first_name": "Ada", "last_name": "Lovelace"})
    }

    #[tokio::test]
    async fn qr_path() {
        let fake = Fake::new();
        fake.respond("getMe", Ok(me()));
        let auth = Auth::new(fake.clone());
        assert_eq!(auth.current(), AuthState::Idle);

        fake.push_state(st("authorizationStateWaitTdlibParameters"));
        wait_for(&auth, AuthState::Connecting).await;
        fake.push_state(st("authorizationStateWaitPhoneNumber"));
        wait_for(&auth, AuthState::WaitPhone).await;

        auth.request_qr().await.unwrap();
        assert_eq!(fake.sent("requestQrCodeAuthentication").len(), 1);
        fake.push_state(
            json!({"@type": "authorizationStateWaitOtherDeviceConfirmation",
            "link": "tg://login?token=abc"}),
        );
        wait_for(
            &auth,
            AuthState::WaitQr {
                link: "tg://login?token=abc".into(),
            },
        )
        .await;

        fake.push_state(st("authorizationStateReady"));
        wait_for(
            &auth,
            AuthState::Ready {
                user: "Ada Lovelace".into(),
            },
        )
        .await;
        wait_until("optimizeStorage", || {
            !fake.sent("optimizeStorage").is_empty()
        })
        .await;
        let opt = &fake.sent("optimizeStorage")[0];
        assert_eq!(opt["file_types"], json!([{"@type": "fileTypeDocument"}]));
        assert_eq!(fake.sent("getMe").len(), 1);
    }

    #[tokio::test]
    async fn phone_path() {
        let fake = Fake::new();
        fake.respond("getMe", Ok(json!({"@type": "user", "first_name": "Grace"})));
        let auth = Auth::new(fake.clone());

        fake.push_state(st("authorizationStateWaitPhoneNumber"));
        wait_for(&auth, AuthState::WaitPhone).await;
        auth.submit_phone(" +15550100 ").await.unwrap();
        assert_eq!(
            fake.sent("setAuthenticationPhoneNumber")[0]["phone_number"],
            "+15550100"
        );

        fake.push_state(st("authorizationStateWaitCode"));
        wait_for(&auth, AuthState::WaitCode).await;
        auth.submit_code("12345").await.unwrap();
        assert_eq!(fake.sent("checkAuthenticationCode")[0]["code"], "12345");

        fake.push_state(st("authorizationStateReady"));
        wait_for(
            &auth,
            AuthState::Ready {
                user: "Grace".into(),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn password_path() {
        let fake = Fake::new();
        fake.respond("getMe", Ok(me()));
        let auth = Auth::new(fake.clone());

        fake.push_state(st("authorizationStateWaitCode"));
        wait_for(&auth, AuthState::WaitCode).await;
        auth.submit_code("11111").await.unwrap();
        fake.push_state(json!({"@type": "authorizationStateWaitPassword",
            "password_hint": "cat's name"}));
        wait_for(
            &auth,
            AuthState::WaitPassword {
                hint: "cat's name".into(),
            },
        )
        .await;

        auth.submit_password("hunter2").await.unwrap();
        assert_eq!(
            fake.sent("checkAuthenticationPassword")[0]["password"],
            "hunter2"
        );
        fake.push_state(st("authorizationStateReady"));
        wait_for(
            &auth,
            AuthState::Ready {
                user: "Ada Lovelace".into(),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn failure_then_try_again() {
        let fake = Fake::new();
        let auth = Auth::new(fake.clone());
        fake.push_state(st("authorizationStateWaitCode"));
        wait_for(&auth, AuthState::WaitCode).await;

        fake.respond(
            "checkAuthenticationCode",
            Err(Error::Td {
                code: 400,
                message: "PHONE_CODE_INVALID".into(),
            }),
        );
        let r = auth.submit_code("00000").await;
        assert!(matches!(r, Err(Error::Td { code: 400, .. })));
        assert_eq!(
            auth.current(),
            AuthState::Failed("PHONE_CODE_INVALID".into())
        );

        auth.try_again().await.unwrap();
        assert_eq!(*fake.restarts.lock(), 1);
        assert_eq!(auth.current(), AuthState::Connecting);
        fake.push_state(st("authorizationStateWaitTdlibParameters"));
        fake.push_state(st("authorizationStateWaitPhoneNumber"));
        wait_for(&auth, AuthState::WaitPhone).await;
    }

    #[tokio::test]
    async fn sign_up_is_a_failure() {
        let fake = Fake::new();
        let auth = Auth::new(fake.clone());
        fake.push_state(st("authorizationStateWaitRegistration"));
        wait_for(&auth, AuthState::Failed(SIGN_UP_UNSUPPORTED.into())).await;
    }

    #[tokio::test]
    async fn log_out_restarts_after_close() {
        let fake = Fake::new();
        fake.respond("getMe", Ok(me()));
        let auth = Auth::new(fake.clone());
        fake.push_state(st("authorizationStateReady"));
        wait_for(
            &auth,
            AuthState::Ready {
                user: "Ada Lovelace".into(),
            },
        )
        .await;

        auth.log_out().await.unwrap();
        assert_eq!(auth.current(), AuthState::LoggingOut);
        fake.push_state(st("authorizationStateLoggingOut"));
        fake.push_state(st("authorizationStateClosing"));
        fake.push_state(st("authorizationStateClosed"));
        wait_until("restart", || *fake.restarts.lock() == 1).await;
        fake.push_state(st("authorizationStateWaitPhoneNumber"));
        wait_for(&auth, AuthState::WaitPhone).await;
    }

    #[tokio::test]
    async fn initial_state_is_synced() {
        let fake = Fake::new();
        fake.respond(
            "getAuthorizationState",
            Ok(st("authorizationStateWaitPhoneNumber")),
        );
        let auth = Auth::new(fake.clone());
        wait_for(&auth, AuthState::WaitPhone).await;
    }

    #[test]
    fn display_name_falls_back() {
        assert_eq!(display_name(&me()), "Ada Lovelace");
        let u = json!({"first_name": "", "usernames": {"active_usernames": ["ada"]}});
        assert_eq!(display_name(&u), "@ada");
        assert_eq!(
            display_name(&json!({"phone_number": "15550100"})),
            "15550100"
        );
    }
}
