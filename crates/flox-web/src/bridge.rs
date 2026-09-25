//! Messages posted by the page scripts (`{ "type": ..., "data": ... }`).
//!
//! `shim.js` gives `adblock.js` and `flox_nav.js` a `window.FloxBridge.onMessage`
//! that forwards a JSON string to `window.chrome.webview.postMessage`; `tap.js`
//! posts the same shape directly. Parsing follows Android `PlayerBridge.kt` and the
//! Mac `Sniffer.swift` (`FLOX_PLAYLIST`).

use flox_core::lang::LANGUAGE_NAMES;
use flox_core::sniff::{Caption, StreamKind};
use serde_json::{Map, Value};

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

/// Request headers a native player must not copy from the page (`NativePlayer.RESERVED_HEADERS`).
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "accept-encoding",
    "user-agent",
    "cookie",
];

/// Parses one message; unknown types (including `FLOX_API`) and malformed ones are `None`.
/// Headers are returned as the page sent them; pass them through [`filter_headers`] before use.
pub fn parse(json: &str) -> Option<BridgeMessage> {
    let mut msg: Value = serde_json::from_str(json).ok()?;
    // tolerate a message read with `WebMessageAsJson` (a JSON string holding the JSON)
    if let Value::String(inner) = &msg {
        msg = serde_json::from_str(inner).ok()?;
    }
    let obj = msg.as_object()?;
    let data = obj.get("data");
    match obj.get("type")?.as_str()? {
        "FLOX_MANIFEST" => manifest(data?.as_object()?),
        "FLOX_PLAYLIST" => playlist(data?.as_object()?),
        "FLOX_STREAM" => stream(data?.as_object()?),
        "FLOX_TICK" => tick(data?.as_object()?),
        "PLAYER_EVENT" => {
            let d = data?;
            d.is_object().then(|| BridgeMessage::PlayerEvent(d.clone()))
        }
        "MEDIA_DATA" => match data? {
            Value::Null => None,
            d => Some(BridgeMessage::MediaData(d.clone())),
        },
        _ => None,
    }
}

/// Drops host, content-length, connection, accept-encoding, user-agent and cookie.
pub fn filter_headers(h: &[(String, String)]) -> Vec<(String, String)> {
    h.iter()
        .filter(|(k, _)| {
            let k = k.trim();
            !RESERVED_HEADERS.iter().any(|r| k.eq_ignore_ascii_case(r))
        })
        .cloned()
        .collect()
}

fn str_field<'a>(d: &'a Map<String, Value>, key: &str) -> &'a str {
    d.get(key).and_then(Value::as_str).unwrap_or("")
}

fn http_url(d: &Map<String, Value>) -> Option<String> {
    let url = str_field(d, "url");
    url.starts_with("http").then(|| url.to_owned())
}

/// A `{ name: value }` object as pairs sorted by name; scalar values are stringified,
/// others skipped.
fn headers(d: &Map<String, Value>) -> Vec<(String, String)> {
    let Some(h) = d.get("headers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = h
        .iter()
        .filter_map(|(k, v)| {
            let v = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            Some((k.clone(), v))
        })
        .collect();
    out.sort();
    out
}

/// `adblock.js` sends `hls` or `dash`; anything else is guessed from the URL.
fn stream_kind(kind: &str, url: &str) -> StreamKind {
    match kind.to_ascii_lowercase().as_str() {
        "hls" => StreamKind::Hls,
        "dash" => StreamKind::Dash,
        "file" | "mp4" => StreamKind::File,
        _ => {
            let path = url
                .split(['?', '#'])
                .next()
                .unwrap_or(url)
                .to_ascii_lowercase();
            if path.ends_with(".mpd") {
                StreamKind::Dash
            } else if path.ends_with(".mp4") {
                StreamKind::File
            } else {
                StreamKind::Hls
            }
        }
    }
}

fn manifest(d: &Map<String, Value>) -> Option<BridgeMessage> {
    let url = http_url(d)?;
    Some(BridgeMessage::Manifest {
        kind: stream_kind(str_field(d, "kind"), &url),
        headers: headers(d),
        url,
    })
}

/// The tap script reports `stream.playlist` with its `type`, `playlistHeaders` and
/// `playbackMetadata`, or for a `qualities` source the best entry with `kind: "file"`
/// and `meta: { resolutions: ["<height>"], codecName: "" }`.
fn playlist(d: &Map<String, Value>) -> Option<BridgeMessage> {
    let url = http_url(d)?;
    let meta = match d.get("meta") {
        Some(m @ Value::Object(_)) => m.clone(),
        _ => Value::Object(Map::new()),
    };
    Some(BridgeMessage::Playlist {
        url,
        kind: str_field(d, "kind").to_owned(),
        headers: headers(d),
        meta,
    })
}

fn stream(d: &Map<String, Value>) -> Option<BridgeMessage> {
    let list = d.get("captions")?.as_array()?;
    let captions = list
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|c| {
            Some(Caption {
                url: http_url(c)?,
                language: caption_language(str_field(c, "language")),
                kind: str_field(c, "type").to_owned(),
            })
        })
        .collect();
    Some(BridgeMessage::Stream { captions })
}

