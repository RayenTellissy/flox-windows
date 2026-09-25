//! The application context shared by every view model. Wired up by piece P16a.

use std::sync::Arc;

use flox_core::progress::ProgressStore;
use flox_core::settings::SettingsStore;
use flox_player::ffi::MpvLib;
use flox_rip::queue::Queue;
use flox_td::client::TdClient;
use flox_td::library::Library;

/// Long-lived services, created once in `main`.
pub struct AppContext {
    pub runtime: tokio::runtime::Runtime,
    pub settings: SettingsStore,
    pub progress: Arc<ProgressStore>,
    /// Present when API credentials are set and tdjson resolves.
    pub td: Option<Arc<TdClient>>,
    pub library: Option<Arc<Library>>,
    pub queue: Option<Arc<Queue>>,
    /// Present when libmpv resolves.
    pub player_lib: Option<Arc<MpvLib>>,
}
