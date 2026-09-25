//! The mpv OpenGL render context drawn under the Slint scene.
//!
//! Lifecycle (plan section 2): create in Slint's `RenderingSetup` with the
//! window's `get_proc_address`, call [`GlRenderer::render`] in
//! `BeforeRendering` and [`GlRenderer::report_swap`] in `AfterRendering`, and
//! drop in `RenderingTeardown`. Dropping frees the render context before the
//! mpv core can be destroyed. All calls except `new`'s `on_update` must happen
//! on the thread that owns the OpenGL context.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use flox_core::error::{Error, Result};

use crate::ffi::{self, MpvOpenglFbo, MpvOpenglInitParams, MpvRenderContext, MpvRenderParam};
use crate::mpv::{Handle, Mpv};

/// Something that shows video frames (the underlay, or the child-HWND fallback).
pub trait VideoSurface {
    /// A new frame is ready; request a redraw.
    fn frame_ready(&self);
}

type UpdateFn = Box<dyn Fn() + Send + Sync>;

/// `mpv_render_context` with `MPV_RENDER_API_TYPE_OPENGL`.
pub struct GlRenderer {
    ctx: *mut MpvRenderContext,
    /// The update callback's context pointer targets this box; freed after the
    /// render context.
    _on_update: Box<UpdateFn>,
    /// Keeps the mpv core alive until the render context is freed.
    handle: Arc<Handle>,
}

impl std::fmt::Debug for GlRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlRenderer").finish_non_exhaustive()
    }
}

type ProcLookup<'a> = &'a dyn Fn(&CStr) -> *const c_void;

unsafe extern "C" fn get_proc_address_trampoline(
    ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if ctx.is_null() || name.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: `ctx` points at the `ProcLookup` on `GlRenderer::new`'s stack; mpv
        // only resolves GL functions inside mpv_render_context_create.
        let lookup = unsafe { &*(ctx as *const ProcLookup<'_>) };
        // SAFETY: mpv passes a NUL-terminated function name.
        let name = unsafe { CStr::from_ptr(name) };
        lookup(name) as *mut c_void
    }))
    .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        // SAFETY: `ctx` points at the boxed callback owned by the `GlRenderer`,
        // which clears the callback before dropping the box.
        let f = unsafe { &*(ctx as *const UpdateFn) };
        f();
    }));
}

impl GlRenderer {
    /// Creates the render context. `get_proc_address` is only used during this
    /// call. `on_update` runs on an mpv thread and must not call into mpv; it
    /// should just schedule a redraw.
    pub fn new(
        mpv: &Mpv,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        on_update: Box<dyn Fn() + Send + Sync>,
    ) -> Result<GlRenderer> {
        let handle = mpv.handle.clone();
        let lookup: ProcLookup<'_> = get_proc_address;
        let mut init = MpvOpenglInitParams {
            get_proc_address: Some(get_proc_address_trampoline),
            get_proc_address_ctx: &lookup as *const ProcLookup<'_> as *mut c_void,
        };
        let mut params = [
            MpvRenderParam {
                type_: ffi::MPV_RENDER_PARAM_API_TYPE,
                data: ffi::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            MpvRenderParam {
                type_: ffi::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut MpvOpenglInitParams as *mut c_void,
            },
            MpvRenderParam {
                type_: ffi::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut ctx: *mut MpvRenderContext = std::ptr::null_mut();
        // SAFETY: the parameter array is terminated by INVALID and every pointer in
        // it (`init`, `lookup`, the static API name) lives until the call returns.
        let code = unsafe {
            (handle.lib.render_context_create)(&mut ctx, handle.ptr, params.as_mut_ptr())
        };
        handle.check(code)?;
        if ctx.is_null() {
            return Err(Error::Mpv {
                code: ffi::MPV_ERROR_GENERIC,
                message: "mpv_render_context_create returned no context".to_owned(),
            });
        }
        let boxed: Box<UpdateFn> = Box::new(on_update);
        // SAFETY: valid context; the callback context is the heap box owned by the
        // returned renderer, which outlives the registration (see Drop).
        unsafe {
            (handle.lib.render_context_set_update_callback)(
                ctx,
                Some(update_trampoline),
                &*boxed as *const UpdateFn as *mut c_void,
            )
        };
        Ok(GlRenderer {
            ctx,
            _on_update: boxed,
            handle,
        })
    }

    /// Renders into framebuffer `fbo` (physical size, flipped Y).
    pub fn render(&self, fbo: i32, w: i32, h: i32) -> Result<()> {
        let lib = &self.handle.lib;
        // SAFETY: valid context, called on the render thread. Acknowledges any
        // pending update callback; the frame is drawn regardless.
        unsafe { (lib.render_context_update)(self.ctx) };
        let mut target = MpvOpenglFbo {
            fbo,
            w,
            h,
            internal_format: 0,
        };
        let mut flip: c_int = 1;
        let mut params = [
            MpvRenderParam {
                type_: ffi::MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut target as *mut MpvOpenglFbo as *mut c_void,
            },
            MpvRenderParam {
                type_: ffi::MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip as *mut c_int as *mut c_void,
            },
            MpvRenderParam {
                type_: ffi::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        // SAFETY: INVALID-terminated params pointing at locals alive for the call.
        let code = unsafe { (lib.render_context_render)(self.ctx, params.as_mut_ptr()) };
        lib.check(code)
    }

    /// `mpv_render_context_report_swap`.
    pub fn report_swap(&self) {
        // SAFETY: valid context, called on the render thread after the swap.
        unsafe { (self.handle.lib.render_context_report_swap)(self.ctx) };
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        let lib = &self.handle.lib;
        // SAFETY: valid context. Clearing the callback first guarantees it is not
        // running against a freed box; then the context is freed exactly once,
        // before `handle` (and possibly the core) is released.
        unsafe {
            (lib.render_context_set_update_callback)(self.ctx, None, std::ptr::null_mut());
            (lib.render_context_free)(self.ctx);
        }
    }
}
