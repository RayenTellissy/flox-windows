//! What `main` starts besides the window, and what rebuilds it while the app runs.
//!
//! - [`sweep_temp`] empties the Flox temp folder at launch (job leftovers and the
//!   WebView2 profiles).
//! - [`start_telegram`] loads tdjson and starts the one [`TdClient`] when the API id
//!   and hash are set; [`start_queue`] builds the upload queue over it once ffmpeg and
//!   ffprobe resolve.
//! - [`Integration`] is the Settings seam ([`Shell::set_telegram_restart`]). When the Telegram credentials change it closes the running
//!   TDLib instance, waits for `authorizationStateClosed` and starts a new one with
//!   the new parameters behind the same [`TdClient`] ([`TdClient::restart_with`]), so
//!   the auth watcher, the library, the player's `flox://` streams and the queue's
//!   uploader all follow it; `td_receive` allows one client per process, so a second
//!   client is never created. When Telegram was off at launch (no credentials, or
//!   tdjson missing) it starts the stack and hands it to the shell.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use flox_core::paths::{AppPaths, Dirs};
use flox_core::settings::{Settings, SettingsStore};
use flox_core::tools::{self, Tool};
use flox_rip::queue::{Queue, QueueDeps};
use flox_rip::tools::ToolPaths;
use flox_sys::dirs::SystemDirs;
use flox_td::auth::Auth;
use flox_td::client::{TdClient, TdParams};
use flox_td::ffi::TdJson;
use flox_td::library::Library;
use flox_td::transport::TdTransport;
use tokio::runtime::Handle;

use crate::app::{JobQueue, LibraryAdmin, QueueEvents, Services, Shell, Telegram};
use crate::player::mpv_engine::TdAccess;

/// The Telegram stack `main` (or [`Integration`]) started.
pub struct TelegramStack {
    pub telegram: Telegram,
    /// Present when the client started.
    pub client: Option<Arc<TdClient>>,
    pub library: Option<Arc<Library>>,
}

impl TelegramStack {
    fn off(telegram: Telegram) -> Self {
        Self {
            telegram,
            client: None,
            library: None,
        }
    }
}

/// `setTdlibParameters` for this app: the database and files under the app's data
/// folder, `device_model = "Windows"` (the OS name on development builds).
pub fn td_params(paths: &AppPaths, api_id: i32, api_hash: String) -> TdParams {
    TdParams {
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
    }
}

/// The API id and hash in effect, when both are set.
pub fn credentials(settings: &Settings) -> Option<(i32, String)> {
    Some((
        settings.effective_telegram_api_id()?,
        settings.effective_telegram_api_hash()?,
    ))
}

/// Starts TDLib when the API id and hash are set and tdjson resolves.
pub fn start_telegram(runtime: &Handle, paths: &AppPaths, settings: &Settings) -> TelegramStack {
    let Some((api_id, api_hash)) = credentials(settings) else {
        return TelegramStack::off(Telegram::NotConfigured);
    };
    let path_env = std::env::var_os("PATH");
    let Some(tdjson) = tools::resolve(
        Tool::TdJson,
        &SystemDirs.app_dir(),
        path_env.as_deref(),
        settings.tdjson_path.as_deref(),
    ) else {
        tracing::warn!("{} not found; Telegram is off", Tool::TdJson.file_name());
        return TelegramStack::off(Telegram::Unavailable);
    };
    let lib = match TdJson::load(&tdjson) {
        Ok(lib) => lib,
        Err(e) => {
            tracing::warn!("cannot load {}: {e}", tdjson.display());
            return TelegramStack::off(Telegram::Unavailable);
        }
    };
    let _entered = runtime.enter();
    let client = match TdClient::start(lib, td_params(paths, api_id, api_hash)) {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!("cannot start TDLib: {e}");
            return TelegramStack::off(Telegram::Unavailable);
        }
    };
    let transport: Arc<dyn TdTransport> = client.clone();
    let auth = Arc::new(Auth::new(transport.clone()));
    let library = Arc::new(Library::new(transport));
    TelegramStack {
        telegram: Telegram::Connected {
            auth,
            library: library.clone(),
        },
        client: Some(client),
        library: Some(library),
    }
}

