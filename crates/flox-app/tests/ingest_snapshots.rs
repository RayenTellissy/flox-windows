//! Renders the ingest screens from fixtures with the software renderer to
//! `target/snapshots/`: Details in ingest mode, the paste links, local files and
//! 4KHDHub dialogs, the Queue with jobs in every state, and the Library manager with
//! its delete confirmation. Loads run inline (`Exec::Inline`); the queue, channel,
//! 4KHDHub and file picker are in-memory fakes.

// Test helpers outside #[test] functions panic on setup failures too.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use async_trait::async_trait;
use flox_app::app::{Exec, FilePicker, HubSource, Ingest, LibraryAdmin, Services, Shell, Telegram};
use flox_app::fixtures::{
    FixtureCatalog, FixtureImages, FixtureLibrary, FixtureLibrarySource, FixtureQueue, Fixtures,
    DEFAULT_PATH,
};
use flox_app::focus::Modifiers;
use flox_app::router::Route;
use flox_app::ui::{
    DetailsState, FilesState, FocusState, HomeState, HubState, IngestBar, LibraryState, PasteState,
    QueueState,
};
use flox_app::{AppWindow, Screen};
use flox_core::error::Result;
use flox_core::model::{EpisodeKey, MediaType, TitleDetails};
use flox_core::progress::ProgressStore;
use flox_core::settings::{Settings, SettingsStore};
use flox_rip::hub::{HubFile, Variant};
use flox_rip::job::{Job, JobState, JobView, Source, Tag};
use flox_td::library::{Entry, Part};
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
    fn create_window_adapter(
        &self,
    ) -> std::result::Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn fixtures() -> Arc<Fixtures> {
    Arc::new(Fixtures::load(Path::new(DEFAULT_PATH)).unwrap())
}

