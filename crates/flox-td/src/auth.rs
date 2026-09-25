//! The login state machine shared by the QR and phone flows. Filled in by piece P4.

use std::sync::Arc;

use flox_core::error::{Error, Result};
use tokio::sync::watch;

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

/// Drives `authorizationState*` updates and the user's inputs.
pub struct Auth {
    #[allow(dead_code)]
    transport: Arc<dyn TdTransport>,
    state: watch::Sender<AuthState>,
}

impl Auth {
    /// Starts in [`AuthState::Idle`].
    pub fn new(t: Arc<dyn TdTransport>) -> Self {
        let (state, _rx) = watch::channel(AuthState::Idle);
        Self {
            transport: t,
            state,
        }
    }

    /// A receiver for the current state and every change.
    pub fn state(&self) -> watch::Receiver<AuthState> {
        self.state.subscribe()
    }

    /// `requestQrCodeAuthentication`.
    pub async fn request_qr(&self) -> Result<()> {
        Err(Error::NotImplemented("flox_td::auth::Auth::request_qr"))
    }

    /// `setAuthenticationPhoneNumber`.
    pub async fn submit_phone(&self, _phone: &str) -> Result<()> {
        Err(Error::NotImplemented("flox_td::auth::Auth::submit_phone"))
    }

    /// `checkAuthenticationCode`.
    pub async fn submit_code(&self, _code: &str) -> Result<()> {
        Err(Error::NotImplemented("flox_td::auth::Auth::submit_code"))
    }

    /// `checkAuthenticationPassword`.
    pub async fn submit_password(&self, _pw: &str) -> Result<()> {
        Err(Error::NotImplemented(
            "flox_td::auth::Auth::submit_password",
        ))
    }

    /// `logOut`.
    pub async fn log_out(&self) -> Result<()> {
        Err(Error::NotImplemented("flox_td::auth::Auth::log_out"))
    }
}
