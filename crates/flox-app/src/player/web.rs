//! The page-player surface behind the engine's page hooks.
//!
//! On Windows it drives `flox_web::page::PagePlayer` (a visible child WebView2 over the player
//! area) from a tokio task, opening it on the first load and resizing it with the window.
//! `FLOX_TICK` comes back as [`Input::Playback`] (progress and end detection), the page load as
//! [`Input::PageReady`] or [`Input::PageFailed`]. Everywhere else, and on Windows when no window
//! handle or runtime is available, a load fails at once with `PageFailed` so the controller
//! moves on to the failed state instead of waiting for its watchdog.

use flox_web::page::{Action, Direction as PageDirection, PageEvent};

use super::controller::{Direction, Input, PageAction, Playback};

/// The flox-web action for a controller page action.
pub fn page_action(action: &PageAction) -> Action {
    let dir = |d: Direction| match d {
        Direction::Left => PageDirection::Left,
        Direction::Right => PageDirection::Right,
        Direction::Up => PageDirection::Up,
        Direction::Down => PageDirection::Down,
    };
    match action {
        PageAction::Space => Action::Space,
        PageAction::Play => Action::Play,
        PageAction::Pause => Action::Pause,
        PageAction::ArrowLeft => Action::ArrowLeft,
        PageAction::ArrowRight => Action::ArrowRight,
        PageAction::Seek(secs) => Action::Seek(*secs),
        PageAction::EnterNav => Action::EnterNav,
        PageAction::ExitNav => Action::ExitNav,
        PageAction::Nav(d) => Action::Nav(dir(*d)),
        PageAction::Activate => Action::Activate,
        PageAction::OpenSettings => Action::OpenSettings,
        PageAction::ClosePanel => Action::ClosePanel,
        PageAction::ApplyStart(secs) => Action::ApplyStart(*secs),
        PageAction::ApplySpeed(rate) => Action::ApplySpeed(*rate),
    }
}

/// The controller input for a page event, if it has one.
pub fn page_input(event: PageEvent) -> Option<Input> {
    match event {
        PageEvent::Tick {
            current_time,
            duration,
            paused,
            ended,
        } => Some(Input::Playback(Playback {
            time: current_time,
            duration,
            paused,
            ended,
        })),
        PageEvent::Ready => Some(Input::PageReady),
        PageEvent::Failed => Some(Input::PageFailed),
        PageEvent::PlayerEvent(_) | PageEvent::MediaData(_) => None,
    }
}

/// The page player as the engine sees it.
pub struct PageSurface {
    pending: Vec<Input>,
    #[cfg(windows)]
    host: Option<imp::Host>,
}

impl PageSurface {
    /// No page player: every load fails.
    pub fn unavailable() -> Self {
        Self {
            pending: Vec::new(),
            #[cfg(windows)]
            host: None,
        }
    }

    /// The WebView2 page player as a child of `hwnd`, sized by `size` (physical pixels) at
    /// each load.
    #[cfg(windows)]
    pub fn windows(
        runtime: tokio::runtime::Handle,
        hwnd: isize,
        size: Box<dyn Fn() -> (i32, i32)>,
    ) -> Self {
        Self {
            pending: Vec::new(),
            host: Some(imp::Host::start(runtime, hwnd, size)),
        }
    }

    pub fn load(&mut self, url: &str) {
        #[cfg(windows)]
        if let Some(host) = &self.host {
            host.load(url);
            return;
        }
        tracing::info!("no page player on this platform: {url}");
        self.pending.push(Input::PageFailed);
    }

    pub fn action(&mut self, action: PageAction) {
        #[cfg(windows)]
        if let Some(host) = &self.host {
            host.action(action);
            return;
        }
        if action == PageAction::ClosePanel {
            self.pending.push(Input::PagePanelClosed(false));
        }
    }

    pub fn close(&mut self) {
        #[cfg(windows)]
        if let Some(host) = &self.host {
            host.unload();
        }
    }

    /// Fits the page view to the player area again (the window was resized). A no-op until
    /// the first load opens the view, and without a page player.
    pub fn resize(&mut self) {
        #[cfg(windows)]
        if let Some(host) = &self.host {
            host.resize();
        }
    }

    /// Inputs produced since the last poll.
    pub fn poll(&mut self) -> Vec<Input> {
        #[cfg(windows)]
        if let Some(host) = &mut self.host {
            host.drain(&mut self.pending);
        }
        std::mem::take(&mut self.pending)
    }
}

#[cfg(windows)]
mod imp {
    use flox_web::assets::ScriptOptions;
    use flox_web::host::Rect;
    use flox_web::page::{Action, PagePlayer};
    use tokio::sync::mpsc;

    use super::{page_action, page_input};
    use crate::player::controller::{Input, PageAction};

    enum Command {
        Load(String, Rect),
        Action(Action),
        ClosePanel,
        Resize(Rect),
        Unload,
    }

    /// The UI-thread side: commands go to the task, inputs come back.
    pub(super) struct Host {
        tx: mpsc::UnboundedSender<Command>,
        rx: mpsc::UnboundedReceiver<Input>,
        size: Box<dyn Fn() -> (i32, i32)>,
    }

