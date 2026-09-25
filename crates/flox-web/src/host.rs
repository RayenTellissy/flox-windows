//! The WebView2 host: an STA thread with its own Win32 message loop, one window and one
//! controller, driven through a command channel.
//!
//! The sniffer uses a [`Surface::Hidden`] host: a `WS_POPUP` 1280x720 tool window that never
//! activates and is cloaked with `DWMWA_CLOAK`, while the controller keeps `IsVisible = true`
//! so the page keeps `document.visibilityState = "visible"` and runs its timers at full rate.
//! The page player uses a [`Surface::Child`] host over the player area of the app window.
//!
//! Every COM object lives on the host thread. The rest of the app talks to it through
//! [`HostCommand`]s and hears back through [`HostEvent`]s, delivered on the host thread to an
//! [`EventSink`].
//!
//! Scripts: the document-start script is registered with `AddScriptToExecuteOnDocumentCreated`.
//! WebView2 has no frame-level version of that call (webview2-com 0.38 exposes none), so as a
//! fallback each frame reported by `FrameCreated` also gets the same script through
//! `ICoreWebView2Frame2::ExecuteScript` on its `ContentLoading`, guarded by
//! `window.__floxDocStart` so a frame that already ran the document-start copy skips it.
//!
//! Minimum WebView2 runtime versions of the interfaces used (from the SDK release notes):
//! `ICoreWebView2Settings2` (user agent) 1.0.864.35, `ICoreWebView2_4` (`FrameCreated`)
//! 1.0.902.49, `ICoreWebView2Frame2` (frame messages, `ContentLoading`, `ExecuteScript`)
//! 1.0.1108.44, `ICoreWebView2Environment10` (InPrivate controller options) 1.0.1185.39,
//! `ICoreWebView2_22` (resource filter with request source kinds) 1.0.2478.35,
//! `ICoreWebView2Frame7` (nested `FrameCreated`) 1.0.2651.64. Each is optional: on an older
//! runtime the host skips that feature and logs it.

use std::path::PathBuf;

use tokio::sync::oneshot;

use crate::bridge::BridgeMessage;

/// Width of the hidden sniffer window.
pub const HIDDEN_WIDTH: i32 = 1280;
/// Height of the hidden sniffer window.
pub const HIDDEN_HEIGHT: i32 = 720;

/// The user agent every host sends (desktop Chrome on Windows).
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

/// Chromium switches shared by both environments.
const BROWSER_ARGUMENTS: &str = "--autoplay-policy=no-user-gesture-required --disable-features=CalculateNativeWinOcclusion,msWebOOUI,msPdfOOUI";

/// Set by the document-start copy of the script; the per-frame fallback skips when it is there.
const DOC_START_MARKER: &str = "__floxDocStart";

/// A rectangle in physical pixels. For a child host it is relative to the parent's client area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The same size at the origin, clamped to at least 1x1 (WebView2 rejects empty bounds
    /// on some runtimes and stops rendering).
    pub fn local(&self) -> Rect {
        Rect::new(0, 0, self.width.max(1), self.height.max(1))
    }
}

/// Where the WebView is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// A cloaked 1280x720 tool window the page believes is visible (the sniffer).
    Hidden,
    /// A child window of `parent` (an `HWND` as an integer) at `rect` (the page player).
    /// It starts hidden; send [`HostCommand::Show`].
    Child { parent: isize, rect: Rect },
}

/// How a host is set up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConfig {
    pub surface: Surface,
    /// Adds `--mute-audio` (the sniffer); the page player is not muted.
    pub muted: bool,
    /// The WebView2 user data folder. Each environment needs its own: WebView2 refuses a
    /// second environment on the same folder with different browser arguments.
    pub user_data: PathBuf,
    /// Run in the main frame after every successful navigation (the page player's
    /// `flox_nav.js`), except on `about:` pages.
    pub after_load_script: Option<String>,
}

/// A request for the host thread.
#[derive(Debug)]
pub enum HostCommand {
    /// Navigates the main frame.
    Navigate(String),
    /// Replaces the document-start script (unchanged scripts are not re-registered). Later
    /// commands wait until the registration completes, so a following `Navigate` loads with it.
    SetDocumentScript(String),
    /// Runs a script in the main frame; `reply` receives its JSON result, or `None` on failure.
    ExecuteScript {
        script: String,
        reply: Option<oneshot::Sender<Option<String>>>,
    },
    /// Moves and resizes the window (a hidden host keeps its position).
    Resize(Rect),
    /// Shows a child host; uncloaks a hidden one (for debugging).
    Show,
    /// Hides a child host; cloaks a hidden one again.
    Hide,
    /// Closes the controller and the window and ends the thread.
    Close,
}

impl HostCommand {
    /// Whether the command ends the host.
    pub fn is_close(&self) -> bool {
        matches!(self, HostCommand::Close)
    }

    /// Whether later commands must wait for this one to complete on the host thread.
    pub fn blocks_queue(&self) -> bool {
        matches!(self, HostCommand::SetDocumentScript(_))
    }
}