fn services(dir: &Path, fixtures: Arc<Fixtures>) -> Arc<Services> {
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

/// The fixture prints as channel entries.
struct FakeAdmin(Vec<Entry>);

impl FakeAdmin {
    fn new(fixtures: &Fixtures) -> Self {
        let entries = fixtures
            .library
            .iter()
            .map(|p| {
                let key = match p.media {
                    MediaType::Movie => EpisodeKey::movie(p.id),
                    MediaType::Tv => EpisodeKey::episode(p.id, p.season, p.episode),
                };
                Entry {
                    key,
                    quality: p.quality.clone(),
                    codec: if p.quality.contains("2160") {
                        "hevc".to_owned()
                    } else {
                        "h264".to_owned()
                    },
                    parts: vec![Part {
                        message_id: p.message_id,
                        file_id: 1,
                        size: p.size,
                    }],
                    subtitle: None,
                    size: p.size.max(1_400_000_000),
                    newest_message_id: p.message_id,
                }
            })
            .collect();
        Self(entries)
    }
}

#[async_trait]
impl LibraryAdmin for FakeAdmin {
    async fn entries(&self, _channel_title: &str) -> Result<Vec<Entry>> {
        Ok(self.0.clone())
    }

    async fn delete(&self, _entry: &Entry) -> Result<()> {
        Ok(())
    }
}

/// Finds TV titles only; the page lists two seasons.
struct FakeHub;

fn hub_file(episode: u32, gib_tenths: u64, tag: &str) -> HubFile {
    HubFile {
        episode: Some(episode),
        name: format!("Severance.S02E{episode:02}.{tag}.mkv"),
        size_bytes: (gib_tenths << 30) / 10,
        link: format!("https://hubcloud.test/drive/{tag}{episode}"),
    }
}

#[async_trait]
impl HubSource for FakeHub {
    async fn find(&self, title: &TitleDetails) -> Result<Option<String>> {
        Ok((title.summary.media == MediaType::Tv)
            .then(|| "https://4khdhub.one/severance-series-2186/".to_owned()))
    }

    async fn variants(&self, _page: &str, _media: MediaType) -> Result<Vec<Variant>> {
        Ok(vec![
            Variant::new(
                "S02 DV HDR 2160p WEB-DL H265".to_owned(),
                Some(2),
                (1..=6).map(|e| hub_file(e, 62, "2160p.DV")).collect(),
            ),
            Variant::new(
                "S02 SDR 1080p WEB-DL H264".to_owned(),
                Some(2),
                (1..=10).map(|e| hub_file(e, 24, "1080p")).collect(),
            ),
            Variant::new(
                "S02 SDR 720p WEB-DL H264".to_owned(),
                Some(2),
                (1..=10).map(|e| hub_file(e, 9, "720p")).collect(),
            ),
            Variant::new(
                "S01 SDR 1080p WEB-DL H264".to_owned(),
                Some(1),
                (1..=9).map(|e| hub_file(e, 22, "1080p")).collect(),
            ),
        ])
    }
}

struct FakePicker;

#[async_trait]
impl FilePicker for FakePicker {
    async fn pick(&self, _multi: bool) -> Vec<PathBuf> {
        vec![
            PathBuf::from("D:/Rips/Severance.S02E05.Trojan's.Horse.2160p.DV.HEVC.mkv"),
            PathBuf::from("D:/Rips/Severance.S02E04.Woes.Hollow.2160p.HDR10.mkv"),
            PathBuf::from("D:/Rips/extras/behind the scenes 10.mkv"),
            PathBuf::from("D:/Rips/extras/behind the scenes 9.mkv"),
        ]
    }
}

fn job(
    key: EpisodeKey,
    title: &str,
    state: JobState,
    detail: &str,
    progress: Option<f32>,
) -> JobView {
    JobView {
        job: Job::new(key, title, Source::Page, Some(Tag::Sdr)),
        state,
        detail: detail.to_owned(),
        progress,
        attempt: 0,
    }
}

fn queue_views() -> Vec<JobView> {
    vec![
        job(
            EpisodeKey::movie(693134),
            "Dune: Part Two",
            JobState::Done,
            "",
            None,
        ),
        job(
            EpisodeKey::episode(95396, 2, 5),
            "Severance",
            JobState::Uploading,
            "2 parts at once",
            Some(0.72),
        ),
        job(
            EpisodeKey::episode(95396, 2, 6),
            "Severance",
            JobState::Downloading,
            "frame=21480 fps=412 time=00:14:56.20 bitrate=8012.4kbits/s speed=17.2x",
            Some(0.31),
        ),
        job(
            EpisodeKey::episode(95396, 2, 7),
            "Severance",
            JobState::Failed("not enough disk space: need 12.4 GB, have 3.1 GB".to_owned()),
            "",
            None,
        ),
        job(
            EpisodeKey::episode(95396, 2, 8),
            "Severance",
            JobState::Queued,
            "",
            None,
        ),
        job(
            EpisodeKey::episode(136315, 1, 2),
            "The Bear",
            JobState::Cancelled,
            "",
            None,
        ),
    ]
}

fn press_text(shell: &Rc<Shell>, text: &str) {
    shell.key(text, Modifiers::default());
    slint::platform::update_timers_and_animations();
}

fn press(shell: &Rc<Shell>, key: Key) {
    press_text(shell, &char::from(key).to_string());
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
    image.save(dir.join(format!("{name}.png"))).unwrap();
    buffer
}

fn count(buffer: &[Rgb8Pixel], color: (u8, u8, u8)) -> usize {
    buffer.iter().filter(|p| rgb(p) == color).count()
}

/// A click on an item: it takes focus and activates.
fn click(ui: &AppWindow, zone: i32, index: i32) {
    ui.global::<FocusState>().invoke_clicked(zone, index);
    slint::platform::update_timers_and_animations();
}

fn focus(ui: &AppWindow) -> (i32, i32) {
    let state = ui.global::<FocusState>();
    (state.get_zone(), state.get_index())
}

#[test]
fn ingest_screens_snapshot() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(SnapshotPlatform {
        window: window.clone(),
    }))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fixtures = fixtures();

    let ui = AppWindow::new().unwrap();
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    ui.show().unwrap();
    let shell = Shell::new(&ui, services(dir.path(), fixtures.clone()), Exec::Inline);
    shell.set_ingest(Ingest {
        queue: Some(Arc::new(FixtureQueue::new(queue_views()))),
        admin: Some(Arc::new(FakeAdmin::new(&fixtures))),
        hub: Arc::new(FakeHub),
        picker: Arc::new(FakePicker),
    });
    shell.start();

    // The top bar counts unfinished jobs: uploading, downloading, queued.
    assert_eq!(ui.global::<HomeState>().get_queue_count(), 3);

    // Details in ingest mode: Severance season 2, nothing selected yet.
    shell.navigate(Route::Details {
        id: 95396,
        media: MediaType::Tv,
        library_only: false,
    });
    let details = ui.global::<DetailsState>();
    let bar = ui.global::<IngestBar>();
    assert!(details.get_ingest_mode());
    assert!(!bar.get_vidlink_enabled());
    assert_eq!(bar.get_hub_label(), "SEASON FROM 4KHDHUB");
    press(&shell, Key::DownArrow);
    assert_eq!(focus(&ui), (23, 0), "Down from PLAY reaches the action bar");

    // Select E2 and E3 with Space and X from the list; A selects all, N none.
    press(&shell, Key::DownArrow);
    press(&shell, Key::DownArrow);
    assert_eq!(focus(&ui).0, 22);
    press(&shell, Key::DownArrow);
    press_text(&shell, "a");
    assert_eq!(
        bar.get_line(),
        "10 SELECTED · SPACE OR X SELECTS · A ALL · N NONE"
    );
    press_text(&shell, "n");
    press_text(&shell, " ");
    press(&shell, Key::DownArrow);
    press_text(&shell, "x");
    assert!(bar.get_line().starts_with("2 SELECTED"));
    let episodes = details.get_episodes();
    let marks: Vec<bool> = (0..episodes.row_count())
        .map(|i| episodes.row_data(i).unwrap().selected)
        .collect();
    assert_eq!(&marks[..4], &[false, true, true, false]);
    assert!(bar.get_vidlink_enabled());
    assert_eq!(bar.get_hub_label(), "4KHDHUB");
    // Back to the top so the header and bar show.
    press(&shell, Key::UpArrow);
    press(&shell, Key::UpArrow);
    press(&shell, Key::UpArrow);
    press(&shell, Key::UpArrow);
    assert_eq!(focus(&ui), (23, 0));
    let shot = snapshot(&window, "details-ingest");
    assert!(
        count(&shot, (0x29, 0x7A, 0x3A)) > 100,
        "the filled VIDLINK ring"
    );

    // PASTE LINKS: prefilled from the lowest selected episode (E2).
    press(&shell, Key::RightArrow);
    press(&shell, Key::Return);
    let paste = ui.global::<PasteState>();
    assert!(paste.get_open());
    assert_eq!(paste.get_rows().row_data(0).unwrap().episode, "2");
    assert_eq!(focus(&ui), (2001, 0), "the first URL field");
    assert!(ui.global::<FocusState>().get_editing());
    paste.invoke_url_edited(0, "https://cdn.example.net/severance/s02e02.mkv".into());
    // ADD FIELD twice (footer index 0), with a second URL and an ftp one.
    click(&ui, 80, 0);
    assert_eq!(paste.get_rows().row_data(1).unwrap().episode, "3");
    paste.invoke_url_edited(1, "https://cdn.example.net/severance/s02e03.mkv".into());
    click(&ui, 80, 0);
    paste.invoke_url_edited(2, "ftp://files.example.net/s02e04.mkv".into());
    assert_eq!(paste.get_queue_label(), "QUEUE 2");
    snapshot(&window, "paste-links");
    // Esc from a field closes the dialog; the selection stays (as on the Mac).
    press(&shell, Key::Escape);
    assert!(!paste.get_open());
    assert!(bar.get_line().starts_with("2 SELECTED"));
    assert_eq!(focus(&ui), (23, 1));

    // LOCAL FILES: the picker runs first; names give episodes and tags.
    press(&shell, Key::RightArrow);
    press(&shell, Key::Return);
    let files = ui.global::<FilesState>();
    assert!(files.get_open());
    let rows = files.get_rows();
    let parsed: Vec<(String, String)> = (0..rows.row_count())
        .map(|i| {
            let r = rows.row_data(i).unwrap();
            (r.episode.to_string(), r.tag.to_string())
        })
        .collect();
    assert_eq!(
        parsed,
        vec![
            ("2".into(), "SDR".into()),
            ("3".into(), "SDR".into()),
            ("4".into(), "HDR".into()),
            ("5".into(), "DV".into()),
        ]
    );
    assert_eq!(files.get_queue_label(), "QUEUE 4");
    assert_eq!(focus(&ui), (83, 2), "QUEUE N");
    snapshot(&window, "local-files");
    // Closing the files dialog clears the selection.
    press(&shell, Key::Escape);
    assert!(!files.get_open());
    assert!(bar.get_line().starts_with("0 SELECTED"));

    // 4KHDHUB for the whole season: variants for season 2 only, the complete one
    // preselected, E1–E4 already uploaded at 1080p.
    click(&ui, 23, 3);
    let hub = ui.global::<HubState>();
    assert!(hub.get_open());
    assert_eq!(hub.get_phase(), 2);
    let variants = hub.get_variants();
    assert_eq!(variants.row_count(), 3);
    assert_eq!(hub.get_chosen(), 1);
    let row = variants.row_data(0).unwrap();
    assert_eq!((row.count.as_str(), row.complete), ("6 OF 10", false));
    let row = variants.row_data(1).unwrap();
    assert_eq!(row.badge, "4 UPLOADED");
    assert_eq!(hub.get_note(), "4 ALREADY UPLOADED AT THIS QUALITY");
    assert_eq!(hub.get_queue_label(), "QUEUE 6");
    assert_eq!(hub.get_host(), "4khdhub.one");
    snapshot(&window, "hub-variants");
    // Toggling skip queues all ten; QUEUE adds them and closes.
    click(&ui, 89, 0);
    assert_eq!(hub.get_queue_label(), "QUEUE 10");
    click(&ui, 90, 1);
    assert!(!hub.get_open());
    assert_eq!(
        bar.get_line(),
        "0 SELECTED · SPACE OR X SELECTS · A ALL · N NONE · 10 QUEUED"
    );
    assert_eq!(ui.global::<HomeState>().get_queue_count(), 13);

    // A movie the site does not list: the URL field and LOAD.
    shell.navigate(Route::Details {
        id: 693134,
        media: MediaType::Movie,
        library_only: false,
    });
    assert!(bar.get_vidlink_enabled());
    click(&ui, 23, 3);
    assert_eq!(hub.get_phase(), 1);
    assert_eq!(
        hub.get_status(),
        "NOT FOUND ON 4KHDHUB. PASTE THE TITLE'S PAGE URL:"
    );
    assert_eq!(focus(&ui), (86, 0));
    snapshot(&window, "hub-not-found");
    press(&shell, Key::Escape);
    assert!(!hub.get_open());

    // Queue: the seeded jobs (plus the ten just queued), newest last.
    shell.navigate(Route::Queue);
    assert_eq!(ui.get_screen(), Screen::Queue);
    let queue = ui.global::<QueueState>();
    assert_eq!(queue.get_footer(), "WORKING");
    let rows = queue.get_rows();
    assert_eq!(rows.row_count(), 16);
    let failed = rows.row_data(3).unwrap();
    assert_eq!(
        failed.state,
        "FAILED · NOT ENOUGH DISK SPACE: NEED 12.4 GB, HAVE 3.1 GB"
    );
    let actions: Vec<String> = (0..failed.actions.row_count())
        .map(|i| failed.actions.row_data(i).unwrap().to_string())
        .collect();
    assert_eq!(actions, vec!["RETRY", "REMOVE"]);
    assert_eq!(focus(&ui), (1000, 0));
    let shot = snapshot(&window, "queue");
    let green = shot
        .iter()
        .filter(|p| p.g > p.r.saturating_add(24) && p.g > p.b.saturating_add(16))
        .count();
    assert!(green > 20, "DONE in terminal green");
    // RETRY the failed job, then CLEAR FINISHED drops Done and Cancelled.
    click(&ui, 1003, 0);
    assert_eq!(rows.row_data(3).unwrap().state, "QUEUED");
    assert_eq!(focus(&ui), (1003, 0), "focus stays on the retried job");
    click(&ui, 30, 0);
    assert_eq!(queue.get_rows().row_count(), 14);

    // Library manager: grouped by title, then the delete confirmation.
    shell.navigate(Route::Library);
    assert_eq!(ui.get_screen(), Screen::Library);
    let library = ui.global::<LibraryState>();
    assert_eq!(library.get_stamp(), "");
    let groups = library.get_groups();
    let titles: Vec<String> = (0..groups.row_count())
        .map(|i| groups.row_data(i).unwrap().title.to_string())
        .collect();
    assert_eq!(
        titles,
        vec!["ARRIVAL", "DARK", "DUNE: PART TWO", "SEVERANCE"]
    );
    press(&shell, Key::DownArrow);
    assert_eq!(focus(&ui), (33, 0));
    snapshot(&window, "library");
    press(&shell, Key::Return);
    assert!(library.get_confirm_open());
    assert_eq!(
        library.get_confirm_text(),
        "Arrival · MOVIE · 1080P H264 · 7.0 GB"
    );
    assert_eq!(focus(&ui), (34, 0), "CANCEL is focused first");
    snapshot(&window, "library-confirm");
    press(&shell, Key::Escape);
    assert!(!library.get_confirm_open());
    assert_eq!(ui.get_screen(), Screen::Library);
    assert_eq!(focus(&ui), (33, 0));
}