/// ffmpeg, ffprobe (next to the resolved ffmpeg) and yt-dlp from the app folder,
/// `PATH` and the settings overrides. `None` while ffmpeg or ffprobe is missing.
pub fn resolve_tool_paths(settings: &Settings) -> Option<ToolPaths> {
    let app_dir = SystemDirs.app_dir();
    let path_env = std::env::var_os("PATH");
    let find = |tool: Tool, over: Option<&Path>| {
        let found = tools::resolve(tool, &app_dir, path_env.as_deref(), over);
        if found.is_none() {
            tracing::warn!("{} not found", tool.file_name());
        }
        found
    };
    let ffmpeg = find(Tool::Ffmpeg, settings.ffmpeg_path.as_deref());
    let ffprobe = find(Tool::Ffprobe, settings.ffmpeg_path.as_deref());
    let ytdlp = find(Tool::YtDlp, settings.ytdlp_path.as_deref());
    Some(ToolPaths {
        ffmpeg: ffmpeg?,
        ffprobe: ffprobe?,
        ytdlp,
    })
}

/// The upload queue over `td`, when ffmpeg and ffprobe resolve. Jobs work under
/// `paths.temp\<uuid>` (`%TEMP%\flox`); VidLink pages are sniffed with WebView2.
pub fn start_queue(
    runtime: &Handle,
    paths: &AppPaths,
    settings: &SettingsStore,
    td: &Arc<TdClient>,
) -> Option<Arc<Queue>> {
    let Some(tools) = resolve_tool_paths(&settings.get()) else {
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
        tools,
        temp_root: paths.temp.clone(),
        settings: settings.clone(),
        hooks: Arc::new(QueueEvents::default()),
    }))
}

/// What a change of credentials does to the Telegram stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialsStep {
    /// A client runs and new credentials are set: restart it with them.
    Restart(i32, String),
    /// A client runs and the credentials were cleared: close it.
    Close,
    /// No client yet and credentials are set: start one.
    Start,
    /// No client and no credentials: Telegram stays off.
    Off,
}

/// Decides [`CredentialsStep`] from whether a client runs and the credentials in effect.
pub fn credentials_step(
    client_running: bool,
    credentials: Option<(i32, String)>,
) -> CredentialsStep {
    match (client_running, credentials) {
        (true, Some((id, hash))) => CredentialsStep::Restart(id, hash),
        (true, None) => CredentialsStep::Close,
        (false, Some(_)) => CredentialsStep::Start,
        (false, None) => CredentialsStep::Off,
    }
}

/// The running stack [`Integration`] keeps.
#[derive(Default)]
struct Running {
    client: Option<Arc<TdClient>>,
    /// The `Telegram::Connected` built for `client`, put back after a restart.
    connected: Option<Telegram>,
    library: Option<Arc<Library>>,
    queue: Option<Arc<Queue>>,
}

/// Keeps the Telegram stack and the queue in step with Settings (see the module docs).
/// Lives on the UI thread.
pub struct Integration {
    runtime: Handle,
    paths: AppPaths,
    settings: SettingsStore,
    services: Arc<Services>,
    shell: Weak<Shell>,
    running: RefCell<Running>,
}

impl Integration {
    /// Takes over what `main` started.
    pub fn new(
        runtime: Handle,
        paths: AppPaths,
        services: Arc<Services>,
        shell: &Rc<Shell>,
        stack: TelegramStack,
        queue: Option<Arc<Queue>>,
    ) -> Rc<Self> {
        let settings = services.settings.clone();
        let connected = stack.client.is_some().then(|| stack.telegram.clone());
        Rc::new(Self {
            runtime,
            paths,
            settings,
            services,
            shell: Rc::downgrade(shell),
            running: RefCell::new(Running {
                client: stack.client,
                connected,
                library: stack.library,
                queue,
            }),
        })
    }

