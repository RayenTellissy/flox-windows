//! Telegram through TDLib's JSON interface. `tdjson` is loaded at runtime with
//! `libloading`, so nothing links against it at build time.

pub mod auth;
pub mod caption;
pub mod chats;
pub mod client;
pub mod ffi;
pub mod library;
pub mod stream;
pub mod transport;
pub mod upload;
