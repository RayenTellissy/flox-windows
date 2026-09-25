//! `mpv_stream_cb_add_ro`: custom protocols backed by Rust readers.
//!
//! mpv calls the open callback with a URI such as `flox://…`; the boxed opener
//! turns it into a [`StreamSource`], which the read, seek, size, close and
//! cancel trampolines drive from mpv's stream thread. Every trampoline is
//! guarded with `catch_unwind` so a panic never unwinds into C.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::io::{Read, Seek, SeekFrom};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use flox_core::error::{Error, Result};
use parking_lot::Mutex;

use crate::ffi::{self, MpvStreamCbInfo};
use crate::mpv::Mpv;

/// Something that can interrupt a blocked read from another thread.
pub type Canceller = Arc<dyn Fn() + Send + Sync>;

/// A seekable source mpv reads from its stream thread.
pub trait StreamSource: std::io::Read + std::io::Seek + Send {
    /// Total size when known.
    fn size(&self) -> Option<u64>;
    /// Unblocks a pending read (mpv `cancel_fn`).
    ///
    /// Called with the source locked, so it only runs while no read is in
    /// progress. Sources whose reads block should also return a
    /// [`StreamSource::canceller`], which runs concurrently with a read.
    fn cancel(&self);
    /// A handle that interrupts a read that is blocked right now. mpv's
    /// `cancel_fn` arrives on another thread while the stream thread is inside
    /// `read`, so this must not need access to the source itself.
    fn canceller(&self) -> Option<Canceller> {
        None
    }
}

/// The opener maps a URI such as `flox://…` to a source, or `None` for "not found".
pub type Opener = Box<dyn Fn(&str) -> Option<Box<dyn StreamSource>> + Send + Sync>;

/// Registers `protocol` on `mpv`. The opener lives until the mpv core is
/// destroyed (mpv has no way to unregister a protocol).
pub fn register(mpv: &Mpv, protocol: &str, opener: Opener) -> Result<()> {
    let name = std::ffi::CString::new(protocol)
        .map_err(|_| Error::Other(format!("NUL byte in protocol {protocol:?}")))?;
    let boxed: Box<Opener> = Box::new(opener);
    let user_data = &*boxed as *const Opener as *mut c_void;
    // SAFETY: `user_data` points into a heap box that is kept in the handle's
    // keep-alive list below, which is dropped only after mpv_terminate_destroy,
    // after which mpv no longer calls open_fn.
    let code = unsafe {
        (mpv.handle.lib.stream_cb_add_ro)(
            mpv.handle.ptr,
            name.as_ptr(),
            user_data,
            Some(open_trampoline),
        )
    };
    mpv.handle.check(code)?;
    // Moving the outer box keeps the heap address `user_data` points to.
    mpv.handle.keep_alive.lock().push(boxed);
    Ok(())
}

/// Per-stream state behind mpv's `cookie`.
struct Cookie {
    source: Mutex<Box<dyn StreamSource>>,
    canceller: Option<Canceller>,
    cancelled: AtomicBool,
}

unsafe extern "C" fn open_trampoline(
    user_data: *mut c_void,
    uri: *mut c_char,
    info: *mut MpvStreamCbInfo,
) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if user_data.is_null() || uri.is_null() || info.is_null() {
            return ffi::MPV_ERROR_LOADING_FAILED;
        }
        // SAFETY: `user_data` is the `Opener` registered in `register`, alive until
        // the core is destroyed.
        let opener = unsafe { &*(user_data as *const Opener) };
        // SAFETY: mpv passes a NUL-terminated URI valid for this call.
        let uri = unsafe { CStr::from_ptr(uri) }.to_string_lossy();
        let Some(source) = opener(&uri) else {
            return ffi::MPV_ERROR_LOADING_FAILED;
        };
        let canceller = source.canceller();
        let cookie = Box::new(Cookie {
            source: Mutex::new(source),
            canceller,
            cancelled: AtomicBool::new(false),
        });
        // SAFETY: `info` is a valid out-struct owned by mpv for this call.
        unsafe {
            *info = MpvStreamCbInfo {
                cookie: Box::into_raw(cookie) as *mut c_void,
                read_fn: Some(read_trampoline),
                seek_fn: Some(seek_trampoline),
                size_fn: Some(size_trampoline),
                close_fn: Some(close_trampoline),
                cancel_fn: Some(cancel_trampoline),
            };
        }
        0
    }));
    r.unwrap_or(ffi::MPV_ERROR_LOADING_FAILED)
}

/// # Safety
/// `cookie` must come from `open_trampoline` and not yet be closed.
unsafe fn cookie<'a>(cookie: *mut c_void) -> &'a Cookie {
    // SAFETY: guaranteed by the caller; mpv keeps the cookie until close_fn.
    unsafe { &*(cookie as *const Cookie) }
}

unsafe extern "C" fn read_trampoline(c: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: mpv passes the cookie from open_fn.
        let c = unsafe { cookie(c) };
        if c.cancelled.load(Ordering::SeqCst) || buf.is_null() {
            return -1;
        }
        let len = usize::try_from(nbytes).unwrap_or(usize::MAX);
        // SAFETY: mpv hands a writable buffer of `nbytes` bytes, exclusively ours
        // for this call.
        let out = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, len) };
        let mut src = c.source.lock();
        loop {
            match src.read(out) {
                Ok(n) => return i64::try_from(n).unwrap_or(-1),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    if c.cancelled.load(Ordering::SeqCst) {
                        return -1;
                    }
                }
                Err(e) => {
                    tracing::debug!("stream read failed: {e}");
                    return -1;
                }
            }
        }
    }))
    .unwrap_or(-1)
}

