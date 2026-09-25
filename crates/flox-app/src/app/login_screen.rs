//! The Login screen on the shell: follows the auth state, makes the QR and restart
//! requests the flow asks for, sends the phone, code and password, and closes the
//! screen once TDLib is ready.

use std::cell::RefCell;
use std::rc::Rc;

use flox_core::images::Rgba;
use flox_td::auth::AuthState;
use slint::{ComponentHandle, Image, ModelRc, VecModel};

use super::{to_image, Shell, Telegram};
use crate::focus::{Focus, FocusGraph};
use crate::router::Route;
use crate::ui::{AppWindow, LoginAction, LoginState, LoginStep, Screen};
use crate::vm::home::TelegramStatus;
use crate::vm::login::{self as vm, Button, Effect, Flow, Mode, Step, Submit};
use crate::vm::settings::RESTART_TO_APPLY;

/// The QR code's logical size (`Tokens.qr-size`).
const QR_SIZE: f32 = 280.0;

/// The Login screen's UI-thread state.
pub(super) struct LoginScreen {
    flow: RefCell<Flow>,
    step: RefCell<Option<Step>>,
    buttons: RefCell<Vec<Button>>,
    actions: Rc<VecModel<LoginAction>>,
}

impl LoginScreen {
    pub(super) fn new() -> Self {
        Self {
            flow: RefCell::new(Flow::new()),
            step: RefCell::new(None),
            buttons: RefCell::new(Vec::new()),
            actions: Rc::new(VecModel::default()),
        }
    }
}

fn slint_step(step: &Step) -> LoginStep {
    match step {
        Step::Setup => LoginStep::Setup,
        Step::Unavailable => LoginStep::Unavailable,
        Step::Connecting | Step::Done => LoginStep::Connecting,
        Step::Qr { .. } => LoginStep::Qr,
        Step::Phone => LoginStep::Phone,
        Step::Code => LoginStep::Code,
        Step::Password { .. } => LoginStep::Password,
        Step::Failed { .. } => LoginStep::Failed,
    }
}

impl Shell {
    pub(super) fn bind_login(self: &Rc<Self>, ui: &AppWindow) {
        let state = ui.global::<LoginState>();
        state.set_actions(ModelRc::from(self.login_screen.actions.clone()));
        let shell = self.clone();
        state.on_accepted(move |text| shell.login_submit(&text));
    }

    /// Feeds a TDLib auth state to the shell, as the live watcher does. Also the way
    /// to drive Home, Settings and Login from tests.
    pub fn set_auth_state(self: &Rc<Self>, state: AuthState) {
        self.on_auth(state);
    }

    /// Login was opened (`fresh`) or came back into view.
    pub(super) fn show_login(self: &Rc<Self>, fresh: bool) {
        if fresh {
            *self.login_screen.flow.borrow_mut() = Flow::new();
            self.login_screen.step.borrow_mut().take();
        }
        self.login_state_changed();
    }

    /// The Telegram status changed: refresh the screens that show it.
    pub(super) fn account_auth_changed(self: &Rc<Self>) {
        self.render_settings();
        self.login_state_changed();
    }

    fn auth_state(&self) -> Option<AuthState> {
        match &self.home.borrow().status {
            TelegramStatus::Auth(state) => Some(state.clone()),
            _ => None,
        }
    }

    fn login_state_changed(self: &Rc<Self>) {
        if self.screen() != Screen::Login {
            return;
        }
        if let Some(state) = self.auth_state() {
            let effect = self.login_screen.flow.borrow_mut().on_state(&state);
            if let Some(effect) = effect {
                self.login_effect(effect);
            }
        }
        self.render_login();
    }

    fn login_effect(&self, effect: Effect) {
        let Telegram::Connected { auth, .. } = self.services.telegram() else {
            return;
        };
        self.exec.run(
            async move {
                match effect {
                    Effect::RequestQr => auth.request_qr().await,
                    Effect::Restart => auth.try_again().await,
                }
            },
            move |result| {
                // A failure is already shown through AuthState::Failed.
                if let Err(e) = result {
                    tracing::debug!("login {effect:?}: {e}");
                }
            },
        );
    }

