//! The hidden WebView2 host: an STA thread with a message loop, a cloaked 1280x720
//! tool window and a visible controller. Filled in by piece P13.

/// The WebView2 host thread and its controller.
pub struct WebHost {
    _private: (),
}
