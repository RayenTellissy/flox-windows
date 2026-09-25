//! The JSON caption on every library message, byte-compatible with the Mac app:
//! `{"codec":"hevc","e":3,"part":1,"parts":2,"quality":"1080p","s":1,"tmdb":1399,"type":"tv"}`.
//!
//! The Mac writes it with `JSONEncoder` (`.sortedKeys`, `.withoutEscapingSlashes`),
//! and the Android app reads it from the first `{`. Encoding is hand-written so
//! the bytes never depend on a serializer's key order or escaping choices.

use std::fmt::Write as _;

use flox_core::model::{EpisodeKey, MediaType, TmdbId};
use serde_json::Value;

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

impl Caption {
    /// The caption for part `part` of `parts` of `key`; season and episode are
    /// set only for TV.
    pub fn new(key: EpisodeKey, quality: &str, codec: &str, part: u32, parts: u32) -> Self {
        let tv = key.media == MediaType::Tv;
        Self {
            tmdb: key.tmdb,
            media: key.media,
            season: tv.then_some(key.season),
            episode: tv.then_some(key.episode),
            quality: quality.to_owned(),
            codec: codec.to_owned(),
            part,
            parts,
        }
    }

    /// The library key. Movies always use season 0 and episode 0; a TV caption
    /// missing `s` or `e` reads them as 0, as the Android app does.
    pub fn key(&self) -> EpisodeKey {
        match self.media {
            MediaType::Movie => EpisodeKey::movie(self.tmdb),
            MediaType::Tv => EpisodeKey::episode(
                self.tmdb,
                self.season.unwrap_or(0),
                self.episode.unwrap_or(0),
            ),
        }
    }
}

/// Appends `s` as a JSON string the way Foundation's JSON writer does with
/// `.withoutEscapingSlashes`: `"` and `\` escaped, the short forms for
/// backspace, form feed, newline, carriage return and tab, other control
/// characters as `\u00xx`, and everything else (including `/` and non-ASCII)
/// written as is.
fn push_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Sorted keys, no spaces, `/` unescaped. `s` and `e` are omitted for movies
/// and whenever they are absent.
pub fn encode(c: &Caption) -> String {
    let tv = c.media == MediaType::Tv;
    let mut out = String::with_capacity(96);
    out.push_str("{\"codec\":");
    push_string(&mut out, &c.codec);
    if let (true, Some(e)) = (tv, c.episode) {
        let _ = write!(out, ",\"e\":{e}");
    }
    let _ = write!(out, ",\"part\":{},\"parts\":{}", c.part, c.parts);
    out.push_str(",\"quality\":");
    push_string(&mut out, &c.quality);
    if let (true, Some(s)) = (tv, c.season) {
        let _ = write!(out, ",\"s\":{s}");
    }
    let _ = write!(out, ",\"tmdb\":{}", c.tmdb);
    out.push_str(",\"type\":");
    push_string(&mut out, if tv { "tv" } else { "movie" });
    out.push('}');
    out
}

