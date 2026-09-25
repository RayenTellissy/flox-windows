//! The player screen without mpv, rendered with the software renderer to
//! `target/snapshots/player_*.png` for review against DESIGN.dark.md:
//!
//! - `player_overlay`: the overlay over a solid black frame (scrim, BACK, eyebrow, title,
//!   seek bar with the buffered band, times, the full button row, the tracks panel, a hint);
//! - `player_resume`: the ASK dialog;
//! - `player_failed`: PLAYBACK FAILED with the LIBMPV NOT FOUND hint, reached through the real
//!   controller flow with no libmpv, no WebView2 and no page player.

// Test helpers outside #[test] functions panic on setup failures too.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use flox_app::app::{Exec, Services, Shell, Telegram};
use flox_app::fixtures::{
    FixtureCatalog, FixtureImages, FixtureLibrary, FixtureLibrarySource, Fixtures, DEFAULT_PATH,
};
use flox_app::focus::Modifiers;
use flox_app::player::controller::{Phase, TracksPanel, ViewState};
use flox_app::player::view::{self, PlayerDeps, BUTTONS, TRACKS};
use flox_app::router::{PlayRequest, Route};
use flox_app::ui::{FocusState, PlayerState};
use flox_app::{AppWindow, Screen};
use flox_core::model::EpisodeKey;
use flox_core::progress::ProgressStore;
use flox_core::settings::{ResumeMode, Settings, SettingsStore};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

struct SnapshotPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for SnapshotPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn services(dir: &Path) -> Arc<Services> {
    let fixtures = Arc::new(Fixtures::load(Path::new(DEFAULT_PATH)).unwrap());
    let history = dir.join("progress.json");
    std::fs::write(&history, serde_json::to_vec(&fixtures.progress).unwrap()).unwrap();
    let library = Arc::new(FixtureLibrary::new(&fixtures));
    let settings = Settings {
        resume_mode: ResumeMode::Ask,
        ..Settings::default()
    };
    Arc::new(Services {
        settings: SettingsStore::new(dir.join("settings.json"), settings),
        progress: Arc::new(ProgressStore::open(&history).unwrap()),
        catalog: Arc::new(FixtureCatalog(fixtures)),
        images: Arc::new(FixtureImages),
        telegram: Telegram::Offline {
            library: Arc::new(FixtureLibrarySource(library)),
        },
    })
}

fn rgb(p: &Rgb8Pixel) -> (u8, u8, u8) {
    (p.r, p.g, p.b)
}

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
    buffer
}

fn count(buffer: &[Rgb8Pixel], color: (u8, u8, u8)) -> usize {
    buffer.iter().filter(|p| rgb(p) == color).count()
}

fn at(buffer: &[Rgb8Pixel], x: u32, y: u32) -> (u8, u8, u8) {
    rgb(&buffer[(y * WIDTH + x) as usize])
}

fn overlay_state() -> ViewState {
    ViewState {
        phase: Phase::Native,
        resume_label: None,
        overlay_visible: true,
        tracks: Some(TracksPanel {
            heading: "AUDIO".to_owned(),
            labels: vec![
                "English · E-AC3 · 5.1".to_owned(),
                "English · TRUEHD · 7.1".to_owned(),
                "Japanese · AAC · 2.0".to_owned(),
            ],
            selected: 0,
        }),
        hint: Some("VOLUME · 85%".to_owned()),
        eyebrow: "S1 · E3".to_owned(),
        title: "In Perpetuity".to_owned(),
        position: "12:34".to_owned(),
        duration: "56:05".to_owned(),
        time: 754.0,
        length: 3365.0,
        paused: true,
        volume: 85,
        nav_mode: false,
        audio_available: true,
        subtitles_available: true,
        quality_available: true,
        next_available: true,
    }
}

fn press(shell: &Rc<Shell>, key: Key) {
    let text = char::from(key).to_string();
    let player = shell.player_view().unwrap();
    player.key(&text, Modifiers::default(), false);
    player.key_released(&text);
    slint::platform::update_timers_and_animations();
}

