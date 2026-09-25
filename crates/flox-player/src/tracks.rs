//! Audio and subtitle choices built from mpv's `track-list`.

use serde_json::Value;

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

fn tracks_of<'a>(track_list: &'a Value, kind: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    track_list
        .as_array()
        .into_iter()
        .flatten()
        .filter(move |t| t.get("type").and_then(Value::as_str) == Some(kind))
}

fn str_field(t: &Value, key: &str) -> Option<String> {
    t.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The track language, with blank and `und` (undetermined) treated as unknown.
fn lang_field(t: &Value) -> Option<String> {
    str_field(t, "lang").filter(|l| !l.eq_ignore_ascii_case("und"))
}

fn channel_count(t: &Value) -> u32 {
    t.get("demux-channel-count")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// Audio choices, deduplicated by (lang, codec, channels), in `track-list` order.
pub fn audio_options(track_list: &Value) -> Vec<AudioOption> {
    let mut out: Vec<AudioOption> = Vec::new();
    for t in tracks_of(track_list, "audio") {
        let Some(id) = t.get("id").and_then(Value::as_i64) else {
            continue;
        };
        let lang = lang_field(t);
        let codec = str_field(t, "codec")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let channels = channel_count(t);
        match out
            .iter_mut()
            .find(|o| o.lang == lang && o.codec == codec && o.channels == channels)
        {
            Some(o) => o.ids.push(id),
            None => out.push(AudioOption {
                ids: vec![id],
                lang,
                codec,
                channels,
            }),
        }
    }
    out
}

/// Display name for an mpv codec id.
fn codec_name(codec: &str) -> String {
    match codec {
        "eac3" => "E-AC3".to_owned(),
        "ac3" => "AC3".to_owned(),
        "truehd" => "TRUEHD".to_owned(),
        "dts" => "DTS".to_owned(),
        "aac" => "AAC".to_owned(),
        "opus" => "OPUS".to_owned(),
        other => other.to_ascii_uppercase(),
    }
}

/// `2.0`, `5.1`, `7.1`; `None` when unknown.
fn channel_name(n: u32) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1.0".to_owned()),
        2 => Some("2.0".to_owned()),
        6 => Some("5.1".to_owned()),
        8 => Some("7.1".to_owned()),
        n => Some(format!("{n}ch")),
    }
}

/// English name for a two- or three-letter code ([`flox_core::lang::display_name`]); the
/// code itself when unknown (as Android's `Locale.getDisplayLanguage` falls back).
fn language_name(code: &str) -> String {
    flox_core::lang::display_name(code).map_or_else(|| code.to_owned(), str::to_owned)
}

