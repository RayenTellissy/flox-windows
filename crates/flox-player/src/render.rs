//! The mpv OpenGL render context drawn under the Slint scene. Filled in by piece P11.

use std::ffi::{c_void, CStr};

use flox_core::error::{Error, Result};

use crate::mpv::Mpv;

/// Something that shows video frames (the underlay, or the child-HWND fallback).
pub trait VideoSurface {
    /// A new frame is ready; request a redraw.
    fn frame_ready(&self);
}

/// `mpv_render_context` with `MPV_RENDER_API_TYPE_OPENGL`.
pub struct GlRenderer {
    _private: (),
}

impl GlRenderer {
    /// Creates the render context. `on_update` runs on an mpv thread.
    pub fn new(
        _mpv: &Mpv,
        _get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        _on_update: Box<dyn Fn() + Send + Sync>,
    ) -> Result<GlRenderer> {
        Err(Error::NotImplemented(
            "flox_player::render::GlRenderer::new",
        ))
    }

    /// Renders into framebuffer `fbo` (physical size, flipped Y).
    pub fn render(&self, _fbo: i32, _w: i32, _h: i32) -> Result<()> {
        Err(Error::NotImplemented(
            "flox_player::render::GlRenderer::render",
        ))
    }

    /// `mpv_render_context_report_swap`. Filled in by P11.
    #[allow(clippy::unimplemented)]
    pub fn report_swap(&self) {
        unimplemented!("flox_player::render::GlRenderer::report_swap (P11)")
    }
}
