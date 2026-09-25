//! The page-player fallback: a visible child WebView2 over the player area, playing the
//! VidLink page with its own UI.
//!
//! It runs in its own environment (not muted, its own user data folder) with the Playback
//! document-start script, and `flox_nav.js` is run in the main frame after every completed
//! navigation. Keys reach the page as scripts ([`Action`]); `FLOX_TICK`, `PLAYER_EVENT` and
//! `MEDIA_DATA` come back as [`PageEvent`]s.

use serde_json::Value;

use crate::bridge::BridgeMessage;
use crate::host::HostEvent;

/// A direction for the page's spatial navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn name(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }
}

/// Something to do in the page (the `window.__flox` API of `flox_nav.js` and key events).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// A Space keydown: toggles play, or activates in the page's own UI.
    Space,
    /// Space only when the page video is paused.
    Play,
    /// Space only when the page video is playing.
    Pause,
    ArrowLeft,
    ArrowRight,
    /// Seeks the page video by seconds.
    Seek(i64),
    /// Enters spatial navigation mode.
    EnterNav,
    ExitNav,
    Nav(Direction),
    /// Activates the focused page control.
    Activate,
    /// Opens the page's settings panel, entering navigation mode first.
    OpenSettings,
    /// Closes an open page panel; the script's result is whether one was open.
    ClosePanel,
    /// `__floxApplyStart(secs)`.
    ApplyStart(u32),
    /// `__floxApplySpeed(rate)`.
    ApplySpeed(f32),
}

/// What the page player reports.
#[derive(Clone, Debug, PartialEq)]
pub enum PageEvent {
    /// `FLOX_TICK`, every 2 s.
    Tick {
        current_time: f64,
        duration: f64,
        paused: bool,
        ended: bool,
    },
    /// `PLAYER_EVENT`.
    PlayerEvent(Value),
    /// `MEDIA_DATA`.
    MediaData(Value),
    /// The page finished loading.
    Ready,
    /// The page failed to load or its process died.
    Failed,
}

/// Receives [`PageEvent`]s on the host thread; it must not block.
pub type PageSink = Box<dyn Fn(PageEvent) + Send + 'static>;

/// A keydown (with the legacy `keyCode` many players still read) on the focused element.
fn key_script(key: &str, code: &str, key_code: u32) -> String {
    format!(
        "(function () {{ var a = document.activeElement; var t = a && a !== document.body ? a : document.body; t.dispatchEvent(new KeyboardEvent(\"keydown\", {{ key: \"{key}\", code: \"{code}\", keyCode: {key_code}, which: {key_code}, bubbles: true, cancelable: true }})) }})()"
    )
}

/// Runs `body` only when the page video exists and `v.paused` equals `paused`.
fn when_paused(paused: bool, body: &str) -> String {
    let test = if paused { "v.paused" } else { "!v.paused" };
    format!(
        "(function () {{ var v = document.querySelector(\"video\"); if (v && {test}) {body} }})()"
    )
}

/// Calls `window.__flox.<call>` when `flox_nav.js` is installed.
fn flox_call(call: &str) -> String {
    format!("(function (f) {{ return f ? f.{call} : false }})(window.__flox)")
}

/// A JavaScript number for a playback rate; 1 when the value is not a usable rate.
fn rate_literal(rate: f32) -> String {
    if rate.is_finite() && rate > 0.0 {
        format!("{rate}")
    } else {
        "1".to_owned()
    }
}

/// The script for an action.
pub fn action_script(action: &Action) -> String {
    let space = key_script(" ", "Space", 32);
    match action {
        Action::Space => space,
        Action::Play => when_paused(true, &space),
        Action::Pause => when_paused(false, &space),
        Action::ArrowLeft => key_script("ArrowLeft", "ArrowLeft", 37),
        Action::ArrowRight => key_script("ArrowRight", "ArrowRight", 39),
        Action::Seek(secs) => flox_call(&format!("seek({secs})")),
        Action::EnterNav => flox_call("enter()"),
        Action::ExitNav => flox_call("exit()"),
        Action::Nav(dir) => flox_call(&format!("nav(\"{}\")", dir.name())),
        Action::Activate => flox_call("activate()"),
        Action::OpenSettings => "(function (f) { if (!f) return false; if (!f.isActive()) f.enter(); return f.clickLabel(\"setting\") })(window.__flox)".to_owned(),
        Action::ClosePanel => flox_call("closePanel()"),
        Action::ApplyStart(secs) => {
            format!("window.__floxApplyStart && window.__floxApplyStart({secs})")
        }
        Action::ApplySpeed(rate) => format!(
            "window.__floxApplySpeed && window.__floxApplySpeed({})",
            rate_literal(*rate)
        ),
    }
}

