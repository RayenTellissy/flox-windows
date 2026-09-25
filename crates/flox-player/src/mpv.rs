//! A safe mpv handle with properties, commands and an event channel. Filled in by piece P11.

use std::sync::Arc;

use flox_core::error::{Error, Result};

use crate::ffi::MpvLib;

/// mpv data formats for `observe`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    None,
    String,
    Flag,
    Int64,
    Double,
    Node,
}

/// A value that can be read from or written to an mpv property. P11 adds the conversions.
pub trait MpvValue: Sized + Send {}

impl MpvValue for bool {}
impl MpvValue for i64 {}
impl MpvValue for f64 {}
impl MpvValue for String {}
impl MpvValue for serde_json::Value {}

/// Events drained from `mpv_wait_event`.
#[derive(Clone, Debug, PartialEq)]
pub enum MpvEvent {
    StartFile,
    FileLoaded,
    PlaybackRestart,
    EndFile {
        reason: String,
        error: i32,
    },
    PropertyChange {
        name: String,
        value: serde_json::Value,
    },
    Idle,
    Shutdown,
}

/// One mpv instance; `mpv_terminate_destroy` on drop.
pub struct Mpv {
    _private: (),
}

impl Mpv {
    /// `mpv_create`, the options, `mpv_initialize`, and the event thread.
    pub fn new(_lib: Arc<MpvLib>, _opts: &[(&str, &str)]) -> Result<Mpv> {
        Err(Error::NotImplemented("flox_player::mpv::Mpv::new"))
    }

    /// `mpv_command`.
    pub fn command(&self, _args: &[&str]) -> Result<()> {
        Err(Error::NotImplemented("flox_player::mpv::Mpv::command"))
    }

    /// `mpv_set_property`.
    pub fn set_property<T: MpvValue>(&self, _name: &str, _v: T) -> Result<()> {
        Err(Error::NotImplemented("flox_player::mpv::Mpv::set_property"))
    }

    /// `mpv_get_property`.
    pub fn get_property<T: MpvValue>(&self, _name: &str) -> Result<T> {
        Err(Error::NotImplemented("flox_player::mpv::Mpv::get_property"))
    }

    /// `mpv_observe_property`.
    pub fn observe(&self, _name: &str, _fmt: Format) -> Result<()> {
        Err(Error::NotImplemented("flox_player::mpv::Mpv::observe"))
    }

    /// The event receiver (taken once). Filled in by P11.
    #[allow(clippy::unimplemented)]
    pub fn events(&self) -> tokio::sync::mpsc::Receiver<MpvEvent> {
        unimplemented!("flox_player::mpv::Mpv::events (P11)")
    }
}
