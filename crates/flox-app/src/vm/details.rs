//! Title header, seasons, episodes and play entry.
//!
//! Pure logic: the meta lines with library stamps, the PLAY / RESUME label, the
//! library-only filter, the initial season and the player requests. The shell
//! (`app.rs`) loads the data and writes it to `DetailsState`.

use flox_core::fmt::meta_line;
use flox_core::model::{EpisodeInfo, EpisodeKey, MediaType, SeasonInfo, TitleDetails, TmdbId};
use flox_core::progress::ProgressRecord;

use crate::app::{qualities, LibraryView};
use crate::focus::{Zone, ZoneId};
use crate::router::PlayRequest;

// Mirrors `DetailsZones` in ui/screens/details.slint.
pub const PLAY: ZoneId = ZoneId(20);
pub const SEASONS: ZoneId = ZoneId(21);
pub const EPISODES: ZoneId = ZoneId(22);
/// Reserved for the ingest action bar (piece P16c), declared after PLAY.
pub const INGEST: ZoneId = ZoneId(23);

/// The filled button's label and icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayLabel {
    pub text: String,
    /// True shows the continue icon instead of play.
    pub resume: bool,
}

/// Saved progress that counts for resuming (some time was watched).
fn resumable(progress: Option<&ProgressRecord>) -> Option<&ProgressRecord> {
    progress.filter(|p| p.watched > 0)
}

/// `PLAY`, `RESUME`, or `RESUME S2 E3` for TV.
pub fn play_label(media: MediaType, progress: Option<&ProgressRecord>) -> PlayLabel {
    match (resumable(progress), media) {
        (None, _) => PlayLabel {
            text: "PLAY".to_owned(),
            resume: false,
        },
        (Some(_), MediaType::Movie) => PlayLabel {
            text: "RESUME".to_owned(),
            resume: true,
        },
        (Some(p), MediaType::Tv) => PlayLabel {
            text: format!("RESUME S{} E{}", p.season, p.episode),
            resume: true,
        },
    }
}

/// `YEAR · TV|MOVIE · N MIN [· LIBRARY · 1080P, 2160P DV]`. The library stamp is
/// shown for movies; TV stamps sit on the episodes.
pub fn header_meta(d: &TitleDetails, lib: &dyn LibraryView) -> String {
    let stamps = match d.summary.media {
        MediaType::Movie => qualities(lib, EpisodeKey::movie(d.summary.id)),
        MediaType::Tv => Vec::new(),
    };
    meta_line(d.summary.year, d.summary.media, d.runtime_min, &stamps)
}

/// `E01 · 42 MIN [· LIBRARY · 1080P, 2160P DV]`.
pub fn episode_meta(e: &EpisodeInfo, library_qualities: &[String]) -> String {
    let mut parts = vec![format!("E{:02}", e.episode)];
    if let Some(m) = e.runtime_min {
        parts.push(format!("{m} MIN"));
    }
    let stamps: Vec<String> = library_qualities
        .iter()
        .map(|q| q.trim().to_uppercase())
        .filter(|q| !q.is_empty())
        .collect();
    if !stamps.is_empty() {
        parts.push(format!("LIBRARY · {}", stamps.join(", ")));
    }
    parts.join(" · ")
}

/// A season pill's label: `S1`.
pub fn season_label(number: u32) -> String {
    format!("S{number}")
}

/// True when any episode of `season` has a print.
fn season_in_library(id: TmdbId, season: &SeasonInfo, lib: &dyn LibraryView) -> bool {
    (1..=season.episode_count).any(|e| {
        !lib.prints(EpisodeKey::episode(id, season.number, e))
            .is_empty()
    })
}

/// The season pills: every season, or in library-only mode just the uploaded ones.
pub fn visible_seasons(
    d: &TitleDetails,
    lib: &dyn LibraryView,
    library_only: bool,
) -> Vec<SeasonInfo> {
    d.seasons
        .iter()
        .filter(|s| !library_only || season_in_library(d.summary.id, s, lib))
        .cloned()
        .collect()
}

/// The episode list: every episode, or in library-only mode just the uploaded ones.
pub fn visible_episodes(
    id: TmdbId,
    episodes: Vec<EpisodeInfo>,
    lib: &dyn LibraryView,
    library_only: bool,
) -> Vec<EpisodeInfo> {
    episodes
        .into_iter()
        .filter(|e| {
            !library_only
                || !lib
                    .prints(EpisodeKey::episode(id, e.season, e.episode))
                    .is_empty()
        })
        .collect()
}