/// The page-player view of a host event; `about:` loads (parking the view) are dropped.
pub fn page_event(event: HostEvent) -> Option<PageEvent> {
    match event {
        HostEvent::Message(BridgeMessage::Tick {
            current_time,
            duration,
            paused,
            ended,
        }) => Some(PageEvent::Tick {
            current_time,
            duration,
            paused,
            ended,
        }),
        HostEvent::Message(BridgeMessage::PlayerEvent(v)) => Some(PageEvent::PlayerEvent(v)),
        HostEvent::Message(BridgeMessage::MediaData(v)) => Some(PageEvent::MediaData(v)),
        HostEvent::Message(_) => None,
        HostEvent::Loaded { url, .. } if url.starts_with("about:") => None,
        // a navigation replaced by another one (or cancelled by the policy) is not a failure
        HostEvent::Loaded {
            cancelled: true, ..
        } => None,
        HostEvent::Loaded { success, .. } => Some(if success {
            PageEvent::Ready
        } else {
            PageEvent::Failed
        }),
        HostEvent::ProcessFailed { .. } => Some(PageEvent::Failed),
    }
}

#[cfg(windows)]
pub use imp::PagePlayer;

#[cfg(windows)]
mod imp {
    use flox_core::error::Result;
    use flox_core::sniff::SniffMode;

    use super::{action_script, page_event, Action, PageSink};
    use crate::assets::{document_start_script, nav_script, ScriptOptions};
    use crate::host::{default_user_data, HostConfig, Rect, Surface, WebHost};

    /// The page player. Dropping it closes the WebView.
    pub struct PagePlayer {
        host: WebHost,
    }

    impl PagePlayer {
        /// Creates the hidden child WebView over `parent` (an `HWND` as an integer) at `rect`
        /// (physical pixels, relative to the parent's client area).
        pub async fn open(
            parent: isize,
            rect: Rect,
            opts: ScriptOptions,
            on_event: PageSink,
        ) -> Result<PagePlayer> {
            let config = HostConfig {
                surface: Surface::Child { parent, rect },
                muted: false,
                user_data: default_user_data("page"),
                after_load_script: Some(nav_script().to_owned()),
            };
            let host = WebHost::start(
                config,
                Box::new(move |event| {
                    if let Some(event) = page_event(event) {
                        on_event(event);
                    }
                }),
            )
            .await?;
            host.set_document_script(document_start_script(SniffMode::Playback, &opts))?;
            Ok(PagePlayer { host })
        }

        /// Loads the page and shows the view.
        pub fn load(&self, url: &str) -> Result<()> {
            self.host.navigate(url)?;
            self.host.show()
        }

        /// Hides the view and parks it on `about:blank` (stops playback and sound).
        pub fn unload(&self) -> Result<()> {
            self.host.hide()?;
            self.host.navigate("about:blank")
        }

        /// Runs an action without waiting for it.
        pub fn action(&self, action: &Action) -> Result<()> {
            self.host.execute_script(action_script(action))
        }

        /// Closes an open page panel; true when one was open.
        pub async fn close_panel(&self) -> Result<bool> {
            let json = self
                .host
                .evaluate(action_script(&Action::ClosePanel))
                .await?;
            Ok(json.trim() == "true")
        }

        /// Runs any script in the main frame and returns its JSON result.
        pub async fn evaluate(&self, script: String) -> Result<String> {
            self.host.evaluate(script).await
        }

        pub fn show(&self) -> Result<()> {
            self.host.show()
        }

        pub fn hide(&self) -> Result<()> {
            self.host.hide()
        }

