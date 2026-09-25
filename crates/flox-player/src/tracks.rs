//! Audio and subtitle choices built from mpv's `track-list`. Filled in by piece P11.

/// One distinct (language, codec, channels) audio choice; `ids` are the mpv track ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioOption {
    pub ids: Vec<i64>,
    pub lang: Option<String>,
    pub codec: String,
    pub channels: u32,
}

/// One subtitle track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubTrack {
    pub id: i64,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub codec: String,
    pub external: bool,
}

/// Audio choices, deduplicated by (lang, codec, channels). Filled in by P11.
#[allow(clippy::unimplemented)]
pub fn audio_options(_track_list: &serde_json::Value) -> Vec<AudioOption> {
    unimplemented!("flox_player::tracks::audio_options (P11)")
}

/// `"English · E-AC3 · 5.1"`. Filled in by P11.
#[allow(clippy::unimplemented)]
pub fn audio_label(_o: &AudioOption) -> String {
    unimplemented!("flox_player::tracks::audio_label (P11)")
}

/// Subtitle tracks in mpv order. Filled in by P11.
#[allow(clippy::unimplemented)]
pub fn subtitle_tracks(_track_list: &serde_json::Value) -> Vec<SubTrack> {
    unimplemented!("flox_player::tracks::subtitle_tracks (P11)")
}