    fn qr_image(&self, link: &str) -> Option<Image> {
        let scale = self
            .ui
            .upgrade()
            .map(|ui| ui.window().scale_factor())
            .unwrap_or(1.0);
        let size = (QR_SIZE * scale.max(1.0)).round() as u32;
        let pixels = vm::qr_rgba(link, size)?;
        to_image(&Rgba {
            width: size,
            height: size,
            pixels,
        })
    }

    fn render_login(self: &Rc<Self>) {
        let status = self.home.borrow().status.clone();
        let mode = self.login_screen.flow.borrow().mode();
        let step = vm::step(&status, mode);
        if step == Step::Done {
            self.login_screen.step.borrow_mut().take();
            self.back();
            return;
        }
        let mut view = vm::view(&step);
        if step == Step::Setup && self.telegram_restart_pending() {
            view.status = RESTART_TO_APPLY.to_owned();
        }
        let previous = self.login_screen.step.borrow_mut().replace(step.clone());
        let same = previous.as_ref().is_some_and(|p| vm::same_step(p, &step));
        let new_link = match (&step, &previous) {
            (Step::Qr { link }, Some(Step::Qr { link: old })) if link == old => None,
            (Step::Qr { link }, _) => Some(link.clone()),
            _ => None,
        };

        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<LoginState>();
            state.set_step(slint_step(&step));
            state.set_heading(view.heading.into());
            state.set_status(view.status.as_str().into());
            state.set_body(view.body.into());
            state.set_placeholder(view.field.unwrap_or_default().into());
            state.set_secret(view.secret);
            if !same {
                state.set_input("".into());
            }
            if let Some(link) = new_link {
                state.set_qr(self.qr_image(&link).unwrap_or_default());
            }
        }
        self.login_screen.actions.set_vec(
            view.buttons
                .iter()
                .map(|b| LoginAction {
                    label: b.label().into(),
                    filled: b.filled(),
                })
                .collect::<Vec<_>>(),
        );
        *self.login_screen.buttons.borrow_mut() = view.buttons.clone();
        if !same {
            let mut graph = FocusGraph::with_zones(vm::zones(&view));
            graph.focus_first();
            self.graphs.borrow_mut().login = graph;
        }
        self.sync_focus();
    }

    /// CENTER or a click on Login.
    pub(super) fn login_activate(self: &Rc<Self>, focus: Focus) -> Option<Route> {
        match focus.zone {
            vm::FIELD => self.submit_field(),
            vm::ACTIONS => {
                let button = self.login_screen.buttons.borrow().get(focus.index).copied();
                match button? {
                    Button::OpenSettings => return Some(Route::Settings),
                    Button::UsePhone => self.login_switch(Mode::Phone),
                    Button::UseQr => self.login_switch(Mode::Qr),
                    Button::SendCode | Button::Continue => self.submit_field(),
                    Button::TryAgain => self.login_effect(Effect::Restart),
                }
            }
            _ => {}
        }
        None
    }

    fn login_switch(self: &Rc<Self>, mode: Mode) {
        if let Some(state) = self.auth_state() {
            let effect = self.login_screen.flow.borrow_mut().switch(mode, &state);
            if let Some(effect) = effect {
                self.login_effect(effect);
            }
        }
        self.render_login();
    }

    fn submit_field(self: &Rc<Self>) {
        let text = self
            .ui
            .upgrade()
            .map(|ui| ui.global::<LoginState>().get_input())
            .unwrap_or_default();
        self.login_submit(&text);
    }

    /// Enter in the field, SEND CODE or CONTINUE.
    fn login_submit(&self, text: &str) {
        let submit = match &*self.login_screen.step.borrow() {
            Some(step) => vm::submission(step, text),
            None => None,
        };
        let (Some(submit), Telegram::Connected { auth, .. }) = (submit, self.services.telegram())
        else {
            return;
        };
        self.exec.run(
            async move {
                match submit {
                    Submit::Phone(phone) => auth.submit_phone(&phone).await,
                    Submit::Code(code) => auth.submit_code(&code).await,
                    Submit::Password(password) => auth.submit_password(&password).await,
                }
            },
            |result| {
                if let Err(e) = result {
                    tracing::debug!("login: {e}");
                }
            },
        );
    }
}