        pub fn resize(&self, rect: Rect) -> Result<()> {
            self.host.resize(rect)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_scripts() {
        let space = action_script(&Action::Space);
        assert!(space.contains("new KeyboardEvent(\"keydown\""));
        assert!(space.contains("key: \" \", code: \"Space\", keyCode: 32"));
        let play = action_script(&Action::Play);
        assert!(play.contains("if (v && v.paused)"));
        assert!(play.contains("code: \"Space\""));
        let pause = action_script(&Action::Pause);
        assert!(pause.contains("if (v && !v.paused)"));
        assert!(action_script(&Action::ArrowLeft).contains("keyCode: 37"));
        assert!(action_script(&Action::ArrowRight).contains("keyCode: 39"));
    }

    #[test]
    fn nav_api_scripts() {
        assert_eq!(
            action_script(&Action::Seek(-10)),
            "(function (f) { return f ? f.seek(-10) : false })(window.__flox)"
        );
        assert!(action_script(&Action::Nav(Direction::Up)).contains("f.nav(\"up\")"));
        assert!(action_script(&Action::Nav(Direction::Left)).contains("f.nav(\"left\")"));
        assert!(action_script(&Action::EnterNav).contains("f.enter()"));
        assert!(action_script(&Action::ExitNav).contains("f.exit()"));
        assert!(action_script(&Action::Activate).contains("f.activate()"));
        assert!(action_script(&Action::ClosePanel).contains("f.closePanel()"));
        let settings = action_script(&Action::OpenSettings);
        assert!(settings.contains("if (!f.isActive()) f.enter()"));
        assert!(settings.contains("f.clickLabel(\"setting\")"));
    }

    #[test]
    fn start_and_speed() {
        assert_eq!(
            action_script(&Action::ApplyStart(95)),
            "window.__floxApplyStart && window.__floxApplyStart(95)"
        );
        assert_eq!(
            action_script(&Action::ApplySpeed(1.25)),
            "window.__floxApplySpeed && window.__floxApplySpeed(1.25)"
        );
        assert!(action_script(&Action::ApplySpeed(1.0)).ends_with("(1)"));
        assert!(action_script(&Action::ApplySpeed(f32::NAN)).ends_with("(1)"));
        assert!(action_script(&Action::ApplySpeed(0.0)).ends_with("(1)"));
    }

    #[test]
    fn events() {
        let tick = BridgeMessage::Tick {
            current_time: 12.0,
            duration: 100.0,
            paused: false,
            ended: false,
        };
        assert_eq!(
            page_event(HostEvent::Message(tick)),
            Some(PageEvent::Tick {
                current_time: 12.0,
                duration: 100.0,
                paused: false,
                ended: false
            })
        );
        let ev = json!({ "event": "play" });
        assert_eq!(
            page_event(HostEvent::Message(BridgeMessage::PlayerEvent(ev.clone()))),
            Some(PageEvent::PlayerEvent(ev))
        );
        assert_eq!(
            page_event(HostEvent::Message(BridgeMessage::Stream {
                captions: vec![]
            })),
            None
        );
        let loaded = |url: &str, success: bool, cancelled: bool| HostEvent::Loaded {
            url: url.to_owned(),
            success,
            cancelled,
        };
        assert_eq!(
            page_event(loaded("https://vidlink.pro/movie/1", true, false)),
            Some(PageEvent::Ready)
        );
        assert_eq!(
            page_event(loaded("https://vidlink.pro/movie/1", false, false)),
            Some(PageEvent::Failed)
        );
        assert_eq!(
            page_event(loaded("https://vidlink.pro/movie/1", false, true)),
            None
        );
        assert_eq!(page_event(loaded("about:blank", true, false)), None);
        assert_eq!(
            page_event(HostEvent::ProcessFailed { browser: false }),
            Some(PageEvent::Failed)
        );
    }

    /// `node --check` on every action script, when node is installed.
    #[test]
    fn action_scripts_parse_with_node() {
        let has_node = std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !has_node {
            eprintln!("node not found; skipping syntax check");
            return;
        }
        let actions = [
            Action::Space,
            Action::Play,
            Action::Pause,
            Action::ArrowLeft,
            Action::ArrowRight,
            Action::Seek(10),
            Action::EnterNav,
            Action::ExitNav,
            Action::Nav(Direction::Down),
            Action::Activate,
            Action::OpenSettings,
            Action::ClosePanel,
            Action::ApplyStart(30),
            Action::ApplySpeed(1.5),
        ];
        let body: String = actions
            .iter()
            .map(|a| format!("{}\n;\n", action_script(a)))
            .collect();
        let path = std::env::temp_dir().join(format!("flox-web-page-{}.js", std::process::id()));
        std::fs::write(&path, body).unwrap();
        let out = std::process::Command::new("node")
            .arg("--check")
            .arg(&path)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
