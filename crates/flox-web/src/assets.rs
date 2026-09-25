//! The injected page scripts. The raw files live in `assets/web/`; `adblock.js`
//! is templated (`__ALLOW__`, `__NOHEVC__`, `__MAX_HEIGHT__`) before injection.

use flox_core::sniff::SniffMode;

use crate::policy::{CDN_HOSTS, PLAYER_HOSTS};

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

/// The page-side allowlist: player hosts then CDN hosts, deduplicated, as a JSON array.
fn allow_json() -> String {
    let mut hosts: Vec<&str> = Vec::with_capacity(PLAYER_HOSTS.len() + CDN_HOSTS.len());
    for h in PLAYER_HOSTS.iter().chain(CDN_HOSTS) {
        if !hosts.contains(h) {
            hosts.push(h);
        }
    }
    serde_json::Value::from(hosts).to_string()
}

/// `adblock.js` with its placeholders filled in.
fn adblock_script(opts: &ScriptOptions) -> String {
    ADBLOCK_JS
        .replace("__ALLOW__", &allow_json())
        .replace("__NOHEVC__", if opts.no_hevc { "true" } else { "false" })
        .replace("__MAX_HEIGHT__", &opts.max_height.to_string())
}

/// shim + templated adblock (+ tap in Rip mode), for `AddScriptToExecuteOnDocumentCreated`.
pub fn document_start_script(mode: SniffMode, opts: &ScriptOptions) -> String {
    let mut parts = vec![SHIM_JS.to_owned(), adblock_script(opts)];
    if mode == SniffMode::Rip {
        parts.push(TAP_JS.to_owned());
    }
    // each part is a complete statement list; the separator keeps ASI from joining them
    parts.join("\n;\n")
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

    /// `__NAME__` placeholders (upper case), ignoring page identifiers like `__floxUrl`.
    fn placeholders(s: &str) -> Vec<String> {
        let b = s.as_bytes();
        let mut found = Vec::new();
        let mut i = 0;
        while i + 1 < b.len() {
            if b[i] == b'_' && b[i + 1] == b'_' {
                let start = i + 2;
                let mut j = start;
                while j < b.len() && (b[j].is_ascii_uppercase() || b[j] == b'_') {
                    j += 1;
                }
                let name = &s[start..j];
                if name.len() > 2 && name.ends_with("__") {
                    found.push(s[i..j].to_owned());
                }
                i = j.max(i + 1);
            } else {
                i += 1;
            }
        }
        found
    }

    #[test]
    fn placeholder_scan_finds_raw_names() {
        assert_eq!(
            placeholders(ADBLOCK_JS),
            vec!["__ALLOW__", "__ALLOW__", "__NOHEVC__", "__MAX_HEIGHT__"]
        );
    }

    #[test]
    fn templated_script_has_no_placeholders() {
        for mode in [SniffMode::Playback, SniffMode::Rip] {
            let s = document_start_script(mode, &ScriptOptions::default());
            assert!(placeholders(&s).is_empty(), "{:?}", placeholders(&s));
            assert!(!s.contains("__ALLOW__"));
            assert!(!s.contains("__NOHEVC__"));
            assert!(!s.contains("__MAX_HEIGHT__"));
        }
    }

    #[test]
    fn template_values() {
        let s = document_start_script(
            SniffMode::Playback,
            &ScriptOptions {
                no_hevc: false,
                max_height: 1080,
            },
        );
        assert!(s.contains("var MAX_HEIGHT = 1080"));
        assert!(s.contains("if (false) {"));
        let allow =
            "var ALLOW = [\"vidlink.pro\",\"jwplayer.com\",\"jwpcdn.com\",\"image.tmdb.org\",";
        assert!(s.contains(allow), "allowlist not templated");
        let line = s
            .lines()
            .find(|l| l.contains("var ALLOW = "))
            .unwrap()
            .trim()
            .trim_start_matches("var ALLOW = ");
        let hosts: Vec<String> = serde_json::from_str(line).unwrap();
        assert_eq!(hosts.len(), PLAYER_HOSTS.len() + CDN_HOSTS.len() - 1);
        assert_eq!(hosts.iter().filter(|h| *h == "jwpcdn.com").count(), 1);
        for h in PLAYER_HOSTS.iter().chain(CDN_HOSTS) {
            assert!(hosts.iter().any(|x| x == h), "{h}");
        }

        let hevc = document_start_script(
            SniffMode::Playback,
            &ScriptOptions {
                no_hevc: true,
                max_height: 2160,
            },
        );
        assert!(hevc.contains("if (true) {"));
        assert!(hevc.contains("var MAX_HEIGHT = 2160"));
    }

    #[test]
    fn script_order_and_tap_only_in_rip() {
        let opts = ScriptOptions::default();
        let play = document_start_script(SniffMode::Playback, &opts);
        let rip = document_start_script(SniffMode::Rip, &opts);
        assert!(play.starts_with(SHIM_JS));
        assert!(rip.starts_with(SHIM_JS));
        assert!(!play.contains("FLOX_PLAYLIST"));
        let shim = rip.find("window.FloxBridge =").unwrap();
        let adblock = rip.find("__floxInstalled").unwrap();
        let tap = rip.find("FLOX_PLAYLIST").unwrap();
        assert!(shim < adblock && adblock < tap);
        assert!(rip.ends_with(TAP_JS));
    }

    #[test]
    fn bridge_posts_through_webview2() {
        for s in [SHIM_JS, TAP_JS] {
            assert!(s.contains("window.chrome.webview.postMessage("));
            assert!(!s.contains("webkit"));
        }
        assert!(SHIM_JS.contains("onMessage: function (s)"));
        assert!(TAP_JS.contains("JSON.stringify({ type: t, data: d })"));
        // adblock.js and flox_nav.js reach the host only through FloxBridge.onMessage
        for s in [ADBLOCK_JS, NAV_JS] {
            assert!(s.contains("window.FloxBridge.onMessage("));
            assert!(!s.contains("webkit"));
            assert!(!s.contains("chrome.webview"));
        }
    }

    /// `node --check` on each templated script, when node is installed.
    #[test]
    fn templated_scripts_parse_with_node() {
        let has_node = std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !has_node {
            eprintln!("node not found; skipping syntax check");
            return;
        }
        let dir = std::env::temp_dir().join(format!("flox-web-assets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let scripts = [
            (
                "playback.js",
                document_start_script(SniffMode::Playback, &ScriptOptions::default()),
            ),
            (
                "rip.js",
                document_start_script(SniffMode::Rip, &ScriptOptions::default()),
            ),
            ("nav.js", nav_script().to_owned()),
        ];
        for (name, body) in scripts {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            let out = std::process::Command::new("node")
                .arg("--check")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
