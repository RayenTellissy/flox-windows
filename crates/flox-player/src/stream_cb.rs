//! `mpv_stream_cb_add_ro`: custom protocols backed by Rust readers. Filled in by piece P11.

use flox_core::error::{Error, Result};

use crate::mpv::Mpv;

/// A seekable source mpv reads from its stream thread.
pub trait StreamSource: std::io::Read + std::io::Seek + Send {
    /// Total size when known.
    fn size(&self) -> Option<u64>;
    /// Unblocks a pending read (mpv `cancel_fn`).
    fn cancel(&self);
}

/// The opener maps a URI such as `flox://…` to a source, or `None` for "not found".
pub type Opener = Box<dyn Fn(&str) -> Option<Box<dyn StreamSource>> + Send + Sync>;

/// Registers `protocol` on `mpv`.
pub fn register(_mpv: &Mpv, _protocol: &str, _opener: Opener) -> Result<()> {
    Err(Error::NotImplemented("flox_player::stream_cb::register"))
}