unsafe extern "C" fn seek_trampoline(c: *mut c_void, offset: i64) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: mpv passes the cookie from open_fn.
        let c = unsafe { cookie(c) };
        if c.cancelled.load(Ordering::SeqCst) {
            return i64::from(ffi::MPV_ERROR_GENERIC);
        }
        let Ok(pos) = u64::try_from(offset) else {
            return i64::from(ffi::MPV_ERROR_GENERIC);
        };
        match c.source.lock().seek(SeekFrom::Start(pos)) {
            Ok(p) => i64::try_from(p).unwrap_or(i64::from(ffi::MPV_ERROR_GENERIC)),
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                i64::from(ffi::MPV_ERROR_UNSUPPORTED)
            }
            Err(_) => i64::from(ffi::MPV_ERROR_GENERIC),
        }
    }))
    .unwrap_or(i64::from(ffi::MPV_ERROR_GENERIC))
}

unsafe extern "C" fn size_trampoline(c: *mut c_void) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: mpv passes the cookie from open_fn.
        let c = unsafe { cookie(c) };
        c.source
            .lock()
            .size()
            .and_then(|s| i64::try_from(s).ok())
            .unwrap_or(i64::from(ffi::MPV_ERROR_UNSUPPORTED))
    }))
    .unwrap_or(i64::from(ffi::MPV_ERROR_UNSUPPORTED))
}

unsafe extern "C" fn close_trampoline(c: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !c.is_null() {
            // SAFETY: close_fn is the last call for this cookie; reclaim the box
            // created in open_fn exactly once.
            drop(unsafe { Box::from_raw(c as *mut Cookie) });
        }
    }));
}

unsafe extern "C" fn cancel_trampoline(c: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: mpv passes the cookie from open_fn; cancel_fn never follows close_fn.
        let c = unsafe { cookie(c) };
        c.cancelled.store(true, Ordering::SeqCst);
        if let Some(cancel) = &c.canceller {
            cancel();
        }
        // No read in flight: tell the source directly. Never block here.
        if let Some(src) = c.source.try_lock() {
            src.cancel();
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::AtomicUsize;

    struct Src {
        inner: Cursor<Vec<u8>>,
        cancels: Arc<AtomicUsize>,
    }

    impl Read for Src {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Seek for Src {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    impl StreamSource for Src {
        fn size(&self) -> Option<u64> {
            Some(self.inner.get_ref().len() as u64)
        }
        fn cancel(&self) {
            self.cancels.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn open(opener: &Opener, uri: &str) -> (c_int, MpvStreamCbInfo) {
        let uri = std::ffi::CString::new(uri).unwrap();
        let mut info = MpvStreamCbInfo {
            cookie: std::ptr::null_mut(),
            read_fn: None,
            seek_fn: None,
            size_fn: None,
            close_fn: None,
            cancel_fn: None,
        };
        // SAFETY: valid opener, URI and out-struct.
        let code = unsafe {
            open_trampoline(
                opener as *const Opener as *mut c_void,
                uri.as_ptr() as *mut c_char,
                &mut info,
            )
        };
        (code, info)
    }

    #[test]
    fn trampolines_drive_the_source() {
        let cancels = Arc::new(AtomicUsize::new(0));
        let counter = cancels.clone();
        let opener: Opener = Box::new(move |uri| {
            (uri == "flox://x").then(|| {
                Box::new(Src {
                    inner: Cursor::new(b"0123456789".to_vec()),
                    cancels: counter.clone(),
                }) as Box<dyn StreamSource>
            })
        });

        let (code, _) = open(&opener, "flox://missing");
        assert_eq!(code, ffi::MPV_ERROR_LOADING_FAILED);

        let (code, info) = open(&opener, "flox://x");
        assert_eq!(code, 0);
        let c = info.cookie;
        let mut buf = [0u8; 4];
        // SAFETY: `c` is a live cookie; `buf` is writable for 4 bytes.
        unsafe {
            assert_eq!(size_trampoline(c), 10);
            assert_eq!(read_trampoline(c, buf.as_mut_ptr().cast(), 4), 4);
            assert_eq!(&buf, b"0123");
            assert_eq!(seek_trampoline(c, 8), 8);
            assert_eq!(read_trampoline(c, buf.as_mut_ptr().cast(), 4), 2);
            assert_eq!(&buf[..2], b"89");
            assert_eq!(read_trampoline(c, buf.as_mut_ptr().cast(), 4), 0);
            assert!(seek_trampoline(c, -1) < 0);
            cancel_trampoline(c);
            assert_eq!(cancels.load(Ordering::SeqCst), 1);
            assert_eq!(read_trampoline(c, buf.as_mut_ptr().cast(), 4), -1);
            close_trampoline(c);
        }
    }

    #[test]
    fn a_panicking_opener_fails_the_open() {
        let opener: Opener = Box::new(|_| panic!("boom"));
        let (code, _) = open(&opener, "flox://x");
        assert_eq!(code, ffi::MPV_ERROR_LOADING_FAILED);
    }
}
