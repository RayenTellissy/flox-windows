#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Builds the runtime, stores and services, then opens the window.
//!
//! `flox [--dev-fixtures [PATH]]`: with `--dev-fixtures` the catalog, posters and
//! library come from a JSON file (default `crates/flox-app/fixtures/browse.json`)
//! instead of TMDB and Telegram, and watch history goes to a scratch file.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use flox_app::app::{AppContext, QueueEvents, Services, Telegram};
use flox_app::fixtures::{
    FixtureCatalog, FixtureImages, FixtureLibrary, FixtureLibrarySource, Fixtures,
};
use flox_core::images::{ImageCache, DISK_BYTES, MEM_BYTES};
use flox_core::paths::{AppPaths, Dirs};
use flox_core::progress::ProgressStore;
use flox_core::settings::{Settings, SettingsStore};
use flox_core::tmdb::Tmdb;
use flox_core::tools::{self, Tool};
use flox_rip::queue::{Queue, QueueDeps};
use flox_rip::tools::ToolPaths;
use flox_sys::dirs::SystemDirs;
use flox_td::auth::Auth;
use flox_td::client::{TdClient, TdParams};
use flox_td::ffi::TdJson;
use flox_td::library::Library;
use flox_td::transport::TdTransport;

/// Command-line options.
#[derive(Debug, Default)]
struct Args {
    fixtures: Option<PathBuf>,
}

const USAGE: &str = "usage: flox [--dev-fixtures [PATH]]";

fn parse_args(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<Args> {
    let mut parsed = Args::default();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--dev-fixtures") => {
                let path = match args.peek() {
                    Some(next) if !next.to_string_lossy().starts_with("--") => {
                        args.next().map(PathBuf::from)
                    }
                    _ => None,
                };
                parsed.fixtures =
                    Some(path.unwrap_or_else(|| PathBuf::from(flox_app::fixtures::DEFAULT_PATH)));
            }
            Some("--help" | "-h") => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => anyhow::bail!("unknown argument {arg:?}\n{USAGE}"),
        }
    }
    Ok(parsed)
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("FLOX_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

fn open_settings(paths: &AppPaths) -> SettingsStore {
    match SettingsStore::open(paths.settings.clone()) {
        Ok(store) => store,
        Err(e) => {
            tracing::warn!("settings unreadable, using defaults: {e}");
            SettingsStore::new(paths.settings.clone(), Settings::default())
        }
    }
}

fn open_progress(
    path: &std::path::Path,
    fallback: &std::path::Path,
) -> anyhow::Result<ProgressStore> {
    match ProgressStore::open(path) {
        Ok(store) => Ok(store),
        Err(e) => {
            tracing::warn!("watch history unreadable, starting empty: {e}");
            Ok(ProgressStore::open(fallback)?)
        }
    }
}

/// Starts TDLib when the API id and hash are set and tdjson resolves.
fn start_telegram(
    runtime: &tokio::runtime::Runtime,
    paths: &AppPaths,
    settings: &Settings,
) -> (Telegram, Option<Arc<TdClient>>, Option<Arc<Library>>) {
    let (Some(api_id), Some(api_hash)) = (
        settings.effective_telegram_api_id(),
        settings.effective_telegram_api_hash(),
    ) else {
        return (Telegram::NotConfigured, None, None);
    };
    let path_env = std::env::var_os("PATH");
    let Some(tdjson) = tools::resolve(
        Tool::TdJson,
        &SystemDirs.app_dir(),
        path_env.as_deref(),
        settings.tdjson_path.as_deref(),
    ) else {
        tracing::warn!("{} not found; Telegram is off", Tool::TdJson.file_name());
        return (Telegram::Unavailable, None, None);
    };
    let lib = match TdJson::load(&tdjson) {
        Ok(lib) => lib,
        Err(e) => {
            tracing::warn!("cannot load {}: {e}", tdjson.display());
            return (Telegram::Unavailable, None, None);
        }
    };
    let params = TdParams {
        api_id,
        api_hash,
        db_dir: paths.tdlib_db.clone(),
        files_dir: paths.tdlib_files.clone(),
        device_model: if cfg!(windows) {
            "Windows"
        } else {
            std::env::consts::OS
        }
        .to_owned(),
        app_version: flox_core::VERSION.to_owned(),
    };
    let _entered = runtime.enter();
    let client = match TdClient::start(lib, params) {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!("cannot start TDLib: {e}");
            return (Telegram::Unavailable, None, None);
        }
    };
    let transport: Arc<dyn TdTransport> = client.clone();
    let auth = Arc::new(Auth::new(transport.clone()));
    let library = Arc::new(Library::new(transport));
    (
        Telegram::Connected {
            auth,
            library: library.clone(),
        },
        Some(client),
        Some(library),
    )
}

