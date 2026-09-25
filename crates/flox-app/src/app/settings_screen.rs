//! The Settings screen on the shell: the row model, the choice, text and confirm
//! dialogs, LEFT/RIGHT cycling, and what a saved change sets off (Telegram
//! credentials, UI scale, watch history, sign out).
//!
//! # Telegram credentials
//!
//! TDLib takes the API id and hash once, in `setTdlibParameters`, so new credentials
//! need a new TDLib instance. When the id or hash in effect changes, the shell calls
//! the hook installed with [`Shell::set_telegram_restart`] with the new settings. The
//! app installs [`crate::launch::Integration`] there: it closes the running instance,
//! waits for `authorizationStateClosed` and starts a new one with the new parameters
//! behind the same client (only one client may exist per process), or starts the
//! whole stack when Telegram was off, swaps [`Services::telegram`](super::Services),
//! hands the client to the player and the queue, and calls
//! [`Shell::telegram_replaced`], whose auth watcher feeds the new states to the
//! screens as [`Shell::set_auth_state`] does. With no hook installed (snapshot tests),
//! the change is saved and the account row reads `RESTART FLOX TO APPLY`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use flox_core::paths::Dirs;
use flox_core::settings::Settings;
use flox_sys::dirs::SystemDirs;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::{Shell, Telegram};
use crate::focus::{Direction, Focus, FocusGraph, Zone};
use crate::router::Route;
use crate::ui::{AppWindow, Screen, SettingRow, SettingsDialog, SettingsState};
use crate::vm::settings::{self as vm, Action, Confirm, Opt, RowId, TextField};

/// Rebuilds the Telegram stack with new credentials (see the module docs).
pub type TelegramRestart = Rc<dyn Fn(&Settings)>;

/// The dialog on screen.
enum Open {
    Choice(Vec<Opt>),
    Text(TextField),
    Confirm(Confirm),
}

/// The Settings screen's UI-thread state.
pub(super) struct SettingsScreen {
    rows: Rc<VecModel<SettingRow>>,
    options: Rc<VecModel<SharedString>>,
    open: RefCell<Option<Open>>,
    history_cleared: Cell<bool>,
    restart_pending: Cell<bool>,
    restart: RefCell<Option<TelegramRestart>>,
}

impl SettingsScreen {
    pub(super) fn new() -> Self {
        Self {
            rows: Rc::new(VecModel::default()),
            options: Rc::new(VecModel::default()),
            open: RefCell::new(None),
            history_cleared: Cell::new(false),
            restart_pending: Cell::new(false),
            restart: RefCell::new(None),
        }
    }
}

/// Re-applies the UI scale on top of the monitor's scale factor after the setting
/// changed from `previous` to `next`.
pub fn change_ui_scale(ui: &AppWindow, previous: f32, next: f32) {
    if previous <= 0.0 || (previous - next).abs() < f32::EPSILON {
        return;
    }
    let window = ui.window();
    let scale_factor = window.scale_factor() / previous * next;
    window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor });
}

impl Shell {
    /// Installs the hook that rebuilds the Telegram stack when the API id or hash in
    /// effect changes. Without one, the change applies on the next launch.
    pub fn set_telegram_restart(&self, hook: impl Fn(&Settings) + 'static) {
        *self.settings_screen.restart.borrow_mut() = Some(Rc::new(hook));
    }

    pub(super) fn bind_settings(self: &Rc<Self>, ui: &AppWindow) {
        let state = ui.global::<SettingsState>();
        state.set_rows(ModelRc::from(self.settings_screen.rows.clone()));
        state.set_dialog_options(ModelRc::from(self.settings_screen.options.clone()));
        let shell = self.clone();
        state.on_dialog_accepted(move |text| shell.commit_text(&text));
    }

    fn settings_context(&self, settings: &Settings) -> vm::Context {
        let path_env = std::env::var_os("PATH");
        vm::Context {
            status: self.home.borrow().status.clone(),
            library_qualities: self.library.borrow().all_qualities(),
            tools: vm::resolve_tools(settings, &SystemDirs.app_dir(), path_env.as_deref()),
            history_cleared: self.settings_screen.history_cleared.get(),
            restart_pending: self.settings_screen.restart_pending.get(),
            version: flox_core::VERSION.to_owned(),
        }
    }