/// The season selected on open: the saved one if it is listed, else the first.
pub fn initial_season(seasons: &[SeasonInfo], progress: Option<&ProgressRecord>) -> Option<u32> {
    let saved = progress.map(|p| p.season);
    seasons
        .iter()
        .find(|s| Some(s.number) == saved)
        .or_else(|| seasons.first())
        .map(|s| s.number)
}

/// PLAY / RESUME: the saved episode and position, else the start. For TV without
/// progress that is `first` (the first listed episode, which matters in library-only
/// mode) or S1 E1.
pub fn play_request(
    d: &TitleDetails,
    progress: Option<&ProgressRecord>,
    first: Option<(u32, u32)>,
    library_only: bool,
) -> PlayRequest {
    let id = d.summary.id;
    let saved = resumable(progress);
    let key = match d.summary.media {
        MediaType::Movie => EpisodeKey::movie(id),
        MediaType::Tv => match (saved, first) {
            (Some(p), _) => EpisodeKey::episode(id, p.season, p.episode),
            (None, Some((s, e))) => EpisodeKey::episode(id, s, e),
            (None, None) => EpisodeKey::episode(id, 1, 1),
        },
    };
    PlayRequest {
        key,
        start_at: saved.map(|p| p.watched),
        library_only,
    }
}

/// An episode: resumes only when it is the saved episode.
pub fn episode_request(
    id: TmdbId,
    e: &EpisodeInfo,
    progress: Option<&ProgressRecord>,
    library_only: bool,
) -> PlayRequest {
    let start_at = resumable(progress)
        .filter(|p| p.season == e.season && p.episode == e.episode)
        .map(|p| p.watched);
    PlayRequest {
        key: EpisodeKey::episode(id, e.season, e.episode),
        start_at,
        library_only,
    }
}

