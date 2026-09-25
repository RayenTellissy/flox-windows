//! The mpv client and render API symbols Flox uses, as a function-pointer table.
//!
//! The types below are hand-ported from mpv's `client.h`, `render.h`,
//! `render_gl.h` and `stream_cb.h` (client API 2.x). Those headers carry the
//! ISC licence:
//!
//! > Copyright (C) 2014 the mpv developers
//! >
//! > Permission to use, copy, modify, and/or distribute this software for any
//! > purpose with or without fee is hereby granted, provided that the above
//! > copyright notice and this permission notice appear in all copies.
//! >
//! > THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
//! > WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
//! > MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
//! > ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
//! > WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
//! > ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
//! > OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//!
//! The library is opened with `libloading` at runtime, so nothing links
//! against `mpv.lib`/`libmpv.dylib` at build time.

use std::ffi::{c_char, c_double, c_int, c_ulong, c_void, CStr};
use std::path::Path;
use std::sync::Arc;

use flox_core::error::{Error, Result};

// ---------------------------------------------------------------------------
// client.h
// ---------------------------------------------------------------------------

/// `mpv_handle` (opaque).
#[repr(C)]
pub struct MpvHandle {
    _opaque: [u8; 0],
}

/// The client API major version Flox is written against.
pub const CLIENT_API_MAJOR: c_ulong = 2;

// `mpv_error`
pub const MPV_ERROR_SUCCESS: c_int = 0;
pub const MPV_ERROR_PROPERTY_UNAVAILABLE: c_int = -10;
pub const MPV_ERROR_LOADING_FAILED: c_int = -13;
pub const MPV_ERROR_UNSUPPORTED: c_int = -18;
pub const MPV_ERROR_GENERIC: c_int = -20;

// `mpv_format`
pub const MPV_FORMAT_NONE: c_int = 0;
pub const MPV_FORMAT_STRING: c_int = 1;
pub const MPV_FORMAT_OSD_STRING: c_int = 2;
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_INT64: c_int = 4;
pub const MPV_FORMAT_DOUBLE: c_int = 5;
pub const MPV_FORMAT_NODE: c_int = 6;
pub const MPV_FORMAT_NODE_ARRAY: c_int = 7;
pub const MPV_FORMAT_NODE_MAP: c_int = 8;
pub const MPV_FORMAT_BYTE_ARRAY: c_int = 9;

/// `mpv_node.u`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union MpvNodeU {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: c_double,
    pub list: *mut MpvNodeList,
    pub ba: *mut MpvByteArray,
}

/// `mpv_node`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MpvNode {
    pub u: MpvNodeU,
    pub format: c_int,
}

impl MpvNode {
    /// A `MPV_FORMAT_NONE` node.
    pub const fn none() -> Self {
        Self {
            u: MpvNodeU { int64: 0 },
            format: MPV_FORMAT_NONE,
        }
    }
}

/// `mpv_node_list`.
#[repr(C)]
pub struct MpvNodeList {
    pub num: c_int,
    pub values: *mut MpvNode,
    pub keys: *mut *mut c_char,
}

/// `mpv_byte_array`.
#[repr(C)]
pub struct MpvByteArray {
    pub data: *mut c_void,
    pub size: usize,
}

// `mpv_event_id`
pub const MPV_EVENT_NONE: c_int = 0;
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
pub const MPV_EVENT_START_FILE: c_int = 6;
pub const MPV_EVENT_END_FILE: c_int = 7;
pub const MPV_EVENT_FILE_LOADED: c_int = 8;
pub const MPV_EVENT_IDLE: c_int = 11;
pub const MPV_EVENT_PLAYBACK_RESTART: c_int = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;

// `mpv_end_file_reason`
pub const MPV_END_FILE_REASON_EOF: c_int = 0;
pub const MPV_END_FILE_REASON_STOP: c_int = 2;
pub const MPV_END_FILE_REASON_QUIT: c_int = 3;
pub const MPV_END_FILE_REASON_ERROR: c_int = 4;
pub const MPV_END_FILE_REASON_REDIRECT: c_int = 5;