    /// Settings was opened (`fresh`) or came back into view.
    pub(super) fn show_settings(self: &Rc<Self>, fresh: bool) {
        if fresh {
            self.settings_screen.history_cleared.set(false);
            self.settings_screen.open.borrow_mut().take();
            let mut graph = FocusGraph::with_zones(vm::zones(vm::row_ids().len()));
            graph.focus_first();
            self.graphs.borrow_mut().settings = graph;
            if let Some(ui) = self.ui.upgrade() {
                let state = ui.global::<SettingsState>();
                state.set_dialog(SettingsDialog::None);
                state.set_scroll_y(0.0);
            }
        }
        self.render_settings();
    }

    /// Refreshes every row's value while Settings shows.
    pub(super) fn render_settings(&self) {
        if self.screen() != Screen::Settings {
            return;
        }
        let settings = self.services.settings.get();
        let ctx = self.settings_context(&settings);
        let rows: Vec<SettingRow> = vm::rows(&settings, &ctx)
            .into_iter()
            .map(|r| SettingRow {
                header: r.header.into(),
                label: r.label.into(),
                value: r.value.into(),
            })
            .collect();
        let model = &self.settings_screen.rows;
        if model.row_count() == rows.len() {
            for (i, row) in rows.into_iter().enumerate() {
                if model.row_data(i).as_ref() != Some(&row) {
                    model.set_row_data(i, row);
                }
            }
        } else {
            let len = rows.len();
            model.set_vec(rows);
            self.set_len(Screen::Settings, vm::LIST, len);
        }
    }

    fn focused_row(&self) -> Option<RowId> {
        let focus = self.graphs.borrow().settings.focus()?;
        (focus.zone == vm::LIST)
            .then(|| vm::row_ids().get(focus.index).copied())
            .flatten()
    }

    /// LEFT/RIGHT on a settings row. True when it cycled a value.
    pub(super) fn settings_cycle(&self, direction: Direction) -> bool {
        if self.screen() != Screen::Settings || self.graphs.borrow().settings.in_modal() {
            return false;
        }
        let Some(id) = self.focused_row() else {
            return false;
        };
        let delta = if direction == Direction::Left { -1 } else { 1 };
        let settings = self.services.settings.get();
        let ctx = self.settings_context(&settings);
        let mut next = settings;
        if !vm::cycle(id, &mut next, &ctx, delta) {
            return false;
        }
        self.update_settings(|s| *s = next);
        true
    }

    /// CENTER or a click on Settings.
    pub(super) fn settings_activate(self: &Rc<Self>, focus: Focus) -> Option<Route> {
        match focus.zone {
            vm::LIST => {
                let id = *vm::row_ids().get(focus.index)?;
                let settings = self.services.settings.get();
                let ctx = self.settings_context(&settings);
                match vm::action(id, &settings, &ctx) {
                    Action::Toggle => {
                        self.update_settings(|s| {
                            vm::toggle(id, s);
                        });
                    }
                    Action::Choose {
                        title,
                        options,
                        current,
                    } => self.open_choice(title, options, current),
                    Action::Edit(field) => self.open_text(field, &settings),
                    Action::Confirm(c) => self.open_confirm(c),
                    Action::Open(route) => return Some(route),
                    Action::Nothing => {}
                }
            }
            vm::OPTIONS => {
                let choice = match &*self.settings_screen.open.borrow() {
                    Some(Open::Choice(options)) => options.get(focus.index).cloned(),
                    _ => None,
                };
                self.close_settings_dialog();
                if let Some(choice) = choice {
                    self.update_settings(|s| vm::set(s, &choice));
                }
            }
            vm::BUTTONS if focus.index == vm::CANCEL => {
                self.close_settings_dialog();
            }
            vm::BUTTONS => {
                let open = match &*self.settings_screen.open.borrow() {
                    Some(Open::Text(field)) => Some(Ok(*field)),
                    Some(Open::Confirm(c)) => Some(Err(*c)),
                    _ => None,
                };
                match open {
                    Some(Ok(_)) => {
                        let text = self
                            .ui
                            .upgrade()
                            .map(|ui| ui.global::<SettingsState>().get_dialog_text())
                            .unwrap_or_default();
                        self.commit_text(&text);
                    }
                    Some(Err(c)) => {
                        self.close_settings_dialog();
                        self.confirmed(c);
                    }
                    None => {}
                }
            }
            _ => {}
        }
        None
    }