/// The Details zones, top to bottom.
pub fn zones(loaded: bool, seasons: usize, episodes: usize) -> Vec<Zone> {
    vec![
        Zone::row(PLAY, usize::from(loaded)),
        Zone::row(SEASONS, seasons),
        Zone::list(EPISODES, episodes),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{EmptyLibrary, Print};
    use flox_core::model::TitleSummary;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeLibrary(HashMap<EpisodeKey, Vec<Print>>);

    impl FakeLibrary {
        fn with(mut self, key: EpisodeKey, quality: &str) -> Self {
            self.0.entry(key).or_default().push(Print {
                quality: quality.to_owned(),
                size: 1,
                newest_message_id: 1,
            });
            self
        }
    }

    impl LibraryView for FakeLibrary {
        fn titles(&self) -> Vec<(TmdbId, MediaType)> {
            Vec::new()
        }
        fn prints(&self, key: EpisodeKey) -> Vec<Print> {
            self.0.get(&key).cloned().unwrap_or_default()
        }
    }

    fn details(id: TmdbId, media: MediaType, seasons: &[(u32, u32)]) -> TitleDetails {
        TitleDetails {
            summary: TitleSummary {
                id,
                media,
                title: "T".to_owned(),
                year: Some(2021),
                poster_path: None,
                overview: String::new(),
                popularity: 0.0,
            },
            runtime_min: Some(148),
            seasons: seasons
                .iter()
                .map(|&(number, episode_count)| SeasonInfo {
                    number,
                    name: String::new(),
                    episode_count,
                })
                .collect(),
        }
    }

    fn progress(season: u32, episode: u32, watched: u32) -> ProgressRecord {
        ProgressRecord {
            id: 9,
            media: MediaType::Tv,
            title: "T".to_owned(),
            poster: None,
            watched,
            duration: 3000,
            season,
            episode,
            updated: 0,
        }
    }

    fn episode(season: u32, episode: u32) -> EpisodeInfo {
        EpisodeInfo {
            season,
            episode,
            name: format!("Episode {episode}"),
            overview: String::new(),
            still_path: None,
            runtime_min: Some(42),
        }
    }

    #[test]
    fn header_meta_stamps_movies() {
        let lib = FakeLibrary::default()
            .with(EpisodeKey::movie(9), "1080p")
            .with(EpisodeKey::movie(9), "2160p DV");
        assert_eq!(
            header_meta(&details(9, MediaType::Movie, &[]), &lib),
            "2021 · MOVIE · 148 MIN · LIBRARY · 1080P, 2160P DV"
        );
        assert_eq!(
            header_meta(&details(9, MediaType::Movie, &[]), &EmptyLibrary),
            "2021 · MOVIE · 148 MIN"
        );
        let tv_lib = FakeLibrary::default().with(EpisodeKey::episode(9, 1, 1), "1080p");
        let mut tv = details(9, MediaType::Tv, &[(1, 8)]);
        tv.runtime_min = None;
        tv.summary.year = None;
        assert_eq!(header_meta(&tv, &tv_lib), "TV");
    }

    #[test]
    fn episode_meta_lines() {
        assert_eq!(episode_meta(&episode(1, 1), &[]), "E01 · 42 MIN");
        let mut e = episode(2, 12);
        e.runtime_min = None;
        assert_eq!(
            episode_meta(&e, &["1080p".to_owned(), "2160p DV".to_owned()]),
            "E12 · LIBRARY · 1080P, 2160P DV"
        );
    }

    #[test]
    fn play_and_resume_labels() {
        assert_eq!(play_label(MediaType::Movie, None).text, "PLAY");
        let none = progress(1, 1, 0);
        let fresh = play_label(MediaType::Tv, Some(&none));
        assert_eq!(fresh.text, "PLAY");
        assert!(!fresh.resume);
        let p = progress(2, 3, 600);
        assert_eq!(play_label(MediaType::Movie, Some(&p)).text, "RESUME");
        let tv = play_label(MediaType::Tv, Some(&p));
        assert_eq!(tv.text, "RESUME S2 E3");
        assert!(tv.resume);
    }

    #[test]
    fn library_only_filters_seasons_and_episodes() {
        let lib = FakeLibrary::default()
            .with(EpisodeKey::episode(9, 2, 1), "1080p")
            .with(EpisodeKey::episode(9, 2, 3), "720p");
        let d = details(9, MediaType::Tv, &[(1, 4), (2, 4), (3, 2)]);
        let all = visible_seasons(&d, &lib, false);
        assert_eq!(all.len(), 3);
        let only: Vec<u32> = visible_seasons(&d, &lib, true)
            .iter()
            .map(|s| s.number)
            .collect();
        assert_eq!(only, [2]);
        let eps: Vec<EpisodeInfo> = (1..=4).map(|e| episode(2, e)).collect();
        assert_eq!(visible_episodes(9, eps.clone(), &lib, false).len(), 4);
        let kept: Vec<u32> = visible_episodes(9, eps, &lib, true)
            .iter()
            .map(|e| e.episode)
            .collect();
        assert_eq!(kept, [1, 3]);
    }

    #[test]
    fn initial_season_from_progress() {
        let d = details(9, MediaType::Tv, &[(1, 4), (2, 4)]);
        let seasons = visible_seasons(&d, &EmptyLibrary, false);
        assert_eq!(initial_season(&seasons, None), Some(1));
        assert_eq!(initial_season(&seasons, Some(&progress(2, 1, 5))), Some(2));
        assert_eq!(
            initial_season(&seasons, Some(&progress(7, 1, 5))),
            Some(1),
            "a season that is not listed falls back to the first"
        );
        assert_eq!(initial_season(&[], None), None);
    }

    #[test]
    fn play_requests() {
        let tv = details(9, MediaType::Tv, &[(1, 4)]);
        let p = progress(2, 3, 600);
        assert_eq!(
            play_request(&tv, Some(&p), Some((1, 1)), true),
            PlayRequest {
                key: EpisodeKey::episode(9, 2, 3),
                start_at: Some(600),
                library_only: true
            }
        );
        assert_eq!(
            play_request(&tv, None, Some((2, 5)), true).key,
            EpisodeKey::episode(9, 2, 5)
        );
        assert_eq!(
            play_request(&tv, None, None, false),
            PlayRequest {
                key: EpisodeKey::episode(9, 1, 1),
                start_at: None,
                library_only: false
            }
        );
        let movie = details(9, MediaType::Movie, &[]);
        let mut mp = progress(1, 1, 120);
        mp.media = MediaType::Movie;
        assert_eq!(
            play_request(&movie, Some(&mp), None, false),
            PlayRequest {
                key: EpisodeKey::movie(9),
                start_at: Some(120),
                library_only: false
            }
        );
    }

    #[test]
    fn episodes_resume_only_the_saved_one() {
        let p = progress(2, 3, 600);
        assert_eq!(
            episode_request(9, &episode(2, 3), Some(&p), false).start_at,
            Some(600)
        );
        assert_eq!(
            episode_request(9, &episode(2, 4), Some(&p), false).start_at,
            None
        );
        assert_eq!(
            episode_request(9, &episode(2, 4), None, true),
            PlayRequest {
                key: EpisodeKey::episode(9, 2, 4),
                start_at: None,
                library_only: true
            }
        );
    }

    #[test]
    fn zones_before_and_after_load() {
        let loading = zones(false, 0, 0);
        assert!(loading.iter().all(|z| z.is_empty()));
        let loaded = zones(true, 3, 10);
        assert_eq!(loaded[0].len(), 1);
        assert_eq!(loaded[2].len(), 10);
    }
}
