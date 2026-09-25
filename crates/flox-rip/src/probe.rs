//! ffprobe of the finished file, and the quality label.

use std::ffi::OsString;
use std::path::Path;

use flox_core::error::{Error, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::job::Tag;
use crate::process;

/// What ffprobe reports for the first video stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub codec: String,
    pub height: u32,
    pub duration: f64,
}

/// The ffprobe arguments, as on the Mac.
pub fn probe_args(file: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=codec_name,height:format=duration",
        "-of",
        "json",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(file.into());
    args
}

/// Reads ffprobe's JSON the way the Mac does: missing fields become `""` and `0`.
pub fn parse_probe(output: &str) -> Result<Probe> {
    let json = match (output.find('{'), output.rfind('}')) {
        (Some(s), Some(e)) if s < e => &output[s..=e],
        _ => return Err(Error::Tool("probe failed".to_string())),
    };
    let j: Value =
        serde_json::from_str(json).map_err(|e| Error::Tool(format!("probe failed: {e}")))?;
    let stream = j.get("streams").and_then(|s| s.get(0));
    let codec = stream
        .and_then(|s| s.get("codec_name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let height = stream
        .and_then(|s| s.get("height"))
        .and_then(Value::as_u64)
        .and_then(|h| u32::try_from(h).ok())
        .unwrap_or(0);
    let duration = j
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(Value::as_str)
        .and_then(|d| d.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    Ok(Probe {
        codec,
        height,
        duration,
    })
}

/// `ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,height:format=duration -of json`.
/// A duration of 1 s or less is "download produced no video".
pub async fn ffprobe(ffprobe: &Path, file: &Path) -> Result<Probe> {
    let mut output = String::new();
    process::run(
        ffprobe,
        &probe_args(file),
        |line| {
            output.push_str(line);
            output.push('\n')
        },
        CancellationToken::new(),
    )
    .await?;
    let probe = parse_probe(&output)?;
    if probe.duration <= 1.0 {
        return Err(Error::Other("download produced no video".to_string()));
    }
    Ok(probe)
}

/// `"<height>p"`, plus `" DV"` or `" HDR"` for those tags.
pub fn quality_label(height: u32, tag: Option<Tag>) -> String {
    let suffix = match tag {
        Some(Tag::Dv) => " DV",
        Some(Tag::Hdr) => " HDR",
        Some(Tag::Sdr) | None => "",
    };
    format!("{height}p{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(quality_label(2160, Some(Tag::Dv)), "2160p DV");
        assert_eq!(quality_label(2160, Some(Tag::Hdr)), "2160p HDR");
        assert_eq!(quality_label(1080, Some(Tag::Sdr)), "1080p");
        assert_eq!(quality_label(720, None), "720p");
    }

    #[test]
    fn parses_like_the_mac() {
        let out = r#"{ "programs": [], "streams": [ { "codec_name": "hevc", "height": 2160 } ],
            "format": { "duration": "2712.480000" } }"#;
        assert_eq!(
            parse_probe(out).ok(),
            Some(Probe {
                codec: "hevc".to_string(),
                height: 2160,
                duration: 2712.48
            })
        );
        let empty = r#"{ "streams": [], "format": {} }"#;
        assert_eq!(
            parse_probe(empty).ok(),
            Some(Probe {
                codec: String::new(),
                height: 0,
                duration: 0.0
            })
        );
        assert!(parse_probe("No such file or directory").is_err());
    }
}
