//! `--dev-fixtures`: an offline catalog, library and posters read from a JSON file,
//! used for snapshots and for running the shell without a TMDB key or Telegram.
//!
//! Posters and stills are synthesized (flat tonal fills, one tone per path), so no
//! image leaves the machine and the snapshots stay deterministic.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use flox_core::error::{Error, Result};
use flox_core::images::Rgba;
use flox_core::model::{EpisodeInfo, EpisodeKey, MediaType, TitleDetails, TitleSummary, TmdbId};
use flox_core::progress::ProgressRecord;
use flox_core::tmdb::{ImageSize, SEARCH_LIMIT};
use serde::Deserialize;

use crate::app::{Catalog, ImageSource, LibrarySource, LibraryView, Print};
use crate::vm::search::merge_by_popularity;

/// The fixture file shipped with the crate.
pub const DEFAULT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/browse.json");

/// One season's episodes.
#[derive(Clone, Debug, Deserialize)]
pub struct FixtureSeason {
    pub id: TmdbId,
    pub season: u32,
    pub episodes: Vec<EpisodeInfo>,
}

/// One uploaded print.
#[derive(Clone, Debug, Deserialize)]
pub struct FixturePrint {
    pub id: TmdbId,
    pub media: MediaType,
    #[serde(default)]
    pub season: u32,
    #[serde(default)]
    pub episode: u32,
    pub quality: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub message_id: i64,
}

impl FixturePrint {
    fn key(&self) -> EpisodeKey {
        match self.media {
            MediaType::Movie => EpisodeKey::movie(self.id),
            MediaType::Tv => EpisodeKey::episode(self.id, self.season, self.episode),
        }
    }
}

/// Everything the browse screens read.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Fixtures {
    pub trending_movie: Vec<TitleSummary>,
    pub trending_tv: Vec<TitleSummary>,
    pub details: Vec<TitleDetails>,
    pub seasons: Vec<FixtureSeason>,
    pub progress: Vec<ProgressRecord>,
    pub library: Vec<FixturePrint>,
}

impl Fixtures {
    pub fn load(path: &Path) -> Result<Fixtures> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Every distinct title the fixtures mention, trending first.
    fn titles(&self) -> Vec<TitleSummary> {
        let mut seen = std::collections::HashSet::new();
        self.trending_movie
            .iter()
            .chain(&self.trending_tv)
            .chain(self.details.iter().map(|d| &d.summary))
            .filter(|t| seen.insert((t.media, t.id)))
            .cloned()
            .collect()
    }
}

/// The TMDB stand-in.
pub struct FixtureCatalog(pub Arc<Fixtures>);

#[async_trait]
impl Catalog for FixtureCatalog {
    async fn trending(&self, media: MediaType) -> Result<Vec<TitleSummary>> {
        Ok(match media {
            MediaType::Movie => self.0.trending_movie.clone(),
            MediaType::Tv => self.0.trending_tv.clone(),
        })
    }

    async fn search(&self, query: &str) -> Result<Vec<TitleSummary>> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let (movies, tv): (Vec<_>, Vec<_>) = self
            .0
            .titles()
            .into_iter()
            .filter(|t| t.title.to_lowercase().contains(&needle))
            .partition(|t| t.media == MediaType::Movie);
        Ok(merge_by_popularity(movies, tv, SEARCH_LIMIT))
    }

    async fn details(&self, media: MediaType, id: TmdbId) -> Result<TitleDetails> {
        if let Some(d) = self
            .0
            .details
            .iter()
            .find(|d| d.summary.media == media && d.summary.id == id)
        {
            return Ok(d.clone());
        }
        self.0
            .titles()
            .into_iter()
            .find(|t| t.media == media && t.id == id)
            .map(|summary| TitleDetails {
                summary,
                runtime_min: None,
                seasons: Vec::new(),
            })
            .ok_or_else(|| Error::Tmdb(format!("no fixture for {media:?} {id}")))
    }

    async fn season(&self, id: TmdbId, season: u32) -> Result<Vec<EpisodeInfo>> {
        self.0
            .seasons
            .iter()
            .find(|s| s.id == id && s.season == season)
            .map(|s| s.episodes.clone())
            .ok_or_else(|| Error::Tmdb(format!("no fixture for season {season} of {id}")))
    }
}

/// The channel index stand-in: always signed in, never fails.
pub struct FixtureLibrary {
    prints: HashMap<EpisodeKey, Vec<Print>>,
    titles: Vec<(TmdbId, MediaType)>,
}

impl FixtureLibrary {
    pub fn new(fixtures: &Fixtures) -> Self {
        let mut prints: HashMap<EpisodeKey, Vec<Print>> = HashMap::new();
        let mut titles = Vec::new();
        for p in &fixtures.library {
            if !titles.contains(&(p.id, p.media)) {
                titles.push((p.id, p.media));
            }
            prints.entry(p.key()).or_default().push(Print {
                quality: p.quality.clone(),
                size: p.size,
                newest_message_id: p.message_id,
            });
        }
        Self { prints, titles }
    }
}

impl LibraryView for FixtureLibrary {
    fn titles(&self) -> Vec<(TmdbId, MediaType)> {
        self.titles.clone()
    }

