#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Builds the runtime, stores and services, then opens the window.
//!
//! `flox [--dev-fixtures [PATH]] [--dev-play FILE]`: with `--dev-fixtures` the catalog,
//! posters and library come from a JSON file (default `crates/flox-app/fixtures/browse.json`)
//! instead of TMDB and Telegram, and watch history goes to a scratch file. `--dev-play`
//! opens the player on a local file through mpv (the path goes straight to `loadfile`).

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use flox_app::app::{AppContext, Services, Telegram};
use flox_app::fixtures::{
    FixtureCatalog, FixtureImages, FixtureLibrary, FixtureLibrarySource, Fixtures,
};
use flox_app::launch::{start_queue, start_telegram};
use flox_core::images::{ImageCache, DISK_BYTES, MEM_BYTES};
use flox_core::paths::{AppPaths, Dirs};
use flox_core::progress::ProgressStore;
use flox_core::settings::{Settings, SettingsStore};
use flox_core::tmdb::Tmdb;
use flox_core::tools::{self, Tool};
use flox_player::ffi::MpvLib;
use flox_sys::dirs::SystemDirs;
use parking_lot::RwLock;

/// Command-line options.
#[derive(Debug, Default)]
struct Args {
    fixtures: Option<PathBuf>,
    dev_play: Option<PathBuf>,
}

const USAGE: &str = "usage: flox [--dev-fixtures [PATH]] [--dev-play FILE]";

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
            Some("--dev-play") => match args.next() {
                Some(file) => parsed.dev_play = Some(PathBuf::from(file)),
                None => anyhow::bail!("--dev-play needs a file\n{USAGE}"),
            },
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

/// Loads libmpv from the settings override, the app directory or `PATH`.
fn load_libmpv(settings: &Settings) -> Option<Arc<MpvLib>> {
    let path_env = std::env::var_os("PATH");
    let Some(path) = tools::resolve(
        Tool::LibMpv,
        &SystemDirs.app_dir(),
        path_env.as_deref(),
        settings.libmpv_path.as_deref(),
    ) else {
        tracing::warn!("{} not found; the player is off", Tool::LibMpv.file_name());
        return None;
    };
    match MpvLib::load(&path) {
        Ok(lib) => Some(lib),
        Err(e) => {
            tracing::warn!("cannot load {}: {e}", path.display());
            None
        }
    }
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
        telegram: RwLock::new(Telegram::Offline {
            library: Arc::new(FixtureLibrarySource(library)),
        }),
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
            let stack = start_telegram(runtime.handle(), &paths, &settings.get());
            let services = Services {
                settings: settings.clone(),
                progress: progress.clone(),
                catalog: Arc::new(tmdb),
                images: Arc::new(images),
                telegram: RwLock::new(stack.telegram),
            };
            (services, progress, stack.client, stack.library)
        }
    };

    let player_lib = load_libmpv(&settings.get());

    let queue = td
        .as_ref()
        .and_then(|td| start_queue(runtime.handle(), &paths, &settings, td));
    flox_app::run(AppContext {
        runtime,
        paths,
        settings,
        progress,
        services: Arc::new(services),
        td,
        library,
        queue,
        player_lib,
        dev_play: args.dev_play,
    })
}
