//! The Login screen: TDLib's auth state machine drawn as one step at a time.
//!
//! - QR by default (Android): once TDLib waits for a phone number, the screen asks
//!   for a QR link and draws `tg://login?...` at 280 px with the Android instruction.
//! - A ghost USE PHONE NUMBER switches to the Mac flow: phone, code, then the
//!   two-step password with its hint. TDLib cannot take a phone number while a QR
//!   link is pending, so switching from the QR restarts the client first.
//! - Errors are uppercased with TRY AGAIN; the setup hint shows when the API id or
//!   hash is missing. The screen closes once TDLib is ready.

use flox_td::auth::AuthState;
use qrcode::{Color, QrCode};

use crate::focus::{Zone, ZoneId};
use crate::vm::home::TelegramStatus;

/// The text field (phone, code or password). Mirrors `LoginZones.field`.
pub const FIELD: ZoneId = ZoneId(70);
/// The buttons under it. Mirrors `LoginZones.actions`.
pub const ACTIONS: ZoneId = ZoneId(71);

pub const EYEBROW: &str = "TELEGRAM";
pub const TITLE: &str = "Connect your library";
pub const SCAN: &str = "SCAN WITH THE TELEGRAM APP · SETTINGS · DEVICES · LINK DESKTOP DEVICE";
pub const CONNECTING: &str = "CONNECTING";
pub const PHONE: &str = "PHONE NUMBER WITH COUNTRY CODE";
pub const CODE: &str = "CODE FROM THE TELEGRAM APP";
pub const PASSWORD: &str = "TWO-STEP PASSWORD";
pub const TDLIB_NOT_FOUND: &str = "TDLIB NOT FOUND";
pub const SETUP_TITLE: &str = "Add your Telegram API id and hash in Settings";
pub const SETUP_BODY: &str = "Create them at my.telegram.org/apps. The TMDB key goes there too.";
pub const UNAVAILABLE_BODY: &str =
    "Flox could not load TDLib. Put tdjson next to flox.exe, or reinstall Flox.";

/// QR or phone number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Qr,
    Phone,
}

/// What the screen shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// API id or hash missing: the setup hint and OPEN SETTINGS.
    Setup,
    /// tdjson is missing.
    Unavailable,
    Connecting,
    Qr {
        link: String,
    },
    Phone,
    Code,
    Password {
        hint: String,
    },
    Failed {
        message: String,
    },
    /// Signed in: the screen closes.
    Done,
}

/// The step for a Telegram status in `mode`.
pub fn step(status: &TelegramStatus, mode: Mode) -> Step {
    let state = match status {
        TelegramStatus::NotConfigured => return Step::Setup,
        TelegramStatus::Unavailable => return Step::Unavailable,
        TelegramStatus::Auth(state) => state,
    };
    match (state, mode) {
        (AuthState::Idle | AuthState::Connecting | AuthState::LoggingOut, _) => Step::Connecting,
        // QR mode asks for a link as soon as TDLib waits for a phone number.
        (AuthState::WaitPhone, Mode::Qr) => Step::Connecting,
        (AuthState::WaitPhone, Mode::Phone) => Step::Phone,
        (AuthState::WaitQr { link }, Mode::Qr) => Step::Qr { link: link.clone() },
        // Switching to the phone restarts TDLib; wait for it.
        (AuthState::WaitQr { .. }, Mode::Phone) => Step::Connecting,
        (AuthState::WaitCode, _) => Step::Code,
        (AuthState::WaitPassword { hint }, _) => Step::Password { hint: hint.clone() },
        (AuthState::Ready { .. }, _) => Step::Done,
        (AuthState::Failed(message), _) => Step::Failed {
            message: message.to_uppercase(),
        },
    }
}

/// A request the screen makes on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// `requestQrCodeAuthentication`.
    RequestQr,
    /// Close TDLib and start a fresh instance (leaves a pending QR link).
    Restart,
}

/// The mode and the requests already made for the current kind of auth state, so
/// each request is sent once per state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flow {
    mode: Mode,
    last: Option<AuthState>,
    requested: bool,
}

