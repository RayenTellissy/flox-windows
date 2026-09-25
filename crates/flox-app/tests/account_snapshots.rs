//! Drives Settings and Login from the fixtures with the software renderer and saves
//! `target/snapshots/{settings,settings-choice,settings-account,login-qr,login-phone}.png` for review
//! against DESIGN.dark.md. Loads run inline (`Exec::Inline`); auth states are fed
//! with `Shell::set_auth_state`, as the live watcher does.

// Test helpers outside #[test] functions panic on setup failures too.
#![allow(clippy::unwrap_used)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use flox_app::app::{Exec, Services, Shell, Telegram};
use flox_app::fixtures::{
    FixtureCatalog, FixtureImages, FixtureLibrary, FixtureLibrarySource, Fixtures, DEFAULT_PATH,
};
use flox_app::focus::Modifiers;
use flox_app::ui::{FocusState, LoginState, LoginStep, SettingsDialog, SettingsState};
use flox_app::vm::settings::{row_ids, RowId};
use flox_app::{AppWindow, Screen};
use flox_core::progress::ProgressStore;
use flox_core::settings::{Settings, SettingsStore};
use flox_td::auth::AuthState;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, WindowAdapter};
use slint::{ComponentHandle, Model, PhysicalSize, Rgb8Pixel};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const LIST: i32 = 60;
const OPTIONS: i32 = 61;
const BUTTONS: i32 = 63;
const LOGIN_FIELD: i32 = 70;
const LOGIN_ACTIONS: i32 = 71;

struct SnapshotPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for SnapshotPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn services(dir: &Path) -> (Arc<Services>, Arc<ProgressStore>) {
    let fixtures = Arc::new(Fixtures::load(Path::new(DEFAULT_PATH)).unwrap());
    let history = dir.join("progress.json");
    std::fs::write(&history, serde_json::to_vec(&fixtures.progress).unwrap()).unwrap();
    let progress = Arc::new(ProgressStore::open(&history).unwrap());
    let library = Arc::new(FixtureLibrary::new(&fixtures));
    let services = Arc::new(Services {
        settings: SettingsStore::new(dir.join("settings.json"), Settings::default()),
        progress: progress.clone(),
        catalog: Arc::new(FixtureCatalog(fixtures)),
        images: Arc::new(FixtureImages),
        telegram: Telegram::Offline {
            library: Arc::new(FixtureLibrarySource(library)),
        },
    });
    (services, progress)
}

fn press(shell: &Rc<Shell>, key: Key) {
    let text = char::from(key).to_string();
    shell.key(&text, Modifiers::default());
    slint::platform::update_timers_and_animations();
}

fn click(ui: &AppWindow, zone: i32, index: usize) {
    ui.global::<FocusState>()
        .invoke_clicked(zone, i32::try_from(index).unwrap());
}

fn row(id: RowId) -> usize {
    row_ids().iter().position(|r| *r == id).unwrap()
}

fn value(ui: &AppWindow, id: RowId) -> String {
    let rows = ui.global::<SettingsState>().get_rows();
    rows.row_data(row(id)).unwrap().value.to_string()
}

fn saved(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap()
}

/// Draws the window and saves it; returns the pixels.
fn snapshot(window: &MinimalSoftwareWindow, name: &str) -> Vec<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    let mut buffer = vec![Rgb8Pixel::new(0, 0, 0); (WIDTH * HEIGHT) as usize];
    let drawn = window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, WIDTH as usize);
    });
    assert!(drawn, "{name}: nothing was drawn");

    let mut image = image::RgbImage::new(WIDTH, HEIGHT);
    for (i, p) in buffer.iter().enumerate() {
        let i = i as u32;
        image.put_pixel(i % WIDTH, i / WIDTH, image::Rgb([p.r, p.g, p.b]));
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    image.save(&path).unwrap();
    let saved = image::open(&path).unwrap();
    assert_eq!((saved.width(), saved.height()), (WIDTH, HEIGHT));
    buffer
}

fn count(buffer: &[Rgb8Pixel], color: (u8, u8, u8)) -> usize {
    buffer.iter().filter(|p| (p.r, p.g, p.b) == color).count()
}