    /// Installs the credentials hook on `shell`.
    pub fn install(self: &Rc<Self>, shell: &Rc<Shell>) {
        let me = Rc::downgrade(self);
        shell.set_telegram_restart(move |s| {
            if let Some(me) = me.upgrade() {
                me.credentials_changed(s);
            }
        });
    }

    /// New API id or hash (called by Settings after saving them).
    pub fn credentials_changed(&self, settings: &Settings) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let client = self.running.borrow().client.clone();
        match credentials_step(client.is_some(), credentials(settings)) {
            CredentialsStep::Restart(api_id, api_hash) => {
                let Some(client) = client else {
                    return;
                };
                let (connected, queue) = {
                    let running = self.running.borrow();
                    (running.connected.clone(), running.queue.clone())
                };
                if let Some(queue) = &queue {
                    queue.reset_channel();
                }
                if let Some(connected) = connected {
                    self.services.set_telegram(connected);
                }
                shell.telegram_replaced();
                let params = td_params(&self.paths, api_id, api_hash);
                self.runtime.spawn(async move {
                    match client.restart_with(params).await {
                        Ok(()) => tracing::info!("TDLib restarted with new credentials"),
                        Err(e) => tracing::warn!("cannot restart TDLib: {e}"),
                    }
                });
            }
            CredentialsStep::Close => {
                self.services.set_telegram(Telegram::NotConfigured);
                shell.telegram_replaced();
                if let Some(client) = client {
                    self.runtime.spawn(async move {
                        if let Err(e) = client.close().await {
                            tracing::warn!("cannot close TDLib: {e}");
                        }
                    });
                }
            }
            CredentialsStep::Start => self.start(&shell, settings),
            CredentialsStep::Off => {
                self.services.set_telegram(Telegram::NotConfigured);
                shell.telegram_replaced();
            }
        }
    }

    fn start(&self, shell: &Rc<Shell>, settings: &Settings) {
        let stack = start_telegram(&self.runtime, &self.paths, settings);
        let (Some(client), Some(library)) = (stack.client.clone(), stack.library.clone()) else {
            self.services.set_telegram(stack.telegram);
            shell.telegram_replaced();
            return;
        };
        let queue = start_queue(&self.runtime, &self.paths, &self.settings, &client);
        {
            let mut running = self.running.borrow_mut();
            running.client = Some(client.clone());
            running.connected = Some(stack.telegram.clone());
            running.library = Some(library.clone());
            running.queue = queue.clone();
        }
        self.services.set_telegram(stack.telegram);
        shell.set_player_telegram(
            Some(TdAccess {
                transport: client,
                runtime: self.runtime.clone(),
            }),
            Some(library.clone()),
        );
        shell.connect_ingest(
            queue.map(|q| -> Arc<dyn JobQueue> { q }),
            Some(library as Arc<dyn LibraryAdmin>),
        );
        shell.telegram_replaced();
    }
}

/// True when `root` is a folder Flox owns under the system temp folder, safe to empty:
/// not the temp folder itself, not one of its ancestors, and named `flox` or `temp`
/// (`FLOX_HOME\temp` on development builds).
pub fn is_flox_temp(root: &Path, system_temp: &Path) -> bool {
    let named = root
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("flox") || n.eq_ignore_ascii_case("temp"));
    named && root != system_temp && !system_temp.starts_with(root)
}