fn get_u32(o: &serde_json::Map<String, Value>, k: &str) -> Option<u32> {
    o.get(k)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

/// Parses from the first `{`; anything after the object is ignored.
///
/// `tmdb` and `type` are required. Missing `s`/`e` stay `None`, a missing
/// quality or codec is empty, and a missing `part`/`parts` is 1, matching the
/// Android reader.
pub fn parse(text: &str) -> Option<Caption> {
    let start = text.find('{')?;
    let v: Value = serde_json::Deserializer::from_str(&text[start..])
        .into_iter::<Value>()
        .next()?
        .ok()?;
    let o = v.as_object()?;
    let tmdb = o.get("tmdb").and_then(Value::as_u64)?;
    let media = match o.get("type").and_then(Value::as_str)? {
        "movie" => MediaType::Movie,
        "tv" => MediaType::Tv,
        _ => return None,
    };
    let text_of = |k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Some(Caption {
        tmdb,
        media,
        season: get_u32(o, "s"),
        episode: get_u32(o, "e"),
        quality: text_of("quality"),
        codec: text_of("codec"),
        part: get_u32(o, "part").unwrap_or(1),
        parts: get_u32(o, "parts").unwrap_or(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv() -> Caption {
        Caption {
            tmdb: 1399,
            media: MediaType::Tv,
            season: Some(1),
            episode: Some(3),
            quality: "1080p".into(),
            codec: "hevc".into(),
            part: 1,
            parts: 2,
        }
    }

    #[test]
    fn golden_tv() {
        assert_eq!(
            encode(&tv()),
            r#"{"codec":"hevc","e":3,"part":1,"parts":2,"quality":"1080p","s":1,"tmdb":1399,"type":"tv"}"#
        );
    }

    #[test]
    fn golden_movie_omits_season_and_episode() {
        let c = Caption::new(EpisodeKey::movie(603), "2160p DV", "hevc", 1, 1);
        assert_eq!(c.season, None);
        assert_eq!(
            encode(&c),
            r#"{"codec":"hevc","part":1,"parts":1,"quality":"2160p DV","tmdb":603,"type":"movie"}"#
        );
        // Stray season/episode on a movie are never written.
        let stray = Caption {
            season: Some(1),
            episode: Some(2),
            ..c
        };
        assert!(!encode(&stray).contains("\"s\""));
        assert!(!encode(&stray).contains("\"e\""));
    }

    #[test]
    fn new_matches_the_mac_initializer() {
        let c = Caption::new(EpisodeKey::episode(1399, 1, 3), "1080p", "hevc", 1, 2);
        assert_eq!(c, tv());
        assert_eq!(c.key(), EpisodeKey::episode(1399, 1, 3));
    }

    #[test]
    fn escaping_matches_foundation() {
        let c = Caption {
            quality: "a/b \"q\" \\ \n\t\u{1}é".into(),
            ..tv()
        };
        let s = encode(&c);
        assert!(s.contains(r#""quality":"a/b \"q\" \\ \n\t\u0001é""#), "{s}");
        assert_eq!(parse(&s).map(|p| p.quality), Some(c.quality));
    }

    #[test]
    fn round_trips() {
        let c = tv();
        assert_eq!(parse(&encode(&c)), Some(c));
        let m = Caption::new(EpisodeKey::movie(27205), "1080p HDR", "av1", 2, 3);
        assert_eq!(parse(&encode(&m)), Some(m));
    }

    #[test]
    fn parses_the_mac_caption() {
        let c = parse(
            r#"{"codec":"hevc","e":3,"part":1,"parts":2,"quality":"1080p","s":1,"tmdb":1399,"type":"tv"}"#,
        );
        assert_eq!(c, Some(tv()));
    }

    #[test]
    fn parses_the_android_caption_after_leading_text() {
        // Android reads from the first `{`, with keys in any order and
        // anything before or after the object ignored.
        let c = parse(
            "Game of Thrones S01E03\n{\"tmdb\":1399,\"type\":\"tv\",\"s\":1,\"e\":3,\
             \"quality\":\"1080p\",\"codec\":\"hevc\",\"part\":1,\"parts\":2}\n#flox",
        );
        assert_eq!(c, Some(tv()));
    }

    #[test]
    fn tolerates_missing_fields() {
        let c = parse(r#"{"tmdb":603,"type":"movie"}"#).unwrap();
        assert_eq!(c.season, None);
        assert_eq!(c.episode, None);
        assert_eq!(c.quality, "");
        assert_eq!((c.part, c.parts), (1, 1));
        assert_eq!(c.key(), EpisodeKey::movie(603));

        let t = parse(r#"{"tmdb":1,"type":"tv","quality":"720p"}"#).unwrap();
        assert_eq!(t.key(), EpisodeKey::episode(1, 0, 0));
    }

    #[test]
    fn rejects_non_captions() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("just a file"), None);
        assert_eq!(parse("{not json"), None);
        assert_eq!(parse(r#"{"type":"tv"}"#), None);
        assert_eq!(parse(r#"{"tmdb":1,"type":"anime"}"#), None);
        assert_eq!(parse(r#"{"tmdb":-1,"type":"tv"}"#), None);
    }
}