fn tick(d: &Map<String, Value>) -> Option<BridgeMessage> {
    let num = |k: &str| d.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let flag = |k: &str, default: bool| d.get(k).and_then(Value::as_bool).unwrap_or(default);
    Some(BridgeMessage::Tick {
        current_time: num("currentTime"),
        duration: num("duration"),
        paused: flag("paused", true),
        ended: flag("ended", false),
    })
}

/// Extra English names seen on caption lists beyond the 13 settings languages.
const EXTRA_LANGUAGE_NAMES: &[(&str, &str)] = &[
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("sv", "Swedish"),
    ("da", "Danish"),
    ("no", "Norwegian"),
    ("nb", "Norwegian Bokmal"),
    ("fi", "Finnish"),
    ("el", "Greek"),
    ("he", "Hebrew"),
    ("hu", "Hungarian"),
    ("cs", "Czech"),
    ("ro", "Romanian"),
    ("bg", "Bulgarian"),
    ("hr", "Croatian"),
    ("sr", "Serbian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("uk", "Ukrainian"),
    ("fa", "Persian"),
    ("id", "Indonesian"),
    ("ms", "Malay"),
    ("th", "Thai"),
    ("vi", "Vietnamese"),
    ("bn", "Bengali"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("ur", "Urdu"),
    ("tl", "Tagalog"),
    ("et", "Estonian"),
    ("lv", "Latvian"),
    ("lt", "Lithuanian"),
    ("is", "Icelandic"),
    ("ca", "Catalan"),
    ("eu", "Basque"),
    ("gl", "Galician"),
];

/// `"Spanish"` → `"es"`, tolerant of qualifiers (`"English (SDH)"`, `"Portuguese - Brazil"`).
/// Stands in for `flox_core::lang::iso_from_english_name` until that lookup is filled in.
fn iso_from_english_name(name: &str) -> Option<&'static str> {
    let base = name
        .split(['(', '[', '-', ',', '/', '|'])
        .next()
        .unwrap_or(name)
        .trim();
    let words: Vec<&str> = base.split_whitespace().collect();
    let names = LANGUAGE_NAMES.iter().chain(EXTRA_LANGUAGE_NAMES);
    for (iso, english) in names.clone() {
        if base.eq_ignore_ascii_case(english) {
            return Some(*iso);
        }
    }
    // "Brazilian Portuguese", "English SDH": any single word that names a language
    names
        .into_iter()
        .find(|(_, english)| words.iter().any(|w| w.eq_ignore_ascii_case(english)))
        .map(|(iso, _)| *iso)
}

/// The ISO code for a caption label, else the trimmed label as the page gave it.
fn caption_language(label: &str) -> String {
    match iso_from_english_name(label) {
        Some(iso) => iso.to_owned(),
        None => label.trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn manifest() {
        let m = r#"{"type":"FLOX_MANIFEST","data":{"url":"https://cdn.example.com/hls/master.m3u8","kind":"hls","headers":{"Referer":"https://vidlink.pro/","X-Count":3}}}"#;
        assert_eq!(
            parse(m),
            Some(BridgeMessage::Manifest {
                url: "https://cdn.example.com/hls/master.m3u8".to_owned(),
                kind: StreamKind::Hls,
                headers: pairs(&[("Referer", "https://vidlink.pro/"), ("X-Count", "3")]),
            })
        );
        let dash = r#"{"type":"FLOX_MANIFEST","data":{"url":"https://c.example.com/v.mpd","kind":"dash"}}"#;
        assert!(matches!(
            parse(dash),
            Some(BridgeMessage::Manifest { kind: StreamKind::Dash, ref headers, .. }) if headers.is_empty()
        ));
        let guessed =
            r#"{"type":"FLOX_MANIFEST","data":{"url":"https://c.example.com/v.mpd?t=1"}}"#;
        assert!(matches!(
            parse(guessed),
            Some(BridgeMessage::Manifest {
                kind: StreamKind::Dash,
                ..
            })
        ));
        // Android drops manifests without an http URL
        let blob =
            r#"{"type":"FLOX_MANIFEST","data":{"url":"blob:https://vidlink.pro/x","kind":"hls"}}"#;
        assert_eq!(parse(blob), None);
        assert_eq!(parse(r#"{"type":"FLOX_MANIFEST"}"#), None);
    }

    #[test]
    fn playlist_from_stream_playlist() {
        let m = json!({
            "type": "FLOX_PLAYLIST",
            "data": {
                "url": "https://storm.example.com/proxy/master.m3u8",
                "kind": "hls",
                "headers": { "referer": "https://videostr.net/", "origin": "https://videostr.net" },
                "meta": { "resolutions": ["1080", "720"], "codecName": "h264" }
            }
        })
        .to_string();
        assert_eq!(
            parse(&m),
            Some(BridgeMessage::Playlist {
                url: "https://storm.example.com/proxy/master.m3u8".to_owned(),
                kind: "hls".to_owned(),
                headers: pairs(&[
                    ("origin", "https://videostr.net"),
                    ("referer", "https://videostr.net/")
                ]),
                meta: json!({ "resolutions": ["1080", "720"], "codecName": "h264" }),
            })
        );
    }

    #[test]
    fn playlist_from_qualities_is_a_file() {
        // what tap.js posts for a `stream.qualities` source: the tallest entry as a file
        let m = json!({
            "type": "FLOX_PLAYLIST",
            "data": {
                "url": "https://files.example.com/1080.mp4",
                "kind": "file",
                "headers": {},
                "meta": { "resolutions": ["1080"], "codecName": "" }
            }
        })
        .to_string();
        let Some(BridgeMessage::Playlist {
            url,
            kind,
            headers,
            meta,
        }) = parse(&m)
        else {
            panic!("not a playlist")
        };
        assert_eq!(url, "https://files.example.com/1080.mp4");
        assert_eq!(kind, "file");
        assert!(headers.is_empty());
        assert_eq!(meta["resolutions"][0], "1080");
        assert_eq!(meta["codecName"], "");

        // missing meta and kind default to empty, as on the Mac
        let bare =
            r#"{"type":"FLOX_PLAYLIST","data":{"url":"https://x.example.com/a.m3u8","meta":7}}"#;
        assert_eq!(
            parse(bare),
            Some(BridgeMessage::Playlist {
                url: "https://x.example.com/a.m3u8".to_owned(),
                kind: String::new(),
                headers: Vec::new(),
                meta: json!({}),
            })
        );
        assert_eq!(
            parse(r#"{"type":"FLOX_PLAYLIST","data":{"kind":"hls"}}"#),
            None
        );
    }

    #[test]
    fn stream_captions() {
        let m = json!({
            "type": "FLOX_STREAM",
            "data": { "captions": [
                { "url": "https://subs.example.com/en.vtt", "language": "English", "type": "vtt" },
                { "url": "https://subs.example.com/es.srt", "language": "Spanish", "type": "srt" },
                { "url": "https://subs.example.com/pb.vtt", "language": "Portuguese (Brazil)", "type": "vtt" },
                { "url": "https://subs.example.com/sdh.vtt", "language": "english sdh", "type": "vtt" },
                { "url": "https://subs.example.com/nl.vtt", "language": "Dutch", "type": "vtt" },
                { "url": "https://subs.example.com/x.vtt", "language": " Klingon ", "type": "vtt" },
                { "url": "https://subs.example.com/y.vtt" },
                { "url": "blob:https://vidlink.pro/z", "language": "English" },
                { "language": "French" },
                "junk"
            ] }
        })
        .to_string();
        let cap = |url: &str, language: &str, kind: &str| Caption {
            url: url.to_owned(),
            language: language.to_owned(),
            kind: kind.to_owned(),
        };
        assert_eq!(
            parse(&m),
            Some(BridgeMessage::Stream {
                captions: vec![
                    cap("https://subs.example.com/en.vtt", "en", "vtt"),
                    cap("https://subs.example.com/es.srt", "es", "srt"),
                    cap("https://subs.example.com/pb.vtt", "pt", "vtt"),
                    cap("https://subs.example.com/sdh.vtt", "en", "vtt"),
                    cap("https://subs.example.com/nl.vtt", "nl", "vtt"),
                    cap("https://subs.example.com/x.vtt", "Klingon", "vtt"),
                    cap("https://subs.example.com/y.vtt", "", ""),
                ]
            })
        );
        assert_eq!(parse(r#"{"type":"FLOX_STREAM","data":{}}"#), None);
    }

    #[test]
    fn tick() {
        let m = r#"{"type":"FLOX_TICK","data":{"currentTime":61.5,"duration":2700,"paused":false,"ended":false}}"#;
        assert_eq!(
            parse(m),
            Some(BridgeMessage::Tick {
                current_time: 61.5,
                duration: 2700.0,
                paused: false,
                ended: false
            })
        );
        // NaN duration serializes as null; Android defaults paused to true
        let early = r#"{"type":"FLOX_TICK","data":{"currentTime":0,"duration":null}}"#;
        assert_eq!(
            parse(early),
            Some(BridgeMessage::Tick {
                current_time: 0.0,
                duration: 0.0,
                paused: true,
                ended: false
            })
        );
        assert_eq!(parse(r#"{"type":"FLOX_TICK","data":null}"#), None);
    }

    #[test]
    fn player_event_and_media_data() {
        let ev = r#"{"type":"PLAYER_EVENT","data":{"event":"timeupdate","currentTime":12.3,"duration":100,"playing":true}}"#;
        assert_eq!(
            parse(ev),
            Some(BridgeMessage::PlayerEvent(json!({
                "event": "timeupdate", "currentTime": 12.3, "duration": 100, "playing": true
            })))
        );
        assert_eq!(parse(r#"{"type":"PLAYER_EVENT","data":"play"}"#), None);

        let md = r#"{"type":"MEDIA_DATA","data":{"t1399":{"progress":{"watched":120,"duration":3300},"last_season_watched":"1","last_episode_watched":"2"}}}"#;
        let Some(BridgeMessage::MediaData(d)) = parse(md) else {
            panic!("not media data")
        };
        assert_eq!(d["t1399"]["progress"]["watched"], 120);
        assert_eq!(parse(r#"{"type":"MEDIA_DATA","data":null}"#), None);
        assert_eq!(parse(r#"{"type":"MEDIA_DATA"}"#), None);
    }

    #[test]
    fn unknown_and_malformed() {
        assert_eq!(
            parse(r#"{"type":"FLOX_API","data":{"url":"https://vidlink.pro/api/b","body":"{}"}}"#),
            None
        );
        assert_eq!(parse(r#"{"type":"SOMETHING","data":{}}"#), None);
        assert_eq!(parse(r#"{"data":{}}"#), None);
        assert_eq!(parse("[1,2]"), None);
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn json_string_wrapped_message() {
        let inner = r#"{"type":"FLOX_TICK","data":{"currentTime":1,"duration":2,"paused":true,"ended":true}}"#;
        let wrapped = serde_json::to_string(inner).unwrap();
        assert_eq!(parse(&wrapped), parse(inner));
        assert!(parse(&wrapped).is_some());
    }

    #[test]
    fn header_filter() {
        let h = pairs(&[
            ("Host", "cdn.example.com"),
            ("Content-Length", "0"),
            ("CONNECTION", "keep-alive"),
            ("accept-encoding", "gzip"),
            ("User-Agent", "Mozilla/5.0"),
            ("Cookie", "a=b"),
            (" cookie ", "c=d"),
            ("Referer", "https://vidlink.pro/"),
            ("Origin", "https://vidlink.pro"),
            ("Accept", "*/*"),
            ("x-custom", "1"),
            ("Set-Cookie", "kept"),
        ]);
        assert_eq!(
            filter_headers(&h),
            pairs(&[
                ("Referer", "https://vidlink.pro/"),
                ("Origin", "https://vidlink.pro"),
                ("Accept", "*/*"),
                ("x-custom", "1"),
                ("Set-Cookie", "kept"),
            ])
        );
        assert!(filter_headers(&[]).is_empty());
    }

    #[test]
    fn language_names() {
        assert_eq!(iso_from_english_name("English"), Some("en"));
        assert_eq!(iso_from_english_name("english"), Some("en"));
        assert_eq!(iso_from_english_name("English (US)"), Some("en"));
        assert_eq!(iso_from_english_name("Chinese - Simplified"), Some("zh"));
        assert_eq!(iso_from_english_name("Brazilian Portuguese"), Some("pt"));
        assert_eq!(iso_from_english_name("Turkish"), Some("tr"));
        assert_eq!(iso_from_english_name("Norwegian Bokmal"), Some("nb"));
        assert_eq!(iso_from_english_name(""), None);
        assert_eq!(iso_from_english_name("Elvish"), None);
        for (iso, name) in LANGUAGE_NAMES {
            assert_eq!(iso_from_english_name(name), Some(*iso));
        }
    }
}
