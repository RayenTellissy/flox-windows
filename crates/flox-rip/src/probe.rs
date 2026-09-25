//! ffprobe of the finished file, and the quality label. `ffprobe` is filled in by piece P7.

use std::path::Path;

use flox_core::error::{Error, Result};

use crate::job::Tag;

/// What ffprobe reports for the first video stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub codec: String,
    pub height: u32,
    pub duration: f64,
}

/// `ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,height:format=duration -of json`.
/// A duration of 1 s or less is "download produced no video".
pub async fn ffprobe(_ffprobe: &Path, _file: &Path) -> Result<Probe> {
    Err(Error::NotImplemented("flox_rip::probe::ffprobe"))
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
}