/// What the host reports.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    /// A page message from any frame, already parsed.
    Message(BridgeMessage),
    /// The main frame finished a navigation. `cancelled` is set when it was replaced by
    /// another navigation or stopped by the navigation policy (then `success` is false too).
    Loaded {
        url: String,
        success: bool,
        cancelled: bool,
    },
    /// A WebView2 process died; the host is unusable when it was the browser process.
    ProcessFailed { browser: bool },
}

/// Receives [`HostEvent`]s on the host thread; it must not block.
pub type EventSink = Box<dyn Fn(HostEvent) + Send + 'static>;

/// `AdditionalBrowserArguments` for an environment.
pub fn browser_arguments(muted: bool) -> String {
    if muted {
        format!("{BROWSER_ARGUMENTS} --mute-audio")
    } else {
        BROWSER_ARGUMENTS.to_owned()
    }
}

/// `%TEMP%\flox\webview2\<profile>`, swept with the rest of `%TEMP%\flox` at launch.
pub fn default_user_data(profile: &str) -> PathBuf {
    std::env::temp_dir()
        .join("flox")
        .join("webview2")
        .join(profile)
}

/// The script as registered for document start: it marks the window before running.
pub fn document_script(script: &str) -> String {
    format!("window.{DOC_START_MARKER} = true\n;\n{script}")
}

/// The script as injected into a frame on `ContentLoading`: skipped when the document-start
/// copy already ran there. The page scripts are IIFEs, so wrapping them changes no scope.
pub fn frame_fallback_script(script: &str) -> String {
    format!(
        ";(function () {{\nif (window.{DOC_START_MARKER}) return\nwindow.{DOC_START_MARKER} = true\n;\n{script}\n}})()"
    )
}

/// Whether a request goes through the ad-block verdict. Local schemes never reach the network
/// and the verdict would block them as non-http.
pub fn intercepts(url: &str) -> bool {
    let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
    !matches!(scheme.as_str(), "data" | "blob" | "about" | "javascript")
}