/// The upload queue, when Telegram is running and ffmpeg and ffprobe resolve. Jobs
/// work under `%TEMP%\flox\<uuid>`; VidLink pages are sniffed with WebView2.
fn start_queue(
    runtime: &tokio::runtime::Runtime,
    paths: &AppPaths,
    settings: &SettingsStore,
    td: Option<&Arc<TdClient>>,
) -> Option<Arc<Queue>> {
    let td = td?;
    let s = settings.get();
    let app_dir = SystemDirs.app_dir();
    let path_env = std::env::var_os("PATH");
    let find = |tool: Tool, over: Option<&std::path::Path>| {
        let found = tools::resolve(tool, &app_dir, path_env.as_deref(), over);
        if found.is_none() {
            tracing::warn!("{} not found", tool.file_name());
        }
        found
    };
    let ffmpeg = find(Tool::Ffmpeg, s.ffmpeg_path.as_deref());
    let ffprobe = find(Tool::Ffprobe, s.ffmpeg_path.as_deref());
    let ytdlp = find(Tool::YtDlp, s.ytdlp_path.as_deref());
    let (Some(ffmpeg), Some(ffprobe)) = (ffmpeg, ffprobe) else {
        tracing::warn!("the upload queue is off until ffmpeg and ffprobe are found");
        return None;
    };
    let transport: Arc<dyn TdTransport> = td.clone();
    let _entered = runtime.enter();
    Some(Queue::new(QueueDeps {
        td: transport,
        sniffer: Some(flox_web::platform_sniffer(
            flox_web::assets::ScriptOptions::default(),
        )),
        tools: ToolPaths {
            ffmpeg,
            ffprobe,
            ytdlp,
        },
        temp_root: paths.temp.clone(),
        settings: settings.clone(),
        hooks: Arc::new(QueueEvents::default()),
    }))
}

/// Offline services from a fixture file. Watch history is seeded into a scratch
/// file so the real one is never touched.
fn fixture_services(
    path: &std::path::Path,
    paths: &AppPaths,
    settings: SettingsStore,
) -> anyhow::Result<(Services, Arc<ProgressStore>)> {
    let fixtures = Arc::new(
        Fixtures::load(path).with_context(|| format!("reading fixtures {}", path.display()))?,
    );
    let history = paths.temp.join("dev-fixtures").join("progress.json");
    if let Some(dir) = history.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&history, serde_json::to_vec(&fixtures.progress)?)?;
    let progress = Arc::new(ProgressStore::open(&history)?);
    let library = Arc::new(FixtureLibrary::new(&fixtures));
    let services = Services {
        settings,
        progress: progress.clone(),
        catalog: Arc::new(FixtureCatalog(fixtures)),
        images: Arc::new(FixtureImages),
        telegram: Telegram::Offline {
            library: Arc::new(FixtureLibrarySource(library)),
        },
    };
    Ok((services, progress))
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    let args = parse_args(std::env::args_os().skip(1))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("flox-rt")
        .build()?;
    let paths = AppPaths::from_dirs(&SystemDirs);
    let settings = open_settings(&paths);

    if let Err(e) = flox_sys::notify::ensure_app_identity() {
        tracing::warn!("app identity: {e}");
    }

    let (services, progress, td, library) = match &args.fixtures {
        Some(path) => {
            let (services, progress) = fixture_services(path, &paths, settings.clone())?;
            (services, progress, None, None)
        }
        None => {
            let progress = Arc::new(open_progress(
                &paths.progress,
                &paths.temp.join("progress-unreadable.json"),
            )?);
            let tmdb = Tmdb::new(settings.clone())?;
            let images = ImageCache::new(paths.image_cache.clone(), MEM_BYTES, DISK_BYTES);
            let (telegram, td, library) = start_telegram(&runtime, &paths, &settings.get());
            let services = Services {
                settings: settings.clone(),
                progress: progress.clone(),
                catalog: Arc::new(tmdb),
                images: Arc::new(images),
                telegram,
            };
            (services, progress, td, library)
        }
    };

    let queue = start_queue(&runtime, &paths, &settings, td.as_ref());
    flox_app::run(AppContext {
        runtime,
        paths,
        settings,
        progress,
        services: Arc::new(services),
        td,
        library,
        queue,
        player_lib: None,
    })
}
