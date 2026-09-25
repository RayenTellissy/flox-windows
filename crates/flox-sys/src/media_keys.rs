//! System media transport controls (SMTC) for hardware media keys.
//!
//! On Windows the controls come from
//! `ISystemMediaTransportControlsInterop::GetForWindow(hwnd)`, so key presses
//! and the flyout belong to the player window. `ButtonPressed` events arrive on
//! a system thread and are forwarded to the callback there. SMTC has no
//! play/pause toggle button: the OS sends `Play` or `Pause` depending on the
//! status last set through [`MediaControls::set_playing`].
//!
//! Off Windows attaching succeeds and everything is a no-op logged at debug.

use flox_core::error::Result;

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

/// SMTC attached to a window. Detaches (and clears the OS flyout) on drop.
pub struct MediaControls {
    inner: platform::Inner,
}

impl MediaControls {
    /// `ISystemMediaTransportControlsInterop::GetForWindow(hwnd)`.
    pub fn attach(
        hwnd: isize,
        on_key: Box<dyn Fn(MediaKey) + Send + Sync>,
    ) -> Result<MediaControls> {
        Ok(MediaControls {
            inner: platform::Inner::attach(hwnd, on_key)?,
        })
    }

    /// Updates the playback status and title shown by the OS. Failures are
    /// logged; they never affect playback.
    pub fn set_playing(&self, playing: bool, title: &str) {
        self.inner.set_playing(playing, title);
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;

    use flox_core::error::{Error, Result};
    use windows::core::{factory, Ref, HSTRING};
    use windows::Foundation::TypedEventHandler;
    use windows::Media::{
        MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
        SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;

    use super::MediaKey;

    fn win(context: &str, err: windows::core::Error) -> Error {
        Error::Other(format!("{context}: {err}"))
    }

    pub(super) struct Inner {
        smtc: SystemMediaTransportControls,
        token: i64,
    }

    pub(super) fn key_for(button: SystemMediaTransportControlsButton) -> Option<MediaKey> {
        match button {
            SystemMediaTransportControlsButton::Play => Some(MediaKey::Play),
            SystemMediaTransportControlsButton::Pause => Some(MediaKey::Pause),
            SystemMediaTransportControlsButton::Next => Some(MediaKey::Next),
            SystemMediaTransportControlsButton::Previous => Some(MediaKey::Previous),
            SystemMediaTransportControlsButton::Stop => Some(MediaKey::Stop),
            _ => None,
        }
    }

    impl Inner {
        pub(super) fn attach(
            hwnd: isize,
            on_key: Box<dyn Fn(MediaKey) + Send + Sync>,
        ) -> Result<Inner> {
            let interop =
                factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()
                    .map_err(|e| win("SMTC interop factory", e))?;
            // SAFETY: `hwnd` is the caller's live top-level window handle; the
            // interop only reads it to find the window's SMTC instance.
            let smtc: SystemMediaTransportControls =
                unsafe { interop.GetForWindow(HWND(hwnd as *mut c_void)) }
                    .map_err(|e| win("GetForWindow", e))?;

            smtc.SetIsEnabled(true)
                .map_err(|e| win("SetIsEnabled", e))?;
            smtc.SetIsPlayEnabled(true)
                .map_err(|e| win("SetIsPlayEnabled", e))?;
            smtc.SetIsPauseEnabled(true)
                .map_err(|e| win("SetIsPauseEnabled", e))?;
            smtc.SetIsNextEnabled(true)
                .map_err(|e| win("SetIsNextEnabled", e))?;
            smtc.SetIsPreviousEnabled(true)
                .map_err(|e| win("SetIsPreviousEnabled", e))?;
            smtc.SetIsStopEnabled(true)
                .map_err(|e| win("SetIsStopEnabled", e))?;

            let handler = TypedEventHandler::new(
                move |_: Ref<SystemMediaTransportControls>,
                      args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
                    if let Some(key) = args.as_ref().and_then(|a| a.Button().ok()).and_then(key_for) {
                        on_key(key);
                    }
                    Ok(())
                },
            );
            let token = smtc
                .ButtonPressed(&handler)
                .map_err(|e| win("ButtonPressed", e))?;
            Ok(Inner { smtc, token })
        }

        pub(super) fn set_playing(&self, playing: bool, title: &str) {
            if let Err(err) = self.update(playing, title) {
                tracing::warn!(%err, "could not update the media controls");
            }
        }

        fn update(&self, playing: bool, title: &str) -> windows::core::Result<()> {
            let status = if playing {
                MediaPlaybackStatus::Playing
            } else {
                MediaPlaybackStatus::Paused
            };
            self.smtc.SetPlaybackStatus(status)?;
            let display = self.smtc.DisplayUpdater()?;
            display.SetType(MediaPlaybackType::Video)?;
            display.VideoProperties()?.SetTitle(&HSTRING::from(title))?;
            display.Update()
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            let _ = self.smtc.RemoveButtonPressed(self.token);
            let _ = self.smtc.SetPlaybackStatus(MediaPlaybackStatus::Closed);
            if let Ok(display) = self.smtc.DisplayUpdater() {
                let _ = display.ClearAll();
                let _ = display.Update();
            }
            let _ = self.smtc.SetIsEnabled(false);
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use flox_core::error::Result;

    use super::MediaKey;

    pub(super) struct Inner;

    impl Inner {
        pub(super) fn attach(
            hwnd: isize,
            _on_key: Box<dyn Fn(MediaKey) + Send + Sync>,
        ) -> Result<Inner> {
            tracing::debug!(hwnd, "media controls: no-op on this platform");
            Ok(Inner)
        }

        pub(super) fn set_playing(&self, playing: bool, title: &str) {
            tracing::debug!(
                playing,
                title,
                "media controls update (no-op on this platform)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(windows))]
    #[test]
    fn no_op_off_windows() {
        use super::*;
        let controls = MediaControls::attach(0, Box::new(|_| {})).expect("attach");
        controls.set_playing(true, "Title");
        controls.set_playing(false, "Title");
    }

    #[cfg(windows)]
    #[test]
    fn button_mapping() {
        use super::platform::key_for;
        use super::MediaKey;
        use windows::Media::SystemMediaTransportControlsButton as B;
        assert_eq!(key_for(B::Play), Some(MediaKey::Play));
        assert_eq!(key_for(B::Pause), Some(MediaKey::Pause));
        assert_eq!(key_for(B::Next), Some(MediaKey::Next));
        assert_eq!(key_for(B::Previous), Some(MediaKey::Previous));
        assert_eq!(key_for(B::Stop), Some(MediaKey::Stop));
        assert_eq!(key_for(B::FastForward), None);
    }
}
