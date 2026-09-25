//! Messages posted by the page scripts (`{ "type": ..., "data": ... }`). Filled in by piece P12.

use flox_core::sniff::{Caption, StreamKind};

/// A parsed page message.
#[derive(Clone, Debug, PartialEq)]
pub enum BridgeMessage {
    /// `FLOX_MANIFEST`.
    Manifest {
        url: String,
        kind: StreamKind,
        headers: Vec<(String, String)>,
    },
    /// `FLOX_PLAYLIST` (tap script, rip mode). `kind` is the page's raw type string.
    Playlist {
        url: String,
        kind: String,
        headers: Vec<(String, String)>,
        meta: serde_json::Value,
    },
    /// `FLOX_STREAM`.
    Stream { captions: Vec<Caption> },
    /// `FLOX_TICK`, every 2 s from the page player.
    Tick {
        current_time: f64,
        duration: f64,
        paused: bool,
        ended: bool,
    },
    /// `PLAYER_EVENT`.
    PlayerEvent(serde_json::Value),
    /// `MEDIA_DATA`.
    MediaData(serde_json::Value),
}

/// Parses one message; unknown types are `None`. Filled in by P12.
#[allow(clippy::unimplemented)]
pub fn parse(_json: &str) -> Option<BridgeMessage> {
    unimplemented!("flox_web::bridge::parse (P12)")
}

/// Drops host, content-length, connection, accept-encoding, user-agent and cookie. Filled in by P12.
#[allow(clippy::unimplemented)]
pub fn filter_headers(_h: &[(String, String)]) -> Vec<(String, String)> {
    unimplemented!("flox_web::bridge::filter_headers (P12)")
}
