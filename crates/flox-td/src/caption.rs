//! The JSON caption on every library message, byte-compatible with the Mac app:
//! `{"codec":"hevc","e":3,"part":1,"parts":2,"quality":"1080p","s":1,"tmdb":1399,"type":"tv"}`.
//! Filled in by piece P5.

use flox_core::model::{MediaType, TmdbId};

/// A parsed caption. Movies have no season or episode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caption {
    pub tmdb: TmdbId,
    pub media: MediaType,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub quality: String,
    pub codec: String,
    pub part: u32,
    pub parts: u32,
}

/// Sorted keys, no spaces, `/` unescaped. Filled in by P5.
#[allow(clippy::unimplemented)]
pub fn encode(_c: &Caption) -> String {
    unimplemented!("flox_td::caption::encode (P5)")
}

/// Parses from the first `{`; tolerates missing `s`/`e`. Filled in by P5.
#[allow(clippy::unimplemented)]
pub fn parse(_text: &str) -> Option<Caption> {
    unimplemented!("flox_td::caption::parse (P5)")
}
