//! mpv options derived from settings (plan section 6). Filled in by piece P11.

use flox_core::settings::Settings;

/// The options passed to `Mpv::new`. Filled in by P11.
#[allow(clippy::unimplemented)]
pub fn base_options(_settings: &Settings) -> Vec<(&'static str, String)> {
    unimplemented!("flox_player::options::base_options (P11)")
}