impl Flow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// TDLib reported `state` (or the screen opened on it).
    pub fn on_state(&mut self, state: &AuthState) -> Option<Effect> {
        // A refreshed QR link is the same state as far as requests go.
        let same = self
            .last
            .as_ref()
            .is_some_and(|l| std::mem::discriminant(l) == std::mem::discriminant(state));
        self.last = Some(state.clone());
        if !same {
            self.requested = false;
        }
        self.effect(state)
    }

    /// USE PHONE NUMBER / USE QR CODE.
    pub fn switch(&mut self, mode: Mode, state: &AuthState) -> Option<Effect> {
        if self.mode == mode {
            return None;
        }
        self.mode = mode;
        self.last = Some(state.clone());
        self.requested = false;
        if mode == Mode::Qr && *state == AuthState::WaitCode {
            // TDLib accepts a QR request while it waits for a code.
            self.requested = true;
            return Some(Effect::RequestQr);
        }
        self.effect(state)
    }

    fn effect(&mut self, state: &AuthState) -> Option<Effect> {
        if self.requested {
            return None;
        }
        let effect = match (state, self.mode) {
            (AuthState::WaitPhone, Mode::Qr) => Effect::RequestQr,
            (AuthState::WaitQr { .. }, Mode::Phone) => Effect::Restart,
            _ => return None,
        };
        self.requested = true;
        Some(effect)
    }
}

/// A button under the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    OpenSettings,
    UsePhone,
    UseQr,
    SendCode,
    Continue,
    TryAgain,
}

impl Button {
    pub fn label(self) -> &'static str {
        match self {
            Button::OpenSettings => "OPEN SETTINGS",
            Button::UsePhone => "USE PHONE NUMBER",
            Button::UseQr => "USE QR CODE",
            Button::SendCode => "SEND CODE",
            Button::Continue => "CONTINUE",
            Button::TryAgain => "TRY AGAIN",
        }
    }

    /// Filled (primary) or ghost.
    pub fn filled(self) -> bool {
        !matches!(self, Button::UsePhone | Button::UseQr)
    }
}

/// What a step draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub heading: &'static str,
    /// The mono line under the heading: the instruction, the state or the error.
    pub status: String,
    /// Secondary body text (the setup hint).
    pub body: &'static str,
    /// The field's placeholder; `None` hides the field.
    pub field: Option<&'static str>,
    pub secret: bool,
    pub buttons: Vec<Button>,
}

/// The step's text, field and buttons.
pub fn view(step: &Step) -> View {
    let plain = |status: &str, buttons: Vec<Button>| View {
        heading: TITLE,
        status: status.to_owned(),
        body: "",
        field: None,
        secret: false,
        buttons,
    };
    match step {
        Step::Setup => View {
            heading: SETUP_TITLE,
            body: SETUP_BODY,
            ..plain("", vec![Button::OpenSettings])
        },
        Step::Unavailable => View {
            body: UNAVAILABLE_BODY,
            ..plain(TDLIB_NOT_FOUND, Vec::new())
        },
        Step::Connecting | Step::Done => plain(CONNECTING, Vec::new()),
        Step::Qr { .. } => plain(SCAN, vec![Button::UsePhone]),
        Step::Phone => View {
            field: Some("+1 555 123 4567"),
            ..plain(PHONE, vec![Button::SendCode, Button::UseQr])
        },
        Step::Code => View {
            field: Some("12345"),
            ..plain(CODE, vec![Button::Continue, Button::UseQr])
        },
        Step::Password { hint } => {
            let status = if hint.trim().is_empty() {
                PASSWORD.to_owned()
            } else {
                format!("{PASSWORD} · HINT: {}", hint.trim().to_uppercase())
            };
            View {
                field: Some("Password"),
                secret: true,
                ..plain(&status, vec![Button::Continue])
            }
        }
        Step::Failed { message } => plain(message, vec![Button::TryAgain]),
    }
}

/// The step's zones: the field (when shown), then the buttons.
pub fn zones(view: &View) -> Vec<Zone> {
    vec![
        Zone::row(FIELD, usize::from(view.field.is_some())).text(),
        Zone::row(ACTIONS, view.buttons.len()),
    ]
}

/// What submitting the field sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submit {
    Phone(String),
    Code(String),
    Password(String),
}

/// The request for `text` typed at `step`; `None` when there is nothing to send.
pub fn submission(step: &Step, text: &str) -> Option<Submit> {
    let trimmed = text.trim();
    match step {
        Step::Phone if !trimmed.is_empty() => Some(Submit::Phone(trimmed.to_owned())),
        Step::Code if !trimmed.is_empty() => Some(Submit::Code(trimmed.to_owned())),
        // Passwords are sent as typed.
        Step::Password { .. } if !text.is_empty() => Some(Submit::Password(text.to_owned())),
        _ => None,
    }
}