#[cfg(windows)]
pub use imp::WebHost;

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::rc::Rc;
    use std::sync::mpsc;

    use flox_core::error::{Error, Result};
    use tokio::sync::oneshot;
    use tracing::{debug, warn};
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2, ICoreWebView2Controller,
        ICoreWebView2Environment, ICoreWebView2Environment10, ICoreWebView2EnvironmentOptions,
        ICoreWebView2ExecuteScriptCompletedHandler, ICoreWebView2Frame, ICoreWebView2Frame2,
        ICoreWebView2Frame7, ICoreWebView2NavigationStartingEventArgs, ICoreWebView2Settings2,
        ICoreWebView2WebMessageReceivedEventArgs, ICoreWebView2WebResourceRequest,
        ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2_22, ICoreWebView2_4,
        COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
        COREWEBVIEW2_WEB_ERROR_STATUS, COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_EVENT_SOURCE,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MANIFEST,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_TEXT_TRACK,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST,
        COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
    };
    use webview2_com::{
        take_pwstr, AddScriptToExecuteOnDocumentCreatedCompletedHandler,
        CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
        CreateCoreWebView2EnvironmentCompletedHandler, ExecuteScriptCompletedHandler,
        FrameChildFrameCreatedEventHandler, FrameContentLoadingEventHandler,
        FrameCreatedEventHandler, FrameWebMessageReceivedEventHandler,
        NavigationCompletedEventHandler, NavigationStartingEventHandler,
        NewWindowRequestedEventHandler, ProcessFailedEventHandler, ScriptDialogOpeningEventHandler,
        WebMessageReceivedEventHandler, WebResourceRequestedEventHandler,
    };
    use windows::core::{w, Interface, BOOL, HSTRING, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        E_FAIL, E_POINTER, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, IStream, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostQuitMessage, PostThreadMessageW, RegisterClassExW, SetWindowPos, ShowWindow,
        TranslateMessage, HWND_TOP, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SW_HIDE, SW_SHOWNA, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WM_APP, WNDCLASSEXW, WS_CHILD,
        WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    use super::{
        browser_arguments, document_script, frame_fallback_script, intercepts, EventSink,
        HostCommand, HostConfig, HostEvent, Rect, Surface, HIDDEN_HEIGHT, HIDDEN_WIDTH, USER_AGENT,
    };
    use crate::bridge;
    use crate::policy::{self, RequestInfo, ResourceKind, Verdict};

    /// Posted to the host thread after a command is queued, to wake `GetMessageW`.
    const WM_WAKE: u32 = WM_APP + 1;

    /// The window class shared by every host window.
    const CLASS_NAME: PCWSTR = w!("FloxWebViewHost");

    /// A running host thread. Dropping it closes the host.
    pub struct WebHost {
        tx: mpsc::Sender<HostCommand>,
        thread_id: u32,
    }

    impl WebHost {
        /// Starts the host thread and waits for its environment and controller. Fails with
        /// [`Error::Unavailable`] when the WebView2 runtime is missing or refuses to start.
        pub async fn start(config: HostConfig, sink: EventSink) -> Result<WebHost> {
            let (tx, rx) = mpsc::channel();
            let (ready_tx, ready_rx) = oneshot::channel();
            std::thread::Builder::new()
                .name("flox-webview2".to_owned())
                .spawn(move || host_thread(config, sink, rx, ready_tx))?;
            let thread_id = ready_rx
                .await
                .map_err(|_| Error::Unavailable("the WebView2 host thread exited".to_owned()))??;
            Ok(WebHost { tx, thread_id })
        }

        /// Queues a command and wakes the host thread.
        pub fn send(&self, cmd: HostCommand) -> Result<()> {
            self.tx
                .send(cmd)
                .map_err(|_| Error::Unavailable("the WebView2 host is closed".to_owned()))?;
            // SAFETY: posting a message with no pointers to a thread id; a failure only means
            // the thread is gone, which the next send reports.
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_WAKE, WPARAM(0), LPARAM(0)) };
            Ok(())
        }

        pub fn navigate(&self, url: &str) -> Result<()> {
            self.send(HostCommand::Navigate(url.to_owned()))
        }

        pub fn set_document_script(&self, script: String) -> Result<()> {
            self.send(HostCommand::SetDocumentScript(script))
        }

        /// Runs a script in the main frame without waiting for it.
        pub fn execute_script(&self, script: String) -> Result<()> {
            self.send(HostCommand::ExecuteScript {
                script,
                reply: None,
            })
        }

        /// Runs a script in the main frame and returns its JSON result.
        pub async fn evaluate(&self, script: String) -> Result<String> {
            let (reply, rx) = oneshot::channel();
            self.send(HostCommand::ExecuteScript {
                script,
                reply: Some(reply),
            })?;
            rx.await
                .ok()
                .flatten()
                .ok_or_else(|| Error::Other("the page script failed".to_owned()))
        }

        pub fn resize(&self, rect: Rect) -> Result<()> {
            self.send(HostCommand::Resize(rect))
        }

        pub fn show(&self) -> Result<()> {
            self.send(HostCommand::Show)
        }

        pub fn hide(&self) -> Result<()> {
            self.send(HostCommand::Hide)
        }

        pub fn close(&self) -> Result<()> {
            self.send(HostCommand::Close)
        }
    }

    impl Drop for WebHost {
        fn drop(&mut self) {
            let _ = self.send(HostCommand::Close);
        }
    }

    fn host_thread(
        config: HostConfig,
        sink: EventSink,
        rx: mpsc::Receiver<HostCommand>,
        ready: oneshot::Sender<Result<u32>>,
    ) {
        // SAFETY: the first COM call on this new thread; balanced by CoUninitialize below.
        let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if let Err(e) = init.ok() {
            let _ = ready.send(Err(Error::Unavailable(format!("CoInitializeEx: {e}"))));
            return;
        }
        match Host::create(&config, sink, rx) {
            Ok(host) => {
                // SAFETY: no preconditions. The thread already has a message queue: creating
                // the environment pumped messages.
                let id = unsafe { GetCurrentThreadId() };
                if ready.send(Ok(id)).is_ok() {
                    host.run();
                } else {
                    host.shutdown();
                }
            }
            Err(e) => {
                let _ = ready.send(Err(Error::Unavailable(format!("WebView2: {e}"))));
            }
        }
        // SAFETY: balances the successful CoInitializeEx; every COM object was dropped above.
        unsafe { CoUninitialize() };
    }

    /// State the event handlers share; only touched on the host thread.
    struct Shared {
        env: ICoreWebView2Environment,
        sink: EventSink,
        /// The per-frame fallback copy of the current document-start script.
        frame_script: RefCell<String>,
        /// The main-frame URL being loaded, to tell main-frame document requests apart.
        main_url: RefCell<String>,
        after_load: Option<String>,
    }

    impl Shared {
        fn emit(&self, event: HostEvent) {
            (self.sink)(event);
        }
    }

    /// The registered document-start script.
    #[derive(Default)]
    struct ScriptSlot {
        /// The script as given to `SetDocumentScript` (before wrapping).
        source: String,
        id: Option<String>,
        pending: bool,
    }

    struct Host {
        hwnd: HWND,
        hidden: bool,
        controller: ICoreWebView2Controller,
        webview: ICoreWebView2,
        shared: Rc<Shared>,
        script: Rc<RefCell<ScriptSlot>>,
        rx: mpsc::Receiver<HostCommand>,
        closed: bool,
    }

    impl Host {
        fn create(
            config: &HostConfig,
            sink: EventSink,
            rx: mpsc::Receiver<HostCommand>,
        ) -> windows::core::Result<Host> {
            let _ = std::fs::create_dir_all(&config.user_data);
            let hidden = matches!(config.surface, Surface::Hidden);
            let (hwnd, rect) = create_window(config.surface)?;
            let built = (|| {
                let env = create_environment(config)?;
                let controller = create_controller(&env, hwnd)?;
                // SAFETY: plain COM calls on live objects created on this thread.
                let webview = unsafe {
                    controller.SetBounds(to_rect(rect.local()))?;
                    // the hidden host stays "visible" so the page is not throttled
                    controller.SetIsVisible(hidden)?;
                    controller.CoreWebView2()?
                };
                Ok::<_, windows::core::Error>((env, controller, webview))
            })();
            let (env, controller, webview) = match built {
                Ok(v) => v,
                Err(e) => {
                    // SAFETY: the window was created on this thread and is not used again.
                    let _ = unsafe { DestroyWindow(hwnd) };
                    return Err(e);
                }
            };
            let shared = Rc::new(Shared {
                env,
                sink,
                frame_script: RefCell::new(String::new()),
                main_url: RefCell::new(String::new()),
                after_load: config.after_load_script.clone(),
            });
            let host = Host {
                hwnd,
                hidden,
                controller,
                webview,
                shared,
                script: Rc::default(),
                rx,
                closed: false,
            };
            if let Err(e) = host.configure() {
                host.shutdown();
                return Err(e);
            }
            Ok(host)
        }

        /// Settings and event handlers.
        fn configure(&self) -> windows::core::Result<()> {
            let wv = &self.webview;

            // alert/confirm/prompt: no handler action cancels the dialog
            let dialogs = ScriptDialogOpeningEventHandler::create(Box::new(|_, _| Ok(())));
            // popups never open
            let popups = NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    // SAFETY: live event args during the callback.
                    let _ = unsafe { args.SetHandled(true) };
                }
                Ok(())
            }));
            let s = self.shared.clone();
            let resources = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    if let Err(e) = on_resource(&s, &args) {
                        debug!("resource request: {e}");
                    }
                }
                Ok(())
            }));
            let s = self.shared.clone();
            let navigation = NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    if let Err(e) = on_navigation(&s, &args, true) {
                        debug!("navigation: {e}");
                    }
                }
                Ok(())
            }));
            let s = self.shared.clone();
            let frame_navigation =
                NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        if let Err(e) = on_navigation(&s, &args, false) {
                            debug!("frame navigation: {e}");
                        }
                    }
                    Ok(())
                }));
            let s = self.shared.clone();
            let completed =
                NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
                    let mut ok = BOOL::default();
                    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                    if let Some(args) = args {
                        // SAFETY: live event args; both are valid out pointers.
                        unsafe {
                            let _ = args.IsSuccess(&mut ok);
                            let _ = args.WebErrorStatus(&mut status);
                        }
                    }
                    let cancelled = status == COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED;
                    on_loaded(&s, sender.as_ref(), ok.as_bool(), cancelled);
                    Ok(())
                }));
            let s = self.shared.clone();
            let messages = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    on_message(&s, &args);
                }
                Ok(())
            }));
            let s = self.shared.clone();
            let failures = ProcessFailedEventHandler::create(Box::new(move |_, args| {
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                if let Some(args) = args {
                    // SAFETY: live event args; `kind` is a valid out pointer.
                    let _ = unsafe { args.ProcessFailedKind(&mut kind) };
                }
                let browser = kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED;
                warn!("WebView2 process failed (kind {})", kind.0);
                s.emit(HostEvent::ProcessFailed { browser });
                Ok(())
            }));
            let s = self.shared.clone();
            let frames = FrameCreatedEventHandler::create(Box::new(move |_, args| {
                // SAFETY: live event args during the callback.
                if let Some(frame) = args.and_then(|a| unsafe { a.Frame() }.ok()) {
                    attach_frame(&s, &frame);
                }
                Ok(())
            }));

            let mut token = 0i64;
            // SAFETY: COM calls on live objects owned by this thread; the handlers are COM
            // objects that WebView2 keeps alive while registered and calls on this thread.
            unsafe {
                let settings = wv.Settings()?;
                settings.SetAreDevToolsEnabled(false)?;
                settings.SetAreDefaultContextMenusEnabled(false)?;
                settings.SetIsStatusBarEnabled(false)?;
                settings.SetAreDefaultScriptDialogsEnabled(false)?;
                match settings.cast::<ICoreWebView2Settings2>() {
                    Ok(s2) => s2.SetUserAgent(&HSTRING::from(USER_AGENT))?,
                    Err(_) => warn!("WebView2 runtime too old to set the user agent"),
                }
                wv.add_ScriptDialogOpening(&dialogs, &mut token)?;
                wv.add_NewWindowRequested(&popups, &mut token)?;

                // ad-block on every request, iframes included when the runtime allows it
                match wv.cast::<ICoreWebView2_22>() {
                    Ok(wv22) => wv22.AddWebResourceRequestedFilterWithRequestSourceKinds(
                        w!("*"),
                        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                        COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
                    )?,
                    Err(_) => {
                        warn!("WebView2 runtime without ICoreWebView2_22; iframe requests may bypass the ad-block");
                        wv.AddWebResourceRequestedFilter(
                            w!("*"),
                            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                        )?
                    }
                }
                wv.add_WebResourceRequested(&resources, &mut token)?;
                wv.add_NavigationStarting(&navigation, &mut token)?;
                wv.add_FrameNavigationStarting(&frame_navigation, &mut token)?;
                wv.add_NavigationCompleted(&completed, &mut token)?;
                wv.add_WebMessageReceived(&messages, &mut token)?;
                wv.add_ProcessFailed(&failures, &mut token)?;
                match wv.cast::<ICoreWebView2_4>() {
                    Ok(wv4) => wv4.add_FrameCreated(&frames, &mut token)?,
                    Err(_) => {
                        warn!("WebView2 runtime without FrameCreated; iframe messages are lost")
                    }
                }
            }
            Ok(())
        }

        /// The message loop. Returns after `Close` (or when every sender is gone).
        fn run(mut self) {
            self.drain();
            let mut msg = MSG::default();
            loop {
                // SAFETY: `msg` is a valid out pointer; this thread owns its queue.
                let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if got.0 == 0 || got.0 == -1 {
                    break;
                }
                let wake = msg.hwnd.is_invalid() && msg.message == WM_WAKE;
                if !wake {
                    // SAFETY: dispatching a message this thread just received.
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                // completions (script registration) arrive as dispatched messages too
                self.drain();
            }
            if !self.closed {
                self.shutdown();
            }
        }

        /// Applies queued commands until the queue is empty or a registration is pending.
        fn drain(&mut self) {
            while !self.closed && !self.script.borrow().pending {
                let cmd = match self.rx.try_recv() {
                    Ok(cmd) => cmd,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => HostCommand::Close,
                };
                if let Err(e) = self.apply(cmd) {
                    warn!("WebView2 host command failed: {e}");
                }
            }
        }

        fn apply(&mut self, cmd: HostCommand) -> windows::core::Result<()> {
            let wv = &self.webview;
            match cmd {
                HostCommand::Navigate(url) => {
                    // SAFETY: COM call on a live object with a live string.
                    unsafe { wv.Navigate(&HSTRING::from(url.as_str())) }
                }
                HostCommand::SetDocumentScript(script) => self.set_script(script),
                HostCommand::ExecuteScript { script, reply } => {
                    let handler =
                        ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
                            if let Some(reply) = reply {
                                let _ = reply.send(result.ok().map(|_| json));
                            }
                            Ok(())
                        }));
                    // SAFETY: COM call on a live object; the handler is a live COM object.
                    unsafe { wv.ExecuteScript(&HSTRING::from(script.as_str()), &handler) }
                }
                HostCommand::Resize(rect) => {
                    let flags = if self.hidden {
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE
                    } else {
                        SWP_NOZORDER | SWP_NOACTIVATE
                    };
                    // SAFETY: the window and controller belong to this thread.
                    unsafe {
                        SetWindowPos(
                            self.hwnd,
                            None,
                            rect.x,
                            rect.y,
                            rect.width.max(1),
                            rect.height.max(1),
                            flags,
                        )?;
                        self.controller.SetBounds(to_rect(rect.local()))
                    }
                }
                HostCommand::Show => {
                    if self.hidden {
                        cloak(self.hwnd, false)
                    } else {
                        // SAFETY: the window and controller belong to this thread.
                        unsafe {
                            let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                            SetWindowPos(
                                self.hwnd,
                                Some(HWND_TOP),
                                0,
                                0,
                                0,
                                0,
                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                            )?;
                            self.controller.SetIsVisible(true)
                        }
                    }
                }
                HostCommand::Hide => {
                    if self.hidden {
                        cloak(self.hwnd, true)
                    } else {
                        // SAFETY: the window and controller belong to this thread.
                        unsafe {
                            self.controller.SetIsVisible(false)?;
                            let _ = ShowWindow(self.hwnd, SW_HIDE);
                        }
                        Ok(())
                    }
                }
                HostCommand::Close => {
                    self.closed = true;
                    self.close_objects();
                    // SAFETY: ends this thread's message loop.
                    unsafe { PostQuitMessage(0) };
                    Ok(())
                }
            }
        }

        fn set_script(&mut self, script: String) -> windows::core::Result<()> {
            let wv = &self.webview;
            {
                let slot = self.script.borrow();
                if slot.id.is_some() && slot.source == script {
                    return Ok(());
                }
            }
            if let Some(old) = self.script.borrow_mut().id.take() {
                // SAFETY: COM call on a live object with a live string.
                unsafe { wv.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(old)) }?;
            }
            *self.shared.frame_script.borrow_mut() = frame_fallback_script(&script);
            {
                let mut slot = self.script.borrow_mut();
                slot.source = script.clone();
                slot.pending = true;
            }
            let slot = self.script.clone();
            let handler = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(
                move |result, id| {
                    let mut slot = slot.borrow_mut();
                    slot.pending = false;
                    match result {
                        Ok(()) => slot.id = Some(id),
                        Err(e) => warn!("document-start script not registered: {e}"),
                    }
                    Ok(())
                },
            ));
            let wrapped = HSTRING::from(document_script(&script));
            // SAFETY: COM call on a live object; the handler is a live COM object.
            let added = unsafe { wv.AddScriptToExecuteOnDocumentCreated(&wrapped, &handler) };
            if added.is_err() {
                self.script.borrow_mut().pending = false;
            }
            added
        }

        fn close_objects(&self) {
            // SAFETY: the controller and window belong to this thread and are not used after.
            unsafe {
                let _ = self.controller.Close();
                let _ = DestroyWindow(self.hwnd);
            }
        }

        /// Tears down without a message loop (setup failed after the controller existed).
        fn shutdown(self) {
            self.close_objects();
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: forwarding the arguments Windows passed to this window procedure.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    fn create_window(surface: Surface) -> windows::core::Result<(HWND, Rect)> {
        // SAFETY: plain Win32 calls; the class name and procedure are 'static.
        unsafe {
            let instance = GetModuleHandleW(None).ok().map(|h| HINSTANCE(h.0));
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance.unwrap_or_default(),
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            // a second registration fails with ERROR_CLASS_ALREADY_EXISTS, which is fine
            let _ = RegisterClassExW(&class);
            match surface {
                Surface::Hidden => {
                    let hwnd = CreateWindowExW(
                        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                        CLASS_NAME,
                        w!("Flox"),
                        WS_POPUP,
                        0,
                        0,
                        HIDDEN_WIDTH,
                        HIDDEN_HEIGHT,
                        None,
                        None,
                        instance,
                        None,
                    )?;
                    // cloak before the first show so nothing ever flashes on screen
                    if let Err(e) = cloak(hwnd, true) {
                        let _ = DestroyWindow(hwnd);
                        return Err(e);
                    }
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    Ok((hwnd, Rect::new(0, 0, HIDDEN_WIDTH, HIDDEN_HEIGHT)))
                }
                Surface::Child { parent, rect } => {
                    let hwnd = CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        CLASS_NAME,
                        PCWSTR::null(),
                        WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                        rect.x,
                        rect.y,
                        rect.width.max(1),
                        rect.height.max(1),
                        Some(HWND(parent as *mut c_void)),
                        None,
                        instance,
                        None,
                    )?;
                    Ok((hwnd, rect))
                }
            }
        }
    }

    fn cloak(hwnd: HWND, on: bool) -> windows::core::Result<()> {
        let value = BOOL::from(on);
        // SAFETY: `value` outlives the call and the size matches the attribute's BOOL.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_CLOAK,
                (&value as *const BOOL).cast::<c_void>(),
                std::mem::size_of::<BOOL>() as u32,
            )
        }
    }

    fn to_rect(r: Rect) -> RECT {
        RECT {
            left: r.x,
            top: r.y,
            right: r.x + r.width,
            bottom: r.y + r.height,
        }
    }

    fn wv2_error(e: webview2_com::Error) -> windows::core::Error {
        match e {
            webview2_com::Error::WindowsError(e) => e,
            other => windows::core::Error::new(E_FAIL, other.to_string()),
        }
    }

    fn create_environment(config: &HostConfig) -> windows::core::Result<ICoreWebView2Environment> {
        let options = CoreWebView2EnvironmentOptions::default();
        // SAFETY: the options object is not shared with WebView2 yet, so nothing else reads
        // the cell while it is written.
        unsafe { options.set_additional_browser_arguments(browser_arguments(config.muted)) };
        let options: ICoreWebView2EnvironmentOptions = options.into();
        let folder = HSTRING::from(config.user_data.as_path());
        let slot: Rc<RefCell<Option<ICoreWebView2Environment>>> = Rc::default();
        let out = slot.clone();
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                // SAFETY: the folder string and options outlive the call; the handler is a
                // live COM object.
                unsafe {
                    CreateCoreWebView2EnvironmentWithOptions(
                        PCWSTR::null(),
                        &folder,
                        &options,
                        &handler,
                    )
                }
                .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |result, env| {
                result?;
                *out.borrow_mut() = env;
                Ok(())
            }),
        )
        .map_err(wv2_error)?;
        slot.take().ok_or_else(|| E_POINTER.into())
    }

    fn create_controller(
        env: &ICoreWebView2Environment,
        hwnd: HWND,
    ) -> windows::core::Result<ICoreWebView2Controller> {
        let env10 = env.cast::<ICoreWebView2Environment10>().ok();
        if env10.is_none() {
            warn!("WebView2 runtime without controller options; the profile is not InPrivate");
        }
        let env = env.clone();
        let slot: Rc<RefCell<Option<ICoreWebView2Controller>>> = Rc::default();
        let out = slot.clone();
        CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                // SAFETY: COM calls on live objects; the window belongs to this thread.
                unsafe {
                    match env10 {
                        Some(env10) => {
                            let options = env10.CreateCoreWebView2ControllerOptions()?;
                            options.SetIsInPrivateModeEnabled(true)?;
                            env10.CreateCoreWebView2ControllerWithOptions(
                                hwnd, &options, &handler,
                            )?;
                        }
                        None => env.CreateCoreWebView2Controller(hwnd, &handler)?,
                    }
                }
                Ok(())
            }),
            Box::new(move |result, controller| {
                result?;
                *out.borrow_mut() = controller;
                Ok(())
            }),
        )
        .map_err(wv2_error)?;
        slot.take().ok_or_else(|| E_POINTER.into())
    }

    const CONTEXTS: &[(COREWEBVIEW2_WEB_RESOURCE_CONTEXT, ResourceKind)] = &[
        (
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
            ResourceKind::Document,
        ),
        (
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET,
            ResourceKind::Stylesheet,
        ),
        (COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE, ResourceKind::Image),
        (COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA, ResourceKind::Media),
        (COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT, ResourceKind::Font),
        (
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT,
            ResourceKind::Script,
        ),
        (
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST,
            ResourceKind::XmlHttpRequest,
        ),
        (COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH, ResourceKind::Fetch),
        (
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_TEXT_TRACK,
            ResourceKind::TextTrack,
        ),
        (
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_EVENT_SOURCE,
            ResourceKind::EventSource,
        ),
        (
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET,
            ResourceKind::Websocket,
        ),
        (
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MANIFEST,
            ResourceKind::Manifest,
        ),
    ];

    fn resource_kind(ctx: COREWEBVIEW2_WEB_RESOURCE_CONTEXT) -> ResourceKind {
        CONTEXTS
            .iter()
            .find(|(c, _)| *c == ctx)
            .map_or(ResourceKind::Other, |(_, k)| *k)
    }

    fn header(request: &ICoreWebView2WebResourceRequest, name: &str) -> Option<String> {
        let mut raw = PWSTR::null();
        // SAFETY: COM calls on a live request; `raw` receives a CoTaskMem string that
        // take_pwstr frees.
        unsafe {
            let headers = request.Headers().ok()?;
            headers.GetHeader(&HSTRING::from(name), &mut raw).ok()?;
        }
        Some(take_pwstr(raw))
    }

    fn on_resource(
        shared: &Shared,
        args: &ICoreWebView2WebResourceRequestedEventArgs,
    ) -> windows::core::Result<()> {
        // SAFETY: COM calls on live event args during the callback; strings returned through
        // out pointers are freed by take_pwstr.
        unsafe {
            let request = args.Request()?;
            let mut raw = PWSTR::null();
            request.Uri(&mut raw)?;
            let url = take_pwstr(raw);
            if !intercepts(&url) {
                return Ok(());
            }
            let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
            args.ResourceContext(&mut ctx)?;
            let kind = resource_kind(ctx);
            let fetch_dest = header(&request, "Sec-Fetch-Dest");
            let is_main_frame = kind == ResourceKind::Document && *shared.main_url.borrow() == url;
            let info = RequestInfo {
                url: &url,
                is_main_frame,
                fetch_dest: fetch_dest.as_deref(),
                resource_kind: kind,
            };
            if policy::verdict(&info) == Verdict::Block {
                let response = shared.env.CreateWebResourceResponse(
                    None::<&IStream>,
                    403,
                    w!("Forbidden"),
                    w!(""),
                )?;
                args.SetResponse(&response)?;
            }
        }
        Ok(())
    }

    fn on_navigation(
        shared: &Shared,
        args: &ICoreWebView2NavigationStartingEventArgs,
        main_frame: bool,
    ) -> windows::core::Result<()> {
        let mut raw = PWSTR::null();
        // SAFETY: live event args; the string is freed by take_pwstr.
        unsafe { args.Uri(&mut raw)? };
        let url = take_pwstr(raw);
        if !policy::navigation_allowed(&url, main_frame) {
            debug!("navigation blocked: {url}");
            // SAFETY: live event args during the callback.
            return unsafe { args.SetCancel(true) };
        }
        if main_frame {
            *shared.main_url.borrow_mut() = url;
        }
        Ok(())
    }

    fn on_loaded(shared: &Shared, sender: Option<&ICoreWebView2>, success: bool, cancelled: bool) {
        let url = sender
            .and_then(|wv| {
                let mut raw = PWSTR::null();
                // SAFETY: COM call on the live sender; the string is freed by take_pwstr.
                unsafe { wv.Source(&mut raw) }.ok()?;
                Some(take_pwstr(raw))
            })
            .unwrap_or_default();
        if success && !url.starts_with("about:") {
            if let (Some(script), Some(wv)) = (&shared.after_load, sender) {
                // SAFETY: COM call on the live sender with a live string.
                let run = unsafe {
                    wv.ExecuteScript(
                        &HSTRING::from(script.as_str()),
                        None::<&ICoreWebView2ExecuteScriptCompletedHandler>,
                    )
                };
                if let Err(e) = run {
                    warn!("after-load script failed: {e}");
                }
            }
        }
        shared.emit(HostEvent::Loaded {
            url,
            success,
            cancelled,
        });
    }

    fn message_text(args: &ICoreWebView2WebMessageReceivedEventArgs) -> Option<String> {
        let mut raw = PWSTR::null();
        // SAFETY: live event args; strings are freed by take_pwstr. The shim posts strings;
        // anything else is read as JSON, which bridge::parse also accepts.
        unsafe {
            if args.TryGetWebMessageAsString(&mut raw).is_ok() {
                return Some(take_pwstr(raw));
            }
            let mut raw = PWSTR::null();
            args.WebMessageAsJson(&mut raw).ok()?;
            Some(take_pwstr(raw))
        }
    }

    fn on_message(shared: &Shared, args: &ICoreWebView2WebMessageReceivedEventArgs) {
        if let Some(msg) = message_text(args).as_deref().and_then(bridge::parse) {
            shared.emit(HostEvent::Message(msg));
        }
    }

    /// Frame messages, the document-start fallback and nested frames.
    fn attach_frame(shared: &Rc<Shared>, frame: &ICoreWebView2Frame) {
        let Ok(frame2) = frame.cast::<ICoreWebView2Frame2>() else {
            debug!("WebView2 runtime without ICoreWebView2Frame2");
            return;
        };
        let s = shared.clone();
        let messages = FrameWebMessageReceivedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                on_message(&s, &args);
            }
            Ok(())
        }));
        let s = shared.clone();
        let loading = FrameContentLoadingEventHandler::create(Box::new(move |sender, _| {
            let script = s.frame_script.borrow().clone();
            let frame = sender.and_then(|f| f.cast::<ICoreWebView2Frame2>().ok());
            if let (false, Some(frame)) = (script.is_empty(), frame) {
                // SAFETY: COM call on the live sender frame with a live string.
                let _ = unsafe {
                    frame.ExecuteScript(
                        &HSTRING::from(script),
                        None::<&ICoreWebView2ExecuteScriptCompletedHandler>,
                    )
                };
            }
            Ok(())
        }));
        let s = shared.clone();
        let children = FrameChildFrameCreatedEventHandler::create(Box::new(move |_, args| {
            // SAFETY: live event args during the callback.
            if let Some(child) = args.and_then(|a| unsafe { a.Frame() }.ok()) {
                attach_frame(&s, &child);
            }
            Ok(())
        }));
        let mut token = 0i64;
        // SAFETY: COM calls on a live frame; the handlers are COM objects WebView2 keeps
        // alive while registered and calls on this thread.
        unsafe {
            let _ = frame2.add_WebMessageReceived(&messages, &mut token);
            let _ = frame2.add_ContentLoading(&loading, &mut token);
            if let Ok(frame7) = frame.cast::<ICoreWebView2Frame7>() {
                let _ = frame7.add_FrameCreated(&children, &mut token);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments() {
        let muted = browser_arguments(true);
        let loud = browser_arguments(false);
        assert_eq!(
            muted,
            "--autoplay-policy=no-user-gesture-required --disable-features=CalculateNativeWinOcclusion,msWebOOUI,msPdfOOUI --mute-audio"
        );
        assert!(!loud.contains("--mute-audio"));
        assert!(loud.contains("CalculateNativeWinOcclusion"));
    }

    #[test]
    fn user_data_under_temp() {
        let p = default_user_data("sniffer");
        assert!(p.starts_with(std::env::temp_dir()));
        assert!(p.ends_with("flox/webview2/sniffer") || p.ends_with("flox\\webview2\\sniffer"));
        assert_ne!(default_user_data("page"), p);
    }

    #[test]
    fn script_wrappers() {
        let s = "(function () { window.x = 1 })()";
        let doc = document_script(s);
        assert!(doc.starts_with("window.__floxDocStart = true"));
        assert!(doc.ends_with(s));
        let frame = frame_fallback_script(s);
        assert!(frame.contains("if (window.__floxDocStart) return"));
        assert!(frame.contains(s));
        assert!(frame.ends_with("})()"));
    }

    #[test]
    fn local_schemes_skip_the_verdict() {
        assert!(intercepts("https://vidlink.pro/tv/1/1/1"));
        assert!(intercepts("wss://example.com/socket"));
        assert!(!intercepts("data:image/png;base64,AAAA"));
        assert!(!intercepts("blob:https://vidlink.pro/1234"));
        assert!(!intercepts("about:blank"));
        assert!(!intercepts("DATA:text/plain,x"));
    }

    #[test]
    fn rect_local() {
        assert_eq!(
            Rect::new(10, 20, 300, 200).local(),
            Rect::new(0, 0, 300, 200)
        );
        assert_eq!(Rect::new(5, 5, 0, -3).local(), Rect::new(0, 0, 1, 1));
    }

    #[test]
    fn commands() {
        assert!(HostCommand::Close.is_close());
        assert!(!HostCommand::Show.is_close());
        assert!(HostCommand::SetDocumentScript(String::new()).blocks_queue());
        assert!(!HostCommand::Navigate("about:blank".to_owned()).blocks_queue());
        let (tx, _rx) = oneshot::channel();
        let exec = HostCommand::ExecuteScript {
            script: "1".to_owned(),
            reply: Some(tx),
        };
        assert!(!exec.blocks_queue() && !exec.is_close());
        // commands cross from async tasks to the host thread
        fn assert_send<T: Send>() {}
        assert_send::<HostCommand>();
        assert_send::<EventSink>();
    }

    /// `node --check` on the wrapped scripts, when node is installed.
    #[test]
    fn wrapped_scripts_parse_with_node() {
        use crate::assets::{document_start_script, ScriptOptions};
        use flox_core::sniff::SniffMode;
        let has_node = std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !has_node {
            eprintln!("node not found; skipping syntax check");
            return;
        }
        let dir = std::env::temp_dir().join(format!("flox-web-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = document_start_script(SniffMode::Rip, &ScriptOptions::default());
        for (name, body) in [
            ("doc.js", document_script(&script)),
            ("frame.js", frame_fallback_script(&script)),
        ] {
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
