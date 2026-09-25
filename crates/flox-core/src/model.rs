//! TMDB-shaped models shared across crates.

use serde::{Deserialize, Serialize};

/// A TMDB id.
pub type TmdbId = u64;

/// Movie or TV. Serialized as `"movie"` / `"tv"` (the caption and progress contract).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Movie,
    Tv,
}

/// One playable item. Movies use season 0 and episode 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EpisodeKey {
    pub tmdb: TmdbId,
    pub media: MediaType,
    pub season: u32,
    pub episode: u32,
}

impl EpisodeKey {
    /// The key for a movie (season and episode 0).
    pub fn movie(tmdb: TmdbId) -> Self {
        Self {
            tmdb,
            media: MediaType::Movie,
            season: 0,
            episode: 0,
        }
    }

    /// The key for a TV episode.
    pub fn episode(tmdb: TmdbId, season: u32, episode: u32) -> Self {
        Self {
            tmdb,
            media: MediaType::Tv,
            season,
            episode,
        }
    }
}

/// A title as it appears in a row or grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitleSummary {
    pub id: TmdbId,
    pub media: MediaType,
    pub title: String,
    pub year: Option<u16>,
    pub poster_path: Option<String>,
    pub overview: String,
    pub popularity: f64,
}

/// A season entry from `/tv/{id}` (season 0 is dropped).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeasonInfo {
    pub number: u32,
    pub name: String,
    pub episode_count: u32,
}

/// `/movie/{id}` or `/tv/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitleDetails {
    pub summary: TitleSummary,
    pub runtime_min: Option<u32>,
    pub seasons: Vec<SeasonInfo>,
}

/// One episode from `/tv/{id}/season/{n}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeInfo {
    pub season: u32,
    pub episode: u32,
    pub name: String,
    pub overview: String,
    pub still_path: Option<String>,
    pub runtime_min: Option<u32>,
}