    impl Host {
        pub(super) fn start(
            runtime: tokio::runtime::Handle,
            hwnd: isize,
            size: Box<dyn Fn() -> (i32, i32)>,
        ) -> Host {
            let (tx, commands) = mpsc::unbounded_channel();
            let (inputs, rx) = mpsc::unbounded_channel();
            runtime.spawn(run(hwnd, commands, inputs));
            Host { tx, rx, size }
        }

        fn send(&self, command: Command) {
            if self.tx.send(command).is_err() {
                tracing::warn!("the page player task is gone");
            }
        }

        pub(super) fn load(&self, url: &str) {
            let (w, h) = (self.size)();
            self.send(Command::Load(url.to_owned(), Rect::new(0, 0, w, h)));
        }

        pub(super) fn action(&self, action: PageAction) {
            if action == PageAction::ClosePanel {
                self.send(Command::ClosePanel);
            } else {
                self.send(Command::Action(page_action(&action)));
            }
        }

        pub(super) fn unload(&self) {
            self.send(Command::Unload);
        }

        pub(super) fn resize(&self) {
            let (w, h) = (self.size)();
            self.send(Command::Resize(Rect::new(0, 0, w, h)));
        }

        pub(super) fn drain(&mut self, out: &mut Vec<Input>) {
            while let Ok(input) = self.rx.try_recv() {
                out.push(input);
            }
        }
    }

    async fn run(
        hwnd: isize,
        mut commands: mpsc::UnboundedReceiver<Command>,
        inputs: mpsc::UnboundedSender<Input>,
    ) {
        let mut player: Option<PagePlayer> = None;
        while let Some(command) = commands.recv().await {
            match command {
                Command::Load(url, rect) => {
                    if player.is_none() {
                        let events = inputs.clone();
                        let sink = Box::new(move |event| {
                            if let Some(input) = page_input(event) {
                                let _ = events.send(input);
                            }
                        });
                        match PagePlayer::open(hwnd, rect, ScriptOptions::default(), sink).await {
                            Ok(p) => player = Some(p),
                            Err(e) => {
                                tracing::warn!("page player: {e}");
                                let _ = inputs.send(Input::PageFailed);
                                continue;
                            }
                        }
                    } else if let Some(p) = &player {
                        if let Err(e) = p.resize(rect) {
                            tracing::debug!("page player resize: {e}");
                        }
                    }
                    if let Some(p) = &player {
                        if let Err(e) = p.load(&url) {
                            tracing::warn!("page player load: {e}");
                            let _ = inputs.send(Input::PageFailed);
                        }
                    }
                }
                Command::Action(action) => {
                    if let Some(p) = &player {
                        if let Err(e) = p.action(&action) {
                            tracing::debug!("page action: {e}");
                        }
                    }
                }
                Command::ClosePanel => {
                    let closed = match &player {
                        Some(p) => p.close_panel().await.unwrap_or(false),
                        None => false,
                    };
                    let _ = inputs.send(Input::PagePanelClosed(closed));
                }
                Command::Resize(rect) => {
                    if let Some(p) = &player {
                        if let Err(e) = p.resize(rect) {
                            tracing::debug!("page player resize: {e}");
                        }
                    }
                }
                Command::Unload => {
                    if let Some(p) = &player {
                        if let Err(e) = p.unload() {
                            tracing::debug!("page player unload: {e}");
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn actions_map_one_to_one() {
        assert_eq!(page_action(&PageAction::Space), Action::Space);
        assert_eq!(page_action(&PageAction::Seek(-30)), Action::Seek(-30));
        assert_eq!(
            page_action(&PageAction::Nav(Direction::Up)),
            Action::Nav(PageDirection::Up)
        );
        assert_eq!(
            page_action(&PageAction::ApplySpeed(1.5)),
            Action::ApplySpeed(1.5)
        );
        assert_eq!(
            page_action(&PageAction::ApplyStart(90)),
            Action::ApplyStart(90)
        );
    }

    #[test]
    fn page_events_become_inputs() {
        assert_eq!(
            page_input(PageEvent::Tick {
                current_time: 5.0,
                duration: 60.0,
                paused: false,
                ended: false
            }),
            Some(Input::Playback(Playback {
                time: 5.0,
                duration: 60.0,
                paused: false,
                ended: false
            }))
        );
        assert_eq!(page_input(PageEvent::Ready), Some(Input::PageReady));
        assert_eq!(page_input(PageEvent::Failed), Some(Input::PageFailed));
        assert_eq!(page_input(PageEvent::MediaData(json!({}))), None);
    }

    #[test]
    fn without_a_page_player_loads_fail_and_panels_stay_closed() {
        let mut s = PageSurface::unavailable();
        s.load("https://vidlink.pro/movie/1");
        s.action(PageAction::ClosePanel);
        s.action(PageAction::Space);
        s.resize();
        s.close();
        assert_eq!(
            s.poll(),
            vec![Input::PageFailed, Input::PagePanelClosed(false)]
        );
        assert!(s.poll().is_empty());
    }
}
