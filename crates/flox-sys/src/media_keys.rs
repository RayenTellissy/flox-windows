//! System media transport controls (SMTC) for hardware media keys. Filled in by piece P14.

use flox_core::error::{Error, Result};

/// A media key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKey {
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}

/// SMTC attached to a window.
pub struct MediaControls {
    _private: (),
}

impl MediaControls {
    /// `ISystemMediaTransportControlsInterop::GetForWindow(hwnd)`.
    pub fn attach(
        _hwnd: isize,
        _on_key: Box<dyn Fn(MediaKey) + Send + Sync>,
    ) -> Result<MediaControls> {
        Err(Error::NotImplemented(
            "flox_sys::media_keys::MediaControls::attach",
        ))
    }

    /// Updates the playback status and title shown by the OS. Filled in by P14.
    #[allow(clippy::unimplemented)]
    pub fn set_playing(&self, _playing: bool, _title: &str) {
        unimplemented!("flox_sys::media_keys::MediaControls::set_playing (P14)")
    }
}
