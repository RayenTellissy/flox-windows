//! The mpv client and render API symbols Flox uses, as a function-pointer table
//! hand-ported from mpv's ISC-licensed headers. Filled in by piece P11.

use std::path::Path;
use std::sync::Arc;

use flox_core::error::{Error, Result};

/// A loaded libmpv.
pub struct MpvLib {
    _private: (),
}

impl MpvLib {
    /// Loads the library and binds every symbol.
    pub fn load(_path: &Path) -> Result<Arc<MpvLib>> {
        Err(Error::NotImplemented("flox_player::ffi::MpvLib::load"))
    }
}
