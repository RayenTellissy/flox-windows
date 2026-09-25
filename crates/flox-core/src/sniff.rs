//! The page-sniffing contract. `flox-web` implements [`Sniffer`] with WebView2;
//! `flox-rip` and `flox-app` consume it.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::model::{EpisodeKey, MediaType};

/// The VidLink host.
pub const VIDLINK_HOST: &str = "vidlink.pro";

/// Why a page is being sniffed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SniffMode {
    /// Resolve on the first manifest (45 s).
    Playback,
    /// Resolve shortly after `FLOX_PLAYLIST` (60 s), with the tap script injected.
    Rip,
}

/// A caption track offered by the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caption {
    pub url: String,
    /// ISO code when known, else the page's label.
    pub language: String,
    /// `"srt"`, `"vtt"`, ...
    pub kind: String,
}

/// The manifest type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamKind {
    Hls,
    Dash,
    File,
}

/// What a sniff found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SniffResult {
    pub url: String,
    pub kind: StreamKind,
    /// Request headers the page used for the manifest, already filtered.
    pub headers: Vec<(String, String)>,
    pub captions: Vec<Caption>,
}

/// Loads an embed page and reports its stream.
#[async_trait]
pub trait Sniffer: Send + Sync {
    async fn sniff(
        &self,
        page_url: &str,
        mode: SniffMode,
        cancel: CancellationToken,
    ) -> Result<SniffResult>;
}

/// The VidLink embed URL for an episode or movie. `start_at` is added only when above 0.
pub fn vidlink_url(key: EpisodeKey, start_at: Option<u32>) -> String {
    let base = match key.media {
        MediaType::Tv => format!(
            "https://{VIDLINK_HOST}/tv/{}/{}/{}",
            key.tmdb, key.season, key.episode
        ),
        MediaType::Movie => format!("https://{VIDLINK_HOST}/movie/{}", key.tmdb),
    };
    let start = match start_at {
        Some(s) if s > 0 => format!("&startAt={s}"),
        _ => String::new(),
    };
    format!("{base}?autoplay=true&primaryColor=fafafa&nextbutton=false{start}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tv_url_with_start() {
        assert_eq!(
            vidlink_url(EpisodeKey::episode(1399, 1, 3), Some(90)),
            "https://vidlink.pro/tv/1399/1/3?autoplay=true&primaryColor=fafafa&nextbutton=false&startAt=90"
        );
    }

    #[test]
    fn movie_url_without_start() {
        assert_eq!(
            vidlink_url(EpisodeKey::movie(27205), Some(0)),
            "https://vidlink.pro/movie/27205?autoplay=true&primaryColor=fafafa&nextbutton=false"
        );
    }
}
