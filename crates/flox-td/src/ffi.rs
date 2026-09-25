//! The four `td_*` entry points of `tdjson`, bound at runtime. Filled in by piece P4.

use std::path::Path;

use flox_core::error::{Error, Result};

/// A loaded `tdjson` library.
pub struct TdJson {
    _private: (),
}

impl TdJson {
    /// Loads the library and binds `td_create_client_id`, `td_send`, `td_receive`, `td_execute`.
    pub fn load(_path: &Path) -> Result<Self> {
        Err(Error::NotImplemented("flox_td::ffi::TdJson::load"))
    }

    /// `td_create_client_id`. Filled in by P4.
    #[allow(clippy::unimplemented)]
    pub fn create_client_id(&self) -> i32 {
        unimplemented!("flox_td::ffi::TdJson::create_client_id (P4)")
    }

    /// `td_send`. Filled in by P4.
    #[allow(clippy::unimplemented)]
    pub fn send(&self, _client: i32, _json: &str) {
        unimplemented!("flox_td::ffi::TdJson::send (P4)")
    }

    /// `td_receive`, blocking up to `timeout` seconds. Filled in by P4.
    #[allow(clippy::unimplemented)]
    pub fn receive(&self, _timeout: f64) -> Option<String> {
        unimplemented!("flox_td::ffi::TdJson::receive (P4)")
    }

    /// `td_execute` for synchronous requests. Filled in by P4.
    #[allow(clippy::unimplemented)]
    pub fn execute(&self, _json: &str) -> Option<String> {
        unimplemented!("flox_td::ffi::TdJson::execute (P4)")
    }
}