/// Empties the job temp root (`%TEMP%\flox`: leftover job folders and the WebView2
/// profiles under `webview2\`) and the WebView2 profile folder `flox-web` uses when
/// it sits elsewhere. Runs at launch, before anything writes there.
pub fn sweep_temp(paths: &AppPaths) {
    let system_temp = std::env::temp_dir();
    if is_flox_temp(&paths.temp, &system_temp) {
        if let Err(e) = flox_rip::temp::sweep(&paths.temp) {
            tracing::warn!("temp sweep of {}: {e}", paths.temp.display());
        }
    } else {
        tracing::warn!("not sweeping {}: not a Flox folder", paths.temp.display());
    }
    if let Some(webview2) = webview2_root() {
        if !webview2.starts_with(&paths.temp) {
            match std::fs::remove_dir_all(&webview2) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("cannot remove {}: {e}", webview2.display()),
            }
        }
    }
}

/// `%TEMP%\flox\webview2`, the parent of every WebView2 profile the app creates.
fn webview2_root() -> Option<PathBuf> {
    let profile = flox_web::host::default_user_data("sniffer");
    let root = profile.parent()?.to_path_buf();
    let owner = root.parent()?;
    is_flox_temp(owner, &std::env::temp_dir()).then_some(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_steps() {
        let creds = Some((42, "hash".to_owned()));
        assert_eq!(
            credentials_step(true, creds.clone()),
            CredentialsStep::Restart(42, "hash".to_owned())
        );
        assert_eq!(credentials_step(true, None), CredentialsStep::Close);
        assert_eq!(credentials_step(false, creds), CredentialsStep::Start);
        assert_eq!(credentials_step(false, None), CredentialsStep::Off);
    }

    #[test]
    fn device_model_and_folders() {
        let paths = AppPaths {
            settings: PathBuf::from("/c/settings.json"),
            progress: PathBuf::from("/c/progress.json"),
            tdlib_db: PathBuf::from("/d/tdlib"),
            tdlib_files: PathBuf::from("/d/tdlib-files"),
            image_cache: PathBuf::from("/d/images"),
            temp: PathBuf::from("/t/flox"),
        };
        let p = td_params(&paths, 7, "h".to_owned());
        assert_eq!((p.api_id, p.api_hash.as_str()), (7, "h"));
        assert_eq!(p.db_dir, PathBuf::from("/d/tdlib"));
        assert_eq!(p.files_dir, PathBuf::from("/d/tdlib-files"));
        assert_eq!(p.app_version, flox_core::VERSION);
        assert!(!p.device_model.is_empty());
    }

    #[test]
    fn only_flox_folders_are_swept() {
        let sys = Path::new("/var/tmp");
        assert!(is_flox_temp(Path::new("/var/tmp/flox"), sys));
        assert!(is_flox_temp(Path::new("/home/me/.flox/temp"), sys));
        assert!(!is_flox_temp(Path::new("/var/tmp"), sys), "the temp folder");
        assert!(!is_flox_temp(Path::new("/var"), sys), "an ancestor");
        assert!(!is_flox_temp(Path::new("/"), sys));
        assert!(!is_flox_temp(Path::new("/var/tmp/other"), sys));
        #[cfg(windows)]
        {
            let win = Path::new(r"C:\Users\me\AppData\Local\Temp");
            assert!(is_flox_temp(
                Path::new(r"C:\Users\me\AppData\Local\Temp\flox"),
                win
            ));
        }
    }

    #[test]
    fn sweep_empties_the_flox_temp_folder_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flox");
        let job = root.join("0f0e0d0c-0000-0000-0000-000000000000");
        std::fs::create_dir_all(job.join("parts")).unwrap();
        std::fs::write(job.join("media.mp4"), b"x").unwrap();
        std::fs::create_dir_all(root.join("webview2").join("sniffer")).unwrap();
        let neighbour = dir.path().join("keep.txt");
        std::fs::write(&neighbour, b"y").unwrap();
        let paths = AppPaths {
            settings: dir.path().join("settings.json"),
            progress: dir.path().join("progress.json"),
            tdlib_db: dir.path().join("db"),
            tdlib_files: dir.path().join("files"),
            image_cache: dir.path().join("images"),
            temp: root.clone(),
        };
        sweep_temp(&paths);
        assert!(root.is_dir());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        assert!(neighbour.is_file());
    }
}