/// `mpv_event_property`.
#[repr(C)]
pub struct MpvEventProperty {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

/// `mpv_event_end_file`.
#[repr(C)]
pub struct MpvEventEndFile {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

/// `mpv_event`.
#[repr(C)]
pub struct MpvEventRaw {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

/// `void (*cb)(void *d)` for `mpv_set_wakeup_callback`.
pub type WakeupFn = unsafe extern "C" fn(d: *mut c_void);

// ---------------------------------------------------------------------------
// stream_cb.h
// ---------------------------------------------------------------------------

pub type StreamReadFn =
    unsafe extern "C" fn(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64;
pub type StreamSeekFn = unsafe extern "C" fn(cookie: *mut c_void, offset: i64) -> i64;
pub type StreamSizeFn = unsafe extern "C" fn(cookie: *mut c_void) -> i64;
pub type StreamCloseFn = unsafe extern "C" fn(cookie: *mut c_void);
pub type StreamCancelFn = unsafe extern "C" fn(cookie: *mut c_void);

/// `mpv_stream_cb_info`.
#[repr(C)]
pub struct MpvStreamCbInfo {
    pub cookie: *mut c_void,
    pub read_fn: Option<StreamReadFn>,
    pub seek_fn: Option<StreamSeekFn>,
    pub size_fn: Option<StreamSizeFn>,
    pub close_fn: Option<StreamCloseFn>,
    pub cancel_fn: Option<StreamCancelFn>,
}

/// `mpv_stream_cb_open_ro_fn`.
pub type StreamOpenFn = unsafe extern "C" fn(
    user_data: *mut c_void,
    uri: *mut c_char,
    info: *mut MpvStreamCbInfo,
) -> c_int;

// ---------------------------------------------------------------------------
// render.h / render_gl.h
// ---------------------------------------------------------------------------

/// `mpv_render_context` (opaque).
#[repr(C)]
pub struct MpvRenderContext {
    _opaque: [u8; 0],
}

// `mpv_render_param_type`
pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;

/// `MPV_RENDER_API_TYPE_OPENGL`.
pub const MPV_RENDER_API_TYPE_OPENGL: &CStr = c"opengl";

/// `mpv_render_param`.
#[repr(C)]
pub struct MpvRenderParam {
    pub type_: c_int,
    pub data: *mut c_void,
}

/// `get_proc_address` in `mpv_opengl_init_params`.
pub type GetProcAddressFn =
    unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void;

/// `mpv_opengl_init_params` (client API 2.x; `extra_exts` was removed in 2.0).
#[repr(C)]
pub struct MpvOpenglInitParams {
    pub get_proc_address: Option<GetProcAddressFn>,
    pub get_proc_address_ctx: *mut c_void,
}

/// `mpv_opengl_fbo`.
#[repr(C)]
pub struct MpvOpenglFbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

/// `mpv_render_update_fn`.
pub type RenderUpdateFn = unsafe extern "C" fn(cb_ctx: *mut c_void);

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// A loaded libmpv: the library plus every bound symbol.
///
/// The function pointers stay valid for as long as `_lib` is alive, which is
/// as long as this struct (callers hold it in an `Arc`).
pub struct MpvLib {
    pub client_api_version: unsafe extern "C" fn() -> c_ulong,
    pub error_string: unsafe extern "C" fn(error: c_int) -> *const c_char,
    pub free: unsafe extern "C" fn(data: *mut c_void),
    pub create: unsafe extern "C" fn() -> *mut MpvHandle,
    pub initialize: unsafe extern "C" fn(ctx: *mut MpvHandle) -> c_int,
    pub terminate_destroy: unsafe extern "C" fn(ctx: *mut MpvHandle),
    pub set_option_string: unsafe extern "C" fn(
        ctx: *mut MpvHandle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int,
    pub command: unsafe extern "C" fn(ctx: *mut MpvHandle, args: *mut *const c_char) -> c_int,
    pub set_property: unsafe extern "C" fn(
        ctx: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int,
    pub get_property: unsafe extern "C" fn(
        ctx: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int,
    pub free_node_contents: unsafe extern "C" fn(node: *mut MpvNode),
    pub observe_property: unsafe extern "C" fn(
        ctx: *mut MpvHandle,
        reply_userdata: u64,
        name: *const c_char,
        format: c_int,
    ) -> c_int,
    pub wait_event:
        unsafe extern "C" fn(ctx: *mut MpvHandle, timeout: c_double) -> *mut MpvEventRaw,
    pub wakeup: unsafe extern "C" fn(ctx: *mut MpvHandle),
    pub set_wakeup_callback:
        unsafe extern "C" fn(ctx: *mut MpvHandle, cb: Option<WakeupFn>, d: *mut c_void),
    pub stream_cb_add_ro: unsafe extern "C" fn(
        ctx: *mut MpvHandle,
        protocol: *const c_char,
        user_data: *mut c_void,
        open_fn: Option<StreamOpenFn>,
    ) -> c_int,
    pub render_context_create: unsafe extern "C" fn(
        res: *mut *mut MpvRenderContext,
        mpv: *mut MpvHandle,
        params: *mut MpvRenderParam,
    ) -> c_int,
    pub render_context_set_update_callback: unsafe extern "C" fn(
        ctx: *mut MpvRenderContext,
        callback: Option<RenderUpdateFn>,
        callback_ctx: *mut c_void,
    ),
    pub render_context_update: unsafe extern "C" fn(ctx: *mut MpvRenderContext) -> u64,
    pub render_context_render:
        unsafe extern "C" fn(ctx: *mut MpvRenderContext, params: *mut MpvRenderParam) -> c_int,
    pub render_context_report_swap: unsafe extern "C" fn(ctx: *mut MpvRenderContext),
    pub render_context_free: unsafe extern "C" fn(ctx: *mut MpvRenderContext),
    _lib: libloading::Library,
}

impl std::fmt::Debug for MpvLib {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MpvLib").finish_non_exhaustive()
    }
}

/// Copies one symbol out of `lib` as a plain function pointer.
///
/// # Safety
/// `T` must be the exact C function-pointer type of `name`.
unsafe fn sym<T: Copy>(lib: &libloading::Library, name: &str) -> Result<T> {
    // SAFETY: the caller guarantees `T` matches the symbol's C signature.
    let s = unsafe { lib.get::<T>(name.as_bytes()) }
        .map_err(|e| Error::Load(format!("{name}: {e}")))?;
    Ok(*s)
}

impl MpvLib {
    /// Loads the library and binds every symbol. Fails with [`Error::Load`] when the
    /// file is missing, a symbol is absent, or the client API major version is not 2.
    pub fn load(path: &Path) -> Result<Arc<MpvLib>> {
        // SAFETY: loading libmpv runs its static initialisers, which have no
        // preconditions beyond being a genuine libmpv build.
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|e| Error::Load(format!("{}: {e}", path.display())))?;
        // SAFETY: every type below is the hand-ported signature of the named symbol
        // from mpv's headers.
        let table = unsafe {
            MpvLib {
                client_api_version: sym(&lib, "mpv_client_api_version")?,
                error_string: sym(&lib, "mpv_error_string")?,
                free: sym(&lib, "mpv_free")?,
                create: sym(&lib, "mpv_create")?,
                initialize: sym(&lib, "mpv_initialize")?,
                terminate_destroy: sym(&lib, "mpv_terminate_destroy")?,
                set_option_string: sym(&lib, "mpv_set_option_string")?,
                command: sym(&lib, "mpv_command")?,
                set_property: sym(&lib, "mpv_set_property")?,
                get_property: sym(&lib, "mpv_get_property")?,
                free_node_contents: sym(&lib, "mpv_free_node_contents")?,
                observe_property: sym(&lib, "mpv_observe_property")?,
                wait_event: sym(&lib, "mpv_wait_event")?,
                wakeup: sym(&lib, "mpv_wakeup")?,
                set_wakeup_callback: sym(&lib, "mpv_set_wakeup_callback")?,
                stream_cb_add_ro: sym(&lib, "mpv_stream_cb_add_ro")?,
                render_context_create: sym(&lib, "mpv_render_context_create")?,
                render_context_set_update_callback: sym(
                    &lib,
                    "mpv_render_context_set_update_callback",
                )?,
                render_context_update: sym(&lib, "mpv_render_context_update")?,
                render_context_render: sym(&lib, "mpv_render_context_render")?,
                render_context_report_swap: sym(&lib, "mpv_render_context_report_swap")?,
                render_context_free: sym(&lib, "mpv_render_context_free")?,
                _lib: lib,
            }
        };
        let version = table.api_version();
        if version >> 16 != CLIENT_API_MAJOR {
            return Err(Error::Load(format!(
                "libmpv client API {}.{} is not supported (need {}.x)",
                version >> 16,
                version & 0xffff,
                CLIENT_API_MAJOR
            )));
        }
        Ok(Arc::new(table))
    }

    /// `mpv_client_api_version()`: major in the high 16 bits, minor in the low 16.
    pub fn api_version(&self) -> c_ulong {
        // SAFETY: takes no arguments and only returns a constant.
        unsafe { (self.client_api_version)() }
    }

    /// `mpv_error_string(code)`.
    pub fn error_message(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string accepts any int and returns a static string
        // (never NULL; "unknown error" for unknown codes).
        let p = unsafe { (self.error_string)(code) };
        if p.is_null() {
            return format!("error {code}");
        }
        // SAFETY: non-null, NUL-terminated static string owned by libmpv.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    /// Maps a status code to `Ok(())` or [`Error::Mpv`].
    pub fn check(&self, code: c_int) -> Result<()> {
        if code >= 0 {
            Ok(())
        } else {
            Err(Error::Mpv {
                code,
                message: self.error_message(code),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_library_is_a_load_error() {
        let err = MpvLib::load(Path::new("/nonexistent/libmpv-2.dll")).unwrap_err();
        assert!(matches!(err, Error::Load(_)), "{err:?}");
    }

    #[test]
    fn layouts_match_the_c_headers() {
        use std::mem::size_of;
        // 64-bit targets only (Windows x64 and Apple silicon).
        assert_eq!(size_of::<MpvNode>(), 16);
        assert_eq!(size_of::<MpvNodeList>(), 24);
        assert_eq!(size_of::<MpvEventRaw>(), 24);
        assert_eq!(size_of::<MpvEventProperty>(), 24);
        assert_eq!(size_of::<MpvEventEndFile>(), 32);
        assert_eq!(size_of::<MpvStreamCbInfo>(), 48);
        assert_eq!(size_of::<MpvRenderParam>(), 16);
        assert_eq!(size_of::<MpvOpenglInitParams>(), 16);
        assert_eq!(size_of::<MpvOpenglFbo>(), 16);
    }
}
