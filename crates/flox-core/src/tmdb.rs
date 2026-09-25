//! TMDB v3 client with an in-memory LRU. Filled in by piece P3.

use std::time::Duration;

use crate::error::{Error, Result};
use crate::model::{EpisodeInfo, MediaType, TitleDetails, TitleSummary, TmdbId};
use crate::settings::SettingsStore;

/// REST base URL.
pub const API_BASE: &str = "https://api.themoviedb.org/3";
/// Image base URL; a size segment and the path follow.
pub const IMAGE_BASE: &str = "https://image.tmdb.org/t/p/";
/// Search keeps the top results by popularity.
pub const SEARCH_LIMIT: usize = 36;
/// Response cache capacity (entries keyed by URL).
pub const CACHE_ENTRIES: usize = 32;
/// TTL for trending and search lists.
pub const LIST_TTL: Duration = Duration::from_secs(60 * 60);
/// TTL for details and seasons.
pub const DETAIL_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Per-request timeout.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// TMDB image sizes: `w342` posters, `w300` stills, `w500` the details poster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageSize {
    W300,
    W342,
    W500,
}

impl ImageSize {
    /// The URL segment, such as `"w342"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ImageSize::W300 => "w300",
            ImageSize::W342 => "w342",
            ImageSize::W500 => "w500",
        }
    }
}

/// The TMDB client. The API key is read from the [`SettingsStore`] on every request.
pub struct Tmdb {
    _private: (),
}

impl Tmdb {
    /// Builds the HTTP client (gzip, 10 s timeout). Filled in by P3.
    pub fn new(_settings: SettingsStore) -> Result<Tmdb> {
        Err(Error::NotImplemented("flox_core::tmdb::Tmdb::new"))
    }

    /// `/trending/{movie|tv}/week`.
    pub async fn trending(&self, _media: MediaType) -> Result<Vec<TitleSummary>> {
        Err(Error::NotImplemented("flox_core::tmdb::Tmdb::trending"))
    }

    /// `/search/movie` + `/search/tv` merged by popularity, top 36.
    pub async fn search(&self, _query: &str) -> Result<Vec<TitleSummary>> {
        Err(Error::NotImplemented("flox_core::tmdb::Tmdb::search"))
    }

    /// `/{movie|tv}/{id}`.
    pub async fn details(&self, _media: MediaType, _id: TmdbId) -> Result<TitleDetails> {
        Err(Error::NotImplemented("flox_core::tmdb::Tmdb::details"))
    }

    /// `/tv/{id}/season/{n}`.
    pub async fn season(&self, _id: TmdbId, _season: u32) -> Result<Vec<EpisodeInfo>> {
        Err(Error::NotImplemented("flox_core::tmdb::Tmdb::season"))
    }

    /// The full image URL for a TMDB path such as `/abc.jpg`.
    pub fn image_url(path: &str, size: ImageSize) -> String {
        format!("{IMAGE_BASE}{}{path}", size.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_url_joins_size_and_path() {
        assert_eq!(
            Tmdb::image_url("/p.jpg", ImageSize::W342),
            "https://image.tmdb.org/t/p/w342/p.jpg"
        );
    }
}
