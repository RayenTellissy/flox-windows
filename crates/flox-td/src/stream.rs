//! A blocking `Read + Seek` over an entry's parts, fed by TDLib downloads with
//! Android's sliding-window rules. Called from mpv's stream thread. Filled in by piece P6.

use std::io;
use std::sync::Arc;

use flox_core::error::{Error, Result};
use tokio_util::sync::CancellationToken;

use crate::library::Part;
use crate::transport::TdTransport;

/// Bytes per `readFilePart`.
pub const CHUNK: usize = 512 * 1024;
/// `downloadFile` window length.
pub const WINDOW: i64 = 256 << 20;
/// Start prefetching the next part when this much of the current part is left.
pub const PREFETCH_WHEN_LEFT: i64 = 512 << 20;
/// How much of the next part to prefetch.
pub const PREFETCH_BYTES: i64 = 64 << 20;

/// The stream.
pub struct TdStream {
    _private: (),
}

impl TdStream {
    /// Prepares the stream; nothing downloads until the first read.
    pub fn open(
        _t: Arc<dyn TdTransport>,
        _rt: tokio::runtime::Handle,
        _parts: Vec<Part>,
    ) -> Result<TdStream> {
        Err(Error::NotImplemented("flox_td::stream::TdStream::open"))
    }

    /// Total size of all parts. Filled in by P6.
    #[allow(clippy::unimplemented)]
    pub fn size(&self) -> u64 {
        unimplemented!("flox_td::stream::TdStream::size (P6)")
    }

    /// Cancels blocked reads. Filled in by P6.
    #[allow(clippy::unimplemented)]
    pub fn cancel_handle(&self) -> CancellationToken {
        unimplemented!("flox_td::stream::TdStream::cancel_handle (P6)")
    }

    /// Cancels downloads and deletes every part's local file.
    pub fn close(self) -> Result<()> {
        Err(Error::NotImplemented("flox_td::stream::TdStream::close"))
    }
}

impl io::Read for TdStream {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other(
            "flox_td::stream::TdStream::read not implemented",
        ))
    }
}

impl io::Seek for TdStream {
    fn seek(&mut self, _pos: io::SeekFrom) -> io::Result<u64> {
        Err(io::Error::other(
            "flox_td::stream::TdStream::seek not implemented",
        ))
    }
}