/// `"English · E-AC3 · 5.1"`. Unknown parts are left out.
pub fn audio_label(o: &AudioOption) -> String {
    let parts: Vec<String> = [
        o.lang.as_deref().map(language_name),
        (!o.codec.is_empty()).then(|| codec_name(&o.codec)),
        channel_name(o.channels),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "Audio".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// Subtitle tracks in mpv order.
pub fn subtitle_tracks(track_list: &Value) -> Vec<SubTrack> {
    tracks_of(track_list, "sub")
        .filter_map(|t| {
            Some(SubTrack {
                id: t.get("id").and_then(Value::as_i64)?,
                lang: lang_field(t),
                title: str_field(t, "title"),
                codec: str_field(t, "codec").unwrap_or_default(),
                external: t.get("external").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Shaped like mpv's `track-list` for a remux with a DV video, several audio
    /// tracks (two identical E-AC3 5.1 English ones) and mixed subtitles.
    fn fixture() -> Value {
        json!([
            {"id": 1, "type": "video", "codec": "hevc", "selected": true},
            {"id": 1, "type": "audio", "lang": "eng", "codec": "eac3", "demux-channel-count": 6, "demux-channels": "5.1", "default": true},
            {"id": 2, "type": "audio", "lang": "eng", "codec": "eac3", "demux-channel-count": 6, "title": "Commentary-free"},
            {"id": 3, "type": "audio", "lang": "eng", "codec": "truehd", "demux-channel-count": 8},
            {"id": 4, "type": "audio", "lang": "spa", "codec": "ac3", "demux-channel-count": 6},
            {"id": 5, "type": "audio", "lang": "fre", "codec": "aac", "demux-channel-count": 2},
            {"id": 6, "type": "audio", "lang": "und", "codec": "opus", "demux-channel-count": 2},
            {"id": 7, "type": "audio", "lang": "jpn", "codec": "dts"},
            {"id": 8, "type": "audio", "lang": "nld", "codec": "flac", "demux-channel-count": 1},
            {"id": 1, "type": "sub", "lang": "eng", "codec": "subrip", "title": "SDH", "external": false},
            {"id": 2, "type": "sub", "codec": "hdmv_pgs_subtitle"},
            {"id": 3, "type": "sub", "lang": "en", "codec": "webvtt", "title": "English", "external": true, "external-filename": "/tmp/x.vtt"}
        ])
    }

    #[test]
    fn audio_options_dedupe_by_lang_codec_channels() {
        let opts = audio_options(&fixture());
        let ids: Vec<Vec<i64>> = opts.iter().map(|o| o.ids.clone()).collect();
        assert_eq!(
            ids,
            vec![
                vec![1, 2],
                vec![3],
                vec![4],
                vec![5],
                vec![6],
                vec![7],
                vec![8]
            ]
        );
        assert_eq!(opts[0].lang.as_deref(), Some("eng"));
        assert_eq!(opts[0].codec, "eac3");
        assert_eq!(opts[0].channels, 6);
        assert_eq!(opts[4].lang, None);
        assert_eq!(opts[5].channels, 0);
    }

    #[test]
    fn audio_labels() {
        let labels: Vec<String> = audio_options(&fixture()).iter().map(audio_label).collect();
        assert_eq!(
            labels,
            vec![
                "English · E-AC3 · 5.1",
                "English · TRUEHD · 7.1",
                "Spanish · AC3 · 5.1",
                "French · AAC · 2.0",
                "OPUS · 2.0",
                "Japanese · DTS",
                "Dutch · FLAC · 1.0",
            ]
        );
    }

    #[test]
    fn label_edge_cases() {
        let o = AudioOption {
            ids: vec![1],
            lang: Some("en".to_owned()),
            codec: "mp3".to_owned(),
            channels: 4,
        };
        assert_eq!(audio_label(&o), "English · MP3 · 4ch");
        let empty = AudioOption {
            ids: vec![1],
            lang: None,
            codec: String::new(),
            channels: 0,
        };
        assert_eq!(audio_label(&empty), "Audio");
        assert_eq!(language_name("pt-BR"), "Portuguese");
        assert_eq!(language_name("GER"), "German");
        assert_eq!(language_name("qaa"), "qaa", "unknown codes stay as given");
    }

    #[test]
    fn subtitle_tracks_in_order() {
        let subs = subtitle_tracks(&fixture());
        assert_eq!(
            subs,
            vec![
                SubTrack {
                    id: 1,
                    lang: Some("eng".to_owned()),
                    title: Some("SDH".to_owned()),
                    codec: "subrip".to_owned(),
                    external: false,
                },
                SubTrack {
                    id: 2,
                    lang: None,
                    title: None,
                    codec: "hdmv_pgs_subtitle".to_owned(),
                    external: false,
                },
                SubTrack {
                    id: 3,
                    lang: Some("en".to_owned()),
                    title: Some("English".to_owned()),
                    codec: "webvtt".to_owned(),
                    external: true,
                },
            ]
        );
    }

    #[test]
    fn non_arrays_give_nothing() {
        assert!(audio_options(&json!(null)).is_empty());
        assert!(subtitle_tracks(&json!({"type": "sub"})).is_empty());
    }
}