    fn open_dialog(&self, open: Open, zones: Vec<Zone>, fill: impl FnOnce(&SettingsState)) {
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<SettingsState>();
            state.set_dialog_error("".into());
            fill(&state);
        }
        *self.settings_screen.open.borrow_mut() = Some(open);
        self.graphs.borrow_mut().settings.push_modal(zones);
        self.sync_focus();
    }

    fn open_choice(&self, title: &str, options: Vec<Opt>, current: usize) {
        let labels: Vec<SharedString> =
            options.iter().map(|o| vm::option_label(o).into()).collect();
        let zones = vm::choice_zones(options.len(), current);
        self.settings_screen.options.set_vec(labels);
        let current = i32::try_from(current).unwrap_or(0);
        self.open_dialog(Open::Choice(options), zones, |state| {
            state.set_dialog_title(title.into());
            state.set_dialog_current(current);
            state.set_dialog(SettingsDialog::Choice);
        });
    }

    fn open_text(&self, field: TextField, settings: &Settings) {
        let edit = vm::text_edit(field, settings);
        self.open_dialog(Open::Text(field), vm::text_zones(), |state| {
            state.set_dialog_title(edit.title.into());
            state.set_dialog_message(edit.message.as_str().into());
            state.set_dialog_text(edit.text.as_str().into());
            state.set_dialog_placeholder(edit.placeholder.as_str().into());
            state.set_dialog_secret(edit.secret);
            state.set_dialog(SettingsDialog::Text);
        });
    }

    fn open_confirm(&self, confirm: Confirm) {
        let (title, message) = vm::confirm_text(confirm);
        self.open_dialog(Open::Confirm(confirm), vm::confirm_zones(), |state| {
            state.set_dialog_title(title.into());
            state.set_dialog_message(message.into());
            state.set_dialog(SettingsDialog::Confirm);
        });
    }

    /// Closes the open settings dialog (BACK, CANCEL, or after a choice). False when
    /// none was open.
    pub(super) fn close_settings_dialog(&self) -> bool {
        if self.screen() != Screen::Settings {
            return false;
        }
        if self.settings_screen.open.borrow_mut().take().is_none() {
            return false;
        }
        self.graphs.borrow_mut().settings.pop_modal();
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<SettingsState>();
            state.set_dialog(SettingsDialog::None);
            state.set_dialog_text("".into());
            state.set_dialog_error("".into());
        }
        self.sync_focus();
        true
    }

    /// OK in the text dialog, or Enter in its field.
    fn commit_text(&self, text: &str) {
        let field = match &*self.settings_screen.open.borrow() {
            Some(Open::Text(field)) => *field,
            _ => return,
        };
        let mut error = None;
        self.update_settings(|s| {
            if let Err(e) = vm::apply_text(field, s, text) {
                error = Some(e);
            }
        });
        match error {
            Some(e) => {
                if let Some(ui) = self.ui.upgrade() {
                    ui.global::<SettingsState>().set_dialog_error(e.into());
                }
            }
            None => {
                self.close_settings_dialog();
            }
        }
    }

    /// OK in a confirm dialog.
    fn confirmed(self: &Rc<Self>, confirm: Confirm) {
        match confirm {
            Confirm::ClearHistory => match self.services.progress.clear() {
                Ok(()) => {
                    self.settings_screen.history_cleared.set(true);
                    self.refresh_continue();
                }
                Err(e) => tracing::warn!("clear watch history: {e}"),
            },
            Confirm::SignOut => {
                if let Telegram::Connected { auth, .. } = self.services.telegram() {
                    self.exec
                        .run(async move { auth.log_out().await }, |result| {
                            if let Err(e) = result {
                                tracing::warn!("sign out: {e}");
                            }
                        });
                }
            }
        }
        self.render_settings();
    }

    /// Saves a change made on this screen, then applies what it sets off.
    fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let previous = self.services.settings.get();
        if let Err(e) = self.services.settings.update(f) {
            tracing::warn!("cannot save settings: {e}");
            return;
        }
        let next = self.services.settings.get();
        if vm::telegram_restart_needed(&previous, &next) {
            self.restart_telegram(&next);
        }
        if let Some(ui) = self.ui.upgrade() {
            change_ui_scale(&ui, previous.ui_scale, next.ui_scale);
        }
        self.render_settings();
    }

    fn restart_telegram(&self, next: &Settings) {
        let hook = self.settings_screen.restart.borrow().clone();
        match hook {
            Some(hook) => hook(next),
            None => {
                tracing::info!("Telegram credentials changed; they apply on the next launch");
                self.settings_screen.restart_pending.set(true);
            }
        }
    }

    /// True when new Telegram credentials wait for a restart.
    pub(super) fn telegram_restart_pending(&self) -> bool {
        self.settings_screen.restart_pending.get()
    }
}