#[test]
fn settings_and_login_snapshot() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(SnapshotPlatform {
        window: window.clone(),
    }))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();

    let ui = AppWindow::new().unwrap();
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    ui.show().unwrap();
    let (services, progress) = services(dir.path());
    let shell = Shell::new(&ui, services.clone(), Exec::Inline);
    let restarts: Rc<RefCell<Vec<Option<i32>>>> = Rc::default();
    let seen = restarts.clone();
    shell.set_telegram_restart(move |s| seen.borrow_mut().push(s.telegram_api_id));
    shell.start();

    // Ctrl+, opens Settings from Home with the first row focused.
    shell.key(
        ",",
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(ui.get_screen(), Screen::Settings);
    let focus = ui.global::<FocusState>();
    assert_eq!((focus.get_zone(), focus.get_index()), (LIST, 0));
    let settings = ui.global::<SettingsState>();
    assert_eq!(settings.get_rows().row_count(), row_ids().len());
    assert_eq!(value(&ui, RowId::Quality), "HIGHEST");
    assert_eq!(value(&ui, RowId::Version), "1.0.0");
    assert_eq!(value(&ui, RowId::Account), "SIGNED IN");

    // RIGHT cycles and saves settings.json; LEFT goes back.
    press(&shell, Key::RightArrow);
    assert_eq!(value(&ui, RowId::Quality), "2160P");
    assert_eq!(saved(dir.path())["quality"], "2160p");
    press(&shell, Key::LeftArrow);
    assert_eq!(value(&ui, RowId::Quality), "HIGHEST");
    assert!(saved(dir.path()).get("quality").is_none());

    // CENTER on a toggle flips it.
    click(&ui, LIST, row(RowId::Subtitles));
    assert_eq!(value(&ui, RowId::Subtitles), "ON");
    assert_eq!(saved(dir.path())["subtitles_enabled"], true);
    press(&shell, Key::UpArrow);
    press(&shell, Key::UpArrow);
    press(&shell, Key::UpArrow);
    assert_eq!(focus.get_index(), 0);
    let top = snapshot(&window, "settings");
    assert_eq!((top[0].r, top[0].g, top[0].b), (0x0E, 0x0E, 0x0E), "canvas");
    assert!(count(&top, (0xFA, 0xFA, 0xFA)) > 300, "focus ring and text");

    // CENTER on Default quality: the choice dialog lists the library's qualities.
    press(&shell, Key::Return);
    assert_eq!(settings.get_dialog(), SettingsDialog::Choice);
    assert_eq!(focus.get_zone(), OPTIONS);
    let options: Vec<String> = settings
        .get_dialog_options()
        .iter()
        .map(|o| o.to_string())
        .collect();
    assert_eq!(options[0], "HIGHEST");
    assert!(options.contains(&"2160P DV".to_owned()), "{options:?}");
    let dv = options.iter().position(|o| o == "2160P DV").unwrap();
    press(&shell, Key::DownArrow);
    press(&shell, Key::DownArrow);
    let choice = snapshot(&window, "settings-choice");
    assert!(count(&choice, (0xFA, 0xFA, 0xFA)) > 300);
    // LEFT/RIGHT do nothing inside a dialog.
    press(&shell, Key::RightArrow);
    assert_eq!(value(&ui, RowId::Quality), "HIGHEST");
    click(&ui, OPTIONS, dv);
    assert_eq!(settings.get_dialog(), SettingsDialog::None);
    assert_eq!(value(&ui, RowId::Quality), "2160P DV");
    assert_eq!(saved(dir.path())["quality"], "2160p DV");
    assert_eq!((focus.get_zone(), focus.get_index()), (LIST, 0));

    // BACK closes a dialog before it leaves the screen.
    press(&shell, Key::Return);
    assert_eq!(settings.get_dialog(), SettingsDialog::Choice);
    press(&shell, Key::Escape);
    assert_eq!(settings.get_dialog(), SettingsDialog::None);
    assert_eq!(ui.get_screen(), Screen::Settings);

    // The API id: a bad value stays in the dialog with an error; a good one saves
    // and asks for a Telegram restart. The TMDB key does not.
    click(&ui, LIST, row(RowId::TelegramApiId));
    assert_eq!(settings.get_dialog(), SettingsDialog::Text);
    assert_eq!(focus.get_zone(), 62);
    assert!(focus.get_editing());
    settings.invoke_dialog_accepted("12a".into());
    assert_eq!(settings.get_dialog_error(), "THE API ID IS A WHOLE NUMBER");
    assert!(restarts.borrow().is_empty());
    settings.set_dialog_text("12345".into());
    press(&shell, Key::DownArrow);
    assert_eq!((focus.get_zone(), focus.get_index()), (BUTTONS, 1), "OK");
    press(&shell, Key::Return);
    assert_eq!(settings.get_dialog(), SettingsDialog::None);
    assert_eq!(*restarts.borrow(), [Some(12345)]);
    assert_eq!(value(&ui, RowId::TelegramApiId), "12345");
    assert_eq!(saved(dir.path())["telegram_api_id"], 12345);

    click(&ui, LIST, row(RowId::TmdbKey));
    assert!(settings.get_dialog_secret());
    settings.invoke_dialog_accepted("0123456789abcdef".into());
    assert_eq!(value(&ui, RowId::TmdbKey), "••••••••CDEF");
    assert_eq!(restarts.borrow().len(), 1);

    // CLEAR WATCH HISTORY asks first; CANCEL is focused.
    assert!(!progress.all().is_empty());
    click(&ui, LIST, row(RowId::ClearHistory));
    assert_eq!(settings.get_dialog(), SettingsDialog::Confirm);
    assert_eq!((focus.get_zone(), focus.get_index()), (BUTTONS, 0));
    press(&shell, Key::Return);
    assert!(!progress.all().is_empty(), "cancelled");
    click(&ui, LIST, row(RowId::ClearHistory));
    click(&ui, BUTTONS, 1);
    assert!(progress.all().is_empty());
    assert_eq!(value(&ui, RowId::ClearHistory), "CLEARED");

    // The bottom of the list: ACCOUNT, TOOLS and ABOUT.
    click(&ui, LIST, row(RowId::Version));
    assert_eq!(
        focus.get_index(),
        i32::try_from(row(RowId::Version)).unwrap()
    );
    let bottom = snapshot(&window, "settings-account");
    assert!(count(&bottom, (0xFA, 0xFA, 0xFA)) > 300);
    assert!(settings.get_scroll_y() < 0.0, "scrolled to the focused row");

    // Login, QR step from a fixture link.
    shell.set_auth_state(AuthState::WaitQr {
        link: "tg://login?token=AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA".into(),
    });
    assert_eq!(value(&ui, RowId::Account), "SIGNED OUT");
    click(&ui, LIST, row(RowId::Account));
    assert_eq!(ui.get_screen(), Screen::Login);
    let login = ui.global::<LoginState>();
    assert_eq!(login.get_step(), LoginStep::Qr);
    assert_eq!(
        login.get_status(),
        "SCAN WITH THE TELEGRAM APP · SETTINGS · DEVICES · LINK DESKTOP DEVICE"
    );
    assert_eq!(login.get_qr().size().width, 280);
    assert_eq!((focus.get_zone(), focus.get_index()), (LOGIN_ACTIONS, 0));
    let qr = snapshot(&window, "login-qr");
    assert!(count(&qr, (0, 0, 0)) > 10_000, "dark modules");
    assert!(count(&qr, (0xFF, 0xFF, 0xFF)) > 10_000, "light modules");

    // USE PHONE NUMBER waits for TDLib to restart, then asks for the phone.
    press(&shell, Key::Return);
    assert_eq!(login.get_step(), LoginStep::Connecting);
    assert_eq!(login.get_status(), "CONNECTING");
    shell.set_auth_state(AuthState::Connecting);
    shell.set_auth_state(AuthState::WaitPhone);
    assert_eq!(login.get_step(), LoginStep::Phone);
    assert_eq!(focus.get_zone(), LOGIN_FIELD);
    assert!(focus.get_editing());
    login.set_input("+1 555 0100".into());
    let phone = snapshot(&window, "login-phone");
    assert!(count(&phone, (0xFA, 0xFA, 0xFA)) > 300);

    shell.set_auth_state(AuthState::WaitPassword { hint: "cat".into() });
    assert_eq!(login.get_step(), LoginStep::Password);
    assert!(login.get_secret());
    assert_eq!(login.get_input(), "", "the field clears between steps");
    assert_eq!(login.get_status(), "TWO-STEP PASSWORD · HINT: CAT");

    shell.set_auth_state(AuthState::Failed("PASSWORD_HASH_INVALID".into()));
    assert_eq!(login.get_step(), LoginStep::Failed);
    assert_eq!(login.get_status(), "PASSWORD_HASH_INVALID");
    assert_eq!(login.get_actions().row_data(0).unwrap().label, "TRY AGAIN");

    // Ready closes Login back to Settings, which now reads SIGNED IN.
    shell.set_auth_state(AuthState::Ready {
        user: "fixtures".into(),
    });
    assert_eq!(ui.get_screen(), Screen::Settings);
    assert_eq!(value(&ui, RowId::Account), "SIGNED IN");
    press(&shell, Key::Escape);
    assert_eq!(ui.get_screen(), Screen::Home);
}
