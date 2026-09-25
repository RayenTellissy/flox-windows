//! The injected page scripts. The raw files live in `assets/web/`; templating
//! (`__ALLOW__`, `__NOHEVC__`, `__MAX_HEIGHT__`) is filled in by piece P12.

use flox_core::sniff::SniffMode;

/// Bridge shim (`window.FloxBridge`), extracted from the Mac sniffer.
pub const SHIM_JS: &str = include_str!("../../../assets/web/shim.js");
/// Ad-block and manifest reporter, from Android, with `MAX_HEIGHT` templated.
pub const ADBLOCK_JS: &str = include_str!("../../../assets/web/adblock.js");
/// Rip-mode source JSON tap, extracted from the Mac sniffer.
pub const TAP_JS: &str = include_str!("../../../assets/web/tap.js");
/// Page-player keyboard navigation, from Android.
pub const NAV_JS: &str = include_str!("../../../assets/web/flox_nav.js");

/// Default height cap for page variants.
pub const DEFAULT_MAX_HEIGHT: u32 = 2160;

/// Values templated into `adblock.js`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptOptions {
    /// `__NOHEVC__`: hide HEVC variants.
    pub no_hevc: bool,
    /// `__MAX_HEIGHT__`: drop variants taller than this.
    pub max_height: u32,
}

impl Default for ScriptOptions {
    fn default() -> Self {
        Self {
            no_hevc: false,
            max_height: DEFAULT_MAX_HEIGHT,
        }
    }
}

/// shim + templated adblock (+ tap in Rip mode), for `AddScriptToExecuteOnDocumentCreated`.
/// Filled in by P12.
#[allow(clippy::unimplemented)]
pub fn document_start_script(_mode: SniffMode, _opts: &ScriptOptions) -> String {
    unimplemented!("flox_web::assets::document_start_script (P12)")
}

/// `flox_nav.js`, injected after load in the page player.
pub fn nav_script() -> &'static str {
    NAV_JS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_embedded() {
        assert!(ADBLOCK_JS.contains("var MAX_HEIGHT = __MAX_HEIGHT__"));
        assert!(ADBLOCK_JS.contains("__ALLOW__"));
        assert!(SHIM_JS.contains("FloxBridge"));
        assert!(TAP_JS.contains("FLOX_PLAYLIST"));
        assert!(!nav_script().is_empty());
        assert_eq!(ScriptOptions::default().max_height, 2160);
    }
}