#[test]
fn player_screen_snapshots() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(SnapshotPlatform {
        window: window.clone(),
    }))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();

    let ui = AppWindow::new().unwrap();
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    ui.show().unwrap();
    let shell = Shell::new(&ui, services(dir.path()), Exec::Inline);
    shell.start();

    // The overlay over a solid black frame, focus on the second audio track.
    ui.set_screen(Screen::Player);
    view::apply(&ui, &overlay_state(), 1400.0, false);
    let focus = ui.global::<FocusState>();
    focus.set_zone(TRACKS.0);
    focus.set_index(1);
    let overlay = snapshot(&window, "player_overlay");
    assert_eq!(
        at(&overlay, 640, 20),
        (0, 0, 0),
        "pure black above the scrim"
    );
    let bottom = at(&overlay, 640, HEIGHT - 4);
    assert!(
        bottom.0 < 0x10 && bottom == (bottom.0, bottom.0, bottom.0),
        "{bottom:?}"
    );
    assert!(
        count(&overlay, (0xFA, 0xFA, 0xFA)) > 1500,
        "glyphs, played band, ring"
    );
    assert!(
        count(&overlay, (0x16, 0x16, 0x16)) > 20_000,
        "the tracks panel card"
    );
    // Focus on play/pause with the panel closed.
    let mut state = overlay_state();
    state.tracks = None;
    state.hint = None;
    view::apply(&ui, &state, 1400.0, false);
    focus.set_zone(BUTTONS.0);
    focus.set_index(1);
    snapshot(&window, "player_controls");

    // The real flow: TV episode with a saved position and resume ASK.
    ui.set_screen(Screen::Home);
    shell.navigate(Route::Player(PlayRequest {
        key: EpisodeKey::episode(95396, 1, 3),
        start_at: Some(754),
        library_only: false,
    }));
    assert_eq!(ui.get_screen(), Screen::Player);
    let player = shell.player_view().unwrap();
    assert_eq!(player.phase(), Some(Phase::Asking));
    let s = ui.global::<PlayerState>();
    assert!(s.get_asking());
    assert_eq!(s.get_resume_label().as_str(), "Resume from 12:34");
    assert_eq!((focus.get_zone(), focus.get_index()), (44, 0));
    snapshot(&window, "player_resume");

    // START OVER: no mpv and no sniffer, so the load fails, reloads once and gives up.
    press(&shell, Key::RightArrow);
    assert_eq!((focus.get_zone(), focus.get_index()), (44, 1));
    press(&shell, Key::Return);
    assert_eq!(player.phase(), Some(Phase::Failed));
    assert_eq!(s.get_stamp().as_str(), "PLAYBACK FAILED");
    assert_eq!(s.get_hint().as_str(), "LIBMPV NOT FOUND");
    assert!(!s.get_overlay());
    let failed = snapshot(&window, "player_failed");
    let grey = failed
        .iter()
        .filter(|p| p.r > 0x40 && p.r == p.g && p.g == p.b)
        .count();
    assert!(grey > 100, "muted stamp and hint");
    assert!(
        failed.iter().all(|p| p.r <= 0x8F),
        "nothing brighter than muted text"
    );

    // BACK leaves the player.
    press(&shell, Key::Escape);
    assert_ne!(ui.get_screen(), Screen::Player);
    assert!(shell.player_view().is_none());
    drop(player);

    // `--dev-play` without libmpv: the file goes to mpv, which is missing, then to the page
    // player, which does not exist here, and the screen ends in PLAYBACK FAILED.
    let mut deps = PlayerDeps::offline();
    deps.dev_file = Some(dir.path().join("clip.mkv"));
    shell.set_player_deps(deps);
    shell.navigate(Route::Player(PlayRequest {
        key: EpisodeKey::movie(0),
        start_at: None,
        library_only: false,
    }));
    let player = shell.player_view().unwrap();
    assert_eq!(player.phase(), Some(Phase::Failed));
    assert_eq!(s.get_title().as_str(), "clip");
    assert_eq!(s.get_hint().as_str(), "LIBMPV NOT FOUND");
    press(&shell, Key::Backspace);
    assert!(shell.player_view().is_none());
}