/// The steps whose field keeps its text between renders of the same step.
pub fn same_step(a: &Step, b: &Step) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// Draws `link` as a black-on-white QR code, `size`×`size` RGBA, with a one-module
/// quiet zone (Android's ZXing `MARGIN = 1`). Modules are whole pixels, centred.
pub fn qr_rgba(link: &str, size: u32) -> Option<Vec<u8>> {
    let code = QrCode::new(link.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let modules = width + 2;
    let size_px = usize::try_from(size).ok()?;
    let scale = size_px / modules;
    if scale == 0 {
        return None;
    }
    let offset = (size_px - modules * scale) / 2;
    let mut pixels = vec![0xFF; size_px * size_px * 4];
    for y in 0..width {
        for x in 0..width {
            if colors.get(y * width + x) != Some(&Color::Dark) {
                continue;
            }
            let left = offset + (x + 1) * scale;
            let top = offset + (y + 1) * scale;
            for py in top..top + scale {
                for px in left..left + scale {
                    let i = (py * size_px + px) * 4;
                    pixels[i..i + 3].fill(0);
                }
            }
        }
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(state: AuthState) -> TelegramStatus {
        TelegramStatus::Auth(state)
    }

    fn qr(link: &str) -> AuthState {
        AuthState::WaitQr { link: link.into() }
    }

    #[test]
    fn states_map_to_steps() {
        let q = Mode::Qr;
        assert_eq!(step(&TelegramStatus::NotConfigured, q), Step::Setup);
        assert_eq!(step(&TelegramStatus::Unavailable, q), Step::Unavailable);
        assert_eq!(step(&auth(AuthState::Idle), q), Step::Connecting);
        assert_eq!(step(&auth(AuthState::Connecting), q), Step::Connecting);
        assert_eq!(step(&auth(AuthState::LoggingOut), q), Step::Connecting);
        assert_eq!(step(&auth(AuthState::WaitPhone), q), Step::Connecting);
        assert_eq!(
            step(&auth(qr("tg://login?token=a")), q),
            Step::Qr {
                link: "tg://login?token=a".into()
            }
        );
        assert_eq!(step(&auth(AuthState::WaitCode), q), Step::Code);
        assert_eq!(
            step(&auth(AuthState::WaitPassword { hint: "cat".into() }), q),
            Step::Password { hint: "cat".into() }
        );
        assert_eq!(
            step(&auth(AuthState::Ready { user: "u".into() }), q),
            Step::Done
        );
        assert_eq!(
            step(&auth(AuthState::Failed("PHONE_CODE_INVALID bad".into())), q),
            Step::Failed {
                message: "PHONE_CODE_INVALID BAD".into()
            }
        );
    }

    #[test]
    fn phone_mode_shows_the_field_and_waits_out_a_pending_qr() {
        let p = Mode::Phone;
        assert_eq!(step(&auth(AuthState::WaitPhone), p), Step::Phone);
        assert_eq!(step(&auth(qr("x")), p), Step::Connecting);
        assert_eq!(step(&auth(AuthState::WaitCode), p), Step::Code);
    }

    #[test]
    fn qr_mode_requests_a_link_once_per_state() {
        let mut f = Flow::new();
        assert_eq!(f.on_state(&AuthState::Connecting), None);
        assert_eq!(f.on_state(&AuthState::WaitPhone), Some(Effect::RequestQr));
        assert_eq!(f.on_state(&AuthState::WaitPhone), None, "already asked");
        assert_eq!(f.on_state(&qr("a")), None);
        assert_eq!(f.on_state(&qr("b")), None, "a refreshed link needs nothing");
        // After a restart TDLib waits for a phone again: ask again.
        assert_eq!(f.on_state(&AuthState::Connecting), None);
        assert_eq!(f.on_state(&AuthState::WaitPhone), Some(Effect::RequestQr));
    }

    #[test]
    fn switching_to_the_phone_restarts_a_pending_qr() {
        let mut f = Flow::new();
        f.on_state(&AuthState::WaitPhone);
        f.on_state(&qr("a"));
        assert_eq!(f.switch(Mode::Phone, &qr("a")), Some(Effect::Restart));
        assert_eq!(f.mode(), Mode::Phone);
        assert_eq!(f.on_state(&qr("b")), None, "one restart is enough");
        assert_eq!(f.on_state(&AuthState::Connecting), None);
        assert_eq!(
            f.on_state(&AuthState::WaitPhone),
            None,
            "no QR in phone mode"
        );
        assert_eq!(f.switch(Mode::Phone, &AuthState::WaitPhone), None);
    }

    #[test]
    fn switching_to_the_phone_before_the_qr_needs_nothing() {
        let mut f = Flow::new();
        assert_eq!(f.switch(Mode::Phone, &AuthState::WaitPhone), None);
        assert_eq!(f.on_state(&AuthState::WaitPhone), None);
    }

    #[test]
    fn switching_back_to_qr_requests_a_link() {
        let mut f = Flow::new();
        f.switch(Mode::Phone, &AuthState::WaitPhone);
        assert_eq!(
            f.switch(Mode::Qr, &AuthState::WaitPhone),
            Some(Effect::RequestQr)
        );
        let mut f = Flow::new();
        f.switch(Mode::Phone, &AuthState::WaitPhone);
        f.on_state(&AuthState::WaitCode);
        assert_eq!(
            f.switch(Mode::Qr, &AuthState::WaitCode),
            Some(Effect::RequestQr)
        );
        assert_eq!(f.on_state(&AuthState::WaitCode), None);
    }

    #[test]
    fn views_carry_the_android_and_mac_text() {
        let v = view(&Step::Qr { link: "x".into() });
        assert_eq!(v.status, SCAN);
        assert_eq!(v.buttons, [Button::UsePhone]);
        assert!(!Button::UsePhone.filled());
        assert_eq!(v.field, None);

        let v = view(&Step::Phone);
        assert_eq!(v.status, "PHONE NUMBER WITH COUNTRY CODE");
        assert_eq!(v.field, Some("+1 555 123 4567"));
        assert_eq!(v.buttons, [Button::SendCode, Button::UseQr]);

        let v = view(&Step::Password {
            hint: "my cat".into(),
        });
        assert_eq!(v.status, "TWO-STEP PASSWORD · HINT: MY CAT");
        assert!(v.secret);
        let v = view(&Step::Password { hint: " ".into() });
        assert_eq!(v.status, "TWO-STEP PASSWORD");

        let v = view(&Step::Failed {
            message: "TIMEOUT".into(),
        });
        assert_eq!(
            (v.status.as_str(), v.buttons[0]),
            ("TIMEOUT", Button::TryAgain)
        );
        assert_eq!(Button::TryAgain.label(), "TRY AGAIN");

        let v = view(&Step::Setup);
        assert_eq!(v.heading, "Add your Telegram API id and hash in Settings");
        assert!(v.body.contains("my.telegram.org/apps"));
        assert_eq!(v.buttons, [Button::OpenSettings]);
        assert_eq!(Button::OpenSettings.label(), "OPEN SETTINGS");

        assert_eq!(view(&Step::Connecting).status, "CONNECTING");
    }

    #[test]
    fn zones_follow_the_view() {
        let z = zones(&view(&Step::Phone));
        assert_eq!(z[0].len(), 1);
        assert_eq!(z[1].len(), 2);
        let z = zones(&view(&Step::Qr { link: "x".into() }));
        assert!(z[0].is_empty());
        assert_eq!(z[1].len(), 1);
        assert!(zones(&view(&Step::Connecting)).iter().all(Zone::is_empty));
    }

    #[test]
    fn submissions() {
        assert_eq!(
            submission(&Step::Phone, " +1 555 "),
            Some(Submit::Phone("+1 555".into()))
        );
        assert_eq!(submission(&Step::Phone, "  "), None);
        assert_eq!(
            submission(&Step::Code, "12345"),
            Some(Submit::Code("12345".into()))
        );
        assert_eq!(
            submission(&Step::Password { hint: "".into() }, " pw "),
            Some(Submit::Password(" pw ".into()))
        );
        assert_eq!(submission(&Step::Connecting, "x"), None);
    }

    #[test]
    fn qr_is_280_px_black_on_white_with_a_quiet_zone() {
        let px = qr_rgba("tg://login?token=AQIDBAUGBwgJCgsMDQ4PEA", 280).unwrap();
        assert_eq!(px.len(), 280 * 280 * 4);
        let at = |x: usize, y: usize| px[(y * 280 + x) * 4];
        let dark = px.chunks(4).filter(|p| p[0] == 0).count();
        assert!(dark > 280 * 280 / 5, "{dark} dark pixels");
        // The corner is quiet zone; the finder pattern starts just inside it.
        assert_eq!(at(0, 0), 0xFF);
        let code = QrCode::new(b"tg://login?token=AQIDBAUGBwgJCgsMDQ4PEA").unwrap();
        let modules = code.width() + 2;
        let scale = 280 / modules;
        let offset = (280 - modules * scale) / 2;
        assert_eq!(at(offset + scale, offset + scale), 0);
        assert_eq!(at(offset + scale - 1, offset + scale - 1), 0xFF);
        assert!(px.chunks(4).all(|p| p[3] == 0xFF));
    }
}
