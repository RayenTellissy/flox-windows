//! libmpv, loaded at runtime with `libloading` (no import library, no link step),
//! wrapped for the player screen.

pub mod ffi;
pub mod filters;
pub mod mpv;
pub mod options;
pub mod render;
pub mod stream_cb;
pub mod tracks;
