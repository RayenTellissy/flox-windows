//! The four `td_*` entry points of `tdjson`, bound at runtime with `libloading`.
//!
//! This is the only module with `unsafe` code in the crate.

use std::ffi::{c_char, c_double, c_int, CStr, CString};
use std::mem::ManuallyDrop;
use std::path::Path;

use flox_core::error::{Error, Result};

type CreateClientIdFn = unsafe extern "C" fn() -> c_int;
type SendFn = unsafe extern "C" fn(c_int, *const c_char);
type ReceiveFn = unsafe extern "C" fn(c_double) -> *const c_char;
type ExecuteFn = unsafe extern "C" fn(*const c_char) -> *const c_char;

/// A loaded `tdjson` library.
///
/// The library is never unloaded: TDLib keeps worker threads running for the
/// rest of the process once a client exists, so unloading it would pull code
/// out from under them.
pub struct TdJson {
    // Keeps the four function pointers below valid.
    _lib: ManuallyDrop<libloading::Library>,
    create_client_id: CreateClientIdFn,
    send: SendFn,
    receive: ReceiveFn,
    execute: ExecuteFn,
}

impl std::fmt::Debug for TdJson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TdJson").finish_non_exhaustive()
    }
}

/// Copies a symbol out of `lib` as a plain function pointer.
///
/// # Safety
///
/// `T` must be the exact C signature of the exported symbol `name`, and the
/// returned pointer must not outlive `lib`.
unsafe fn symbol<T: Copy>(lib: &libloading::Library, name: &[u8]) -> Result<T> {
    // SAFETY: the caller guarantees `T` matches the symbol's real signature.
    let sym = unsafe { lib.get::<T>(name) }.map_err(|e| {
        Error::Load(format!(
            "tdjson is missing {}: {e}",
            String::from_utf8_lossy(name.strip_suffix(b"\0").unwrap_or(name))
        ))
    })?;
    Ok(*sym)
}

/// Converts a C string returned by TDLib into an owned `String`.
///
/// # Safety
///
/// `ptr` must be null or point to a NUL-terminated string that stays valid for
/// the duration of this call.
unsafe fn owned(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null and NUL-terminated per the caller's guarantee.
    let s = unsafe { CStr::from_ptr(ptr) };
    Some(s.to_string_lossy().into_owned())
}

impl TdJson {
    /// Loads the library and binds `td_create_client_id`, `td_send`, `td_receive`, `td_execute`.
    pub fn load(path: &Path) -> Result<Self> {
        // SAFETY: loading a shared library runs its initialisers. tdjson's are
        // plain static constructors with no preconditions on the caller.
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|e| Error::Load(format!("{}: {e}", path.display())))?;
        // SAFETY: each type alias above is the signature declared in
        // td/telegram/td_json_client.h for the named function, and the pointers
        // are stored next to the (never unloaded) library that owns them.
        let (create_client_id, send, receive, execute) = unsafe {
            (
                symbol::<CreateClientIdFn>(&lib, b"td_create_client_id\0")?,
                symbol::<SendFn>(&lib, b"td_send\0")?,
                symbol::<ReceiveFn>(&lib, b"td_receive\0")?,
                symbol::<ExecuteFn>(&lib, b"td_execute\0")?,
            )
        };
        Ok(Self {
            _lib: ManuallyDrop::new(lib),
            create_client_id,
            send,
            receive,
            execute,
        })
    }

    /// `td_create_client_id`.
    pub fn create_client_id(&self) -> i32 {
        // SAFETY: takes no arguments and is safe to call from any thread.
        unsafe { (self.create_client_id)() }
    }

    /// `td_send`. The request is dropped (and logged) if it contains a NUL byte.
    pub fn send(&self, client: i32, json: &str) {
        let Ok(c) = CString::new(json) else {
            tracing::warn!("td_send: request contains a NUL byte, dropped");
            return;
        };
        // SAFETY: `c` is a valid NUL-terminated string for the whole call; TDLib
        // copies it before returning and allows calls from any thread.
        unsafe { (self.send)(client, c.as_ptr()) }
    }

    /// `td_receive`, blocking up to `timeout` seconds. Call from one thread only.
    pub fn receive(&self, timeout: f64) -> Option<String> {
        // SAFETY: TDLib requires td_receive not to be called concurrently; the
        // client calls it from its single receive thread. The returned string is
        // valid until the next td_receive call and is copied out immediately.
        unsafe { owned((self.receive)(timeout)) }
    }

    /// `td_execute` for synchronous requests.
    pub fn execute(&self, json: &str) -> Option<String> {
        let Ok(c) = CString::new(json) else {
            tracing::warn!("td_execute: request contains a NUL byte, dropped");
            return None;
        };
        // SAFETY: `c` is valid for the call. The result is valid until the next
        // td_execute call on this thread and is copied out before returning.
        unsafe { owned((self.execute)(c.as_ptr())) }
    }
}