    fn prints(&self, key: EpisodeKey) -> Vec<Print> {
        self.prints.get(&key).cloned().unwrap_or_default()
    }

    fn all_qualities(&self) -> Vec<String> {
        self.prints
            .values()
            .flatten()
            .map(|p| p.quality.clone())
            .collect()
    }
}

/// Serves one [`FixtureLibrary`] on every refresh.
pub struct FixtureLibrarySource(pub Arc<FixtureLibrary>);

#[async_trait]
impl LibrarySource for FixtureLibrarySource {
    async fn refresh(&self, _channel_title: &str) -> Result<Arc<dyn LibraryView>> {
        Ok(self.0.clone())
    }
}

/// An in-memory job list for the ingest screens without Telegram or ffmpeg: jobs
/// are listed and stay as they are (nothing runs).
pub struct FixtureQueue {
    views: tokio::sync::watch::Sender<Vec<flox_rip::job::JobView>>,
}

impl Default for FixtureQueue {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl FixtureQueue {
    /// A list seeded with `views` (any states).
    pub fn new(views: Vec<flox_rip::job::JobView>) -> Self {
        let (views, _) = tokio::sync::watch::channel(views);
        Self { views }
    }

    fn edit(&self, f: impl FnOnce(&mut Vec<flox_rip::job::JobView>)) {
        self.views.send_modify(f);
    }
}

impl crate::app::JobQueue for FixtureQueue {
    fn add(&self, jobs: Vec<flox_rip::job::Job>) {
        self.edit(|v| v.extend(jobs.into_iter().map(flox_rip::job::JobView::queued)));
    }

    fn cancel(&self, id: uuid::Uuid) {
        self.edit(|v| v.retain(|j| j.job.id != id));
    }

    fn retry(&self, id: uuid::Uuid) {
        self.edit(|v| {
            for j in v.iter_mut().filter(|j| j.job.id == id) {
                j.state = flox_rip::job::JobState::Queued;
                j.detail.clear();
                j.progress = None;
            }
        });
    }

    fn remove(&self, id: uuid::Uuid) {
        self.edit(|v| v.retain(|j| j.job.id != id));
    }

    fn clear_finished(&self) {
        self.edit(|v| {
            v.retain(|j| {
                !matches!(
                    j.state,
                    flox_rip::job::JobState::Done | flox_rip::job::JobState::Cancelled
                )
            })
        });
    }

    fn snapshot(&self) -> tokio::sync::watch::Receiver<Vec<flox_rip::job::JobView>> {
        self.views.subscribe()
    }
}

/// Synthesized posters and stills.
pub struct FixtureImages;

#[async_trait]
impl ImageSource for FixtureImages {
    async fn image(&self, path: &str, size: ImageSize) -> Result<Arc<Rgba>> {
        Ok(Arc::new(synthesize(path, size)))
    }
}

/// A flat fill with a darker lower third, its tone picked from the path.
pub fn synthesize(path: &str, size: ImageSize) -> Rgba {
    let (width, height) = match size {
        ImageSize::W342 => (160, 240),
        ImageSize::W300 => (224, 126),
        ImageSize::W500 => (280, 420),
    };
    let hash = path.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    let tone = 0x2a + (hash % 0x50) as u8;
    let shade = tone.saturating_sub(0x14);
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let v = if y * 3 >= height * 2 { shade } else { tone };
        for _ in 0..width {
            pixels.extend_from_slice(&[v, v, v, 0xff]);
        }
    }
    Rgba {
        width,
        height,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> Fixtures {
        match Fixtures::load(Path::new(DEFAULT_PATH)) {
            Ok(f) => f,
            Err(e) => panic!("fixture file: {e}"),
        }
    }

    #[test]
    fn fixture_file_parses() {
        let f = fixtures();
        assert!(f.trending_movie.len() >= 6);
        assert!(f.trending_tv.len() >= 6);
        assert!(!f.details.is_empty());
        assert!(!f.seasons.is_empty());
        assert!(!f.progress.is_empty());
        assert!(!f.library.is_empty());
    }

    #[test]
    fn search_matches_and_merges() {
        let catalog = FixtureCatalog(Arc::new(fixtures()));
        let found = futures::executor::block_on(catalog.search("the"));
        let found = match found {
            Ok(v) => v,
            Err(e) => panic!("{e}"),
        };
        assert!(!found.is_empty());
        assert!(found.windows(2).all(|w| w[0].popularity >= w[1].popularity));
        let none = futures::executor::block_on(catalog.search("zzzz"));
        assert!(matches!(none, Ok(v) if v.is_empty()));
    }

    #[test]
    fn library_groups_prints() {
        let lib = FixtureLibrary::new(&fixtures());
        assert!(!lib.titles().is_empty());
        let (id, media) = lib.titles()[0];
        let key = match media {
            MediaType::Movie => EpisodeKey::movie(id),
            MediaType::Tv => EpisodeKey::episode(id, 1, 1),
        };
        assert!(!lib.prints(key).is_empty());
    }

    #[test]
    fn synthesized_images_have_the_asked_size() {
        let img = synthesize("/a.jpg", ImageSize::W300);
        assert_eq!((img.width, img.height), (224, 126));
        assert_eq!(img.pixels.len(), 224 * 126 * 4);
    }
}
