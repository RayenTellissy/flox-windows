//! Renders Home, Search and Details from the `--dev-fixtures` data with the software
//! renderer to `target/snapshots/{home,search,details}.png` for review against
//! DESIGN.dark.md. Loads run inline (`Exec::Inline`), so every row, grid and list is
//! filled before the frame is drawn.

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
use flox_app::router::Route;
use flox_app::ui::{DetailsState, FocusState, SearchState};
use flox_app::{AppWindow, Screen};
use flox_core::model::MediaType;
use flox_core::progress::ProgressStore;
use flox_core::settings::{Settings, SettingsStore};
use parking_lot::RwLock;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, WindowAdapter};
use slint::{ComponentHandle, Model, PhysicalSize, Rgb8Pixel};

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
    Arc::new(Services {
        settings: SettingsStore::new(dir.join("settings.json"), Settings::default()),
        progress: Arc::new(ProgressStore::open(&history).unwrap()),
        catalog: Arc::new(FixtureCatalog(fixtures)),
        images: Arc::new(FixtureImages),
        telegram: RwLock::new(Telegram::Offline {
            library: Arc::new(FixtureLibrarySource(library)),
        }),
    })
}

fn press(shell: &Rc<Shell>, key: Key) {
    let text = char::from(key).to_string();
    shell.key(&text, Modifiers::default());
    slint::platform::update_timers_and_animations();
}

fn rgb(p: &Rgb8Pixel) -> (u8, u8, u8) {
    (p.r, p.g, p.b)
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
    buffer.iter().filter(|p| rgb(p) == color).count()
}

#[test]
fn browse_screens_snapshot() {
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

    // Home: focus starts on SEARCH; step down into CONTINUE WATCHING and right once.
    assert_eq!(ui.get_screen(), Screen::Home);
    press(&shell, Key::DownArrow);
    press(&shell, Key::RightArrow);
    let state = ui.global::<FocusState>();
    assert_eq!((state.get_zone(), state.get_index()), (1, 1));
    let home = snapshot(&window, "home");
    assert_eq!(
        rgb(&home[(WIDTH * HEIGHT - 1) as usize]),
        (0x0E, 0x0E, 0x0E)
    );
    assert!(
        count(&home, (0xFA, 0xFA, 0xFA)) > 500,
        "focus ring and titles"
    );
    assert!(
        count(&home, (0x29, 0x7A, 0x3A)) > 10,
        "telegram status dot is green"
    );

    // Search: the query runs at once inline; Down leaves the input for the grid.
    shell.navigate(Route::Search);
    assert_eq!(ui.get_screen(), Screen::Search);
    ui.global::<SearchState>().set_query("the".into());
    shell.search_edited("the", false);
    press(&shell, Key::DownArrow);
    assert_eq!(state.get_zone(), 11);
    let search = snapshot(&window, "search");
    assert!(count(&search, (0xFA, 0xFA, 0xFA)) > 200);
    // BACK from the grid returns to the input, then leaves.
    press(&shell, Key::Escape);
    assert_eq!(state.get_zone(), 10);

    // Details for a TV title with progress: RESUME S2 E3, season 2 preselected.
    shell.navigate(Route::Details {
        id: 95396,
        media: MediaType::Tv,
        library_only: false,
    });
    assert_eq!(ui.get_screen(), Screen::Details);
    assert_eq!(state.get_zone(), 20, "PLAY is focused first");
    let details = snapshot(&window, "details");
    assert!(
        count(&details, (0x29, 0x7A, 0x3A)) > 200,
        "the filled button's green ring"
    );
    assert!(count(&details, (0xFA, 0xFA, 0xFA)) > 2000, "inverted PLAY");

    // BACK unwinds to Search, then Home.
    press(&shell, Key::Escape);
    assert_eq!(ui.get_screen(), Screen::Search);
    press(&shell, Key::Escape);
    assert_eq!(ui.get_screen(), Screen::Home);

    // Library-only Details: the movie header carries the library stamp, and the TV
    // episode list keeps only uploaded episodes (season 2 from progress: E1 to E4).
    shell.navigate(Route::Details {
        id: 693134,
        media: MediaType::Movie,
        library_only: true,
    });
    let details = ui.global::<DetailsState>();
    assert_eq!(
        details.get_meta(),
        "2024 · MOVIE · 167 MIN · LIBRARY · 2160P DV, 1080P"
    );
    assert_eq!(details.get_play_label(), "RESUME");
    shell.navigate(Route::Details {
        id: 95396,
        media: MediaType::Tv,
        library_only: true,
    });
    assert_eq!(details.get_play_label(), "RESUME S2 E3");
    assert_eq!(details.get_seasons().row_count(), 2);
    assert_eq!(details.get_episodes().row_count(), 4);
}
