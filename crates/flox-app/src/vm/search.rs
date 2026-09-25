//! Debounced search and the results grid.

use std::time::Duration;

use flox_core::model::TitleSummary;

use crate::focus::{grid_columns, Zone, ZoneId};

// Mirrors `SearchZones` in ui/screens/search.slint.
pub const INPUT: ZoneId = ZoneId(10);
pub const GRID: ZoneId = ZoneId(11);

/// Typing waits this long before searching; Enter searches at once.
pub const DEBOUNCE: Duration = Duration::from_millis(400);

/// Android's grid: `(width − 96) / 176` columns of 160 px posters 16 px apart.
pub fn columns(window_width: f32) -> usize {
    grid_columns(window_width - 96.0, 160.0, 16.0)
}

/// What to do after the query changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// The query is blank: clear the results and the stamp.
    Clear,
    /// Search `query` after `delay`, then call [`Debounce::fire`] with `generation`.
    Run {
        query: String,
        delay: Duration,
        generation: u64,
    },
}

/// Android's SearchActivity scheduling: every edit cancels the pending search, a
/// blank query clears at once, and a search whose query equals the last one run
/// does nothing (so Enter after the debounce already fired is free).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Debounce {
    generation: u64,
    last_query: String,
}

impl Debounce {
    /// The query text changed (`immediate` for Enter).
    pub fn edit(&mut self, raw: &str, immediate: bool) -> Plan {
        self.generation = self.generation.wrapping_add(1);
        let query = raw.trim();
        if query.is_empty() {
            self.last_query.clear();
            return Plan::Clear;
        }
        Plan::Run {
            query: query.to_owned(),
            delay: if immediate { Duration::ZERO } else { DEBOUNCE },
            generation: self.generation,
        }
    }

    /// The delay elapsed. True when the search should run now.
    pub fn fire(&mut self, generation: u64, query: &str) -> bool {
        if generation != self.generation || query == self.last_query {
            return false;
        }
        self.last_query = query.to_owned();
        true
    }

    /// True while no newer edit has happened since `generation` was planned, so its
    /// results may still be shown.
    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    /// Forgets everything (the screen was closed).
    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.last_query.clear();
    }
}

/// Merges movie and TV results by popularity, most popular first, keeping at most
/// `limit`. The sort is stable, so ties keep movies before TV.
pub fn merge_by_popularity(
    movies: Vec<TitleSummary>,
    tv: Vec<TitleSummary>,
    limit: usize,
) -> Vec<TitleSummary> {
    let mut merged = movies;
    merged.extend(tv);
    merged.retain(|t| t.poster_path.is_some());
    merged.sort_by(|a, b| b.popularity.total_cmp(&a.popularity));
    merged.truncate(limit);
    merged
}

/// The stamp over the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Results {
    /// Blank query.
    Idle,
    Loading,
    Found(usize),
    Failed,
}

impl Results {
    pub fn stamp(&self) -> Option<&'static str> {
        match self {
            Results::Idle | Results::Found(1..) => None,
            Results::Loading => Some("LOADING"),
            Results::Found(0) => Some("NO RESULTS"),
            Results::Failed => Some("REQUEST FAILED"),
        }
    }
}

/// What BACK does on the Search screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Back {
    /// From the grid: return focus to the input first.
    FocusInput,
    /// From the input: leave the screen.
    Leave,
}

pub fn back(in_grid: bool, results: usize) -> Back {
    if in_grid && results > 0 {
        Back::FocusInput
    } else {
        Back::Leave
    }
}

/// The input above the grid.
pub fn zones(results: usize, columns: usize) -> Vec<Zone> {
    vec![
        Zone::row(INPUT, 1).text(),
        Zone::grid(GRID, columns, results),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use flox_core::model::MediaType;

    fn title(id: u64, media: MediaType, popularity: f64) -> TitleSummary {
        TitleSummary {
            id,
            media,
            title: format!("t{id}"),
            year: None,
            poster_path: Some("/p.jpg".to_owned()),
            overview: String::new(),
            popularity,
        }
    }

    fn run(plan: Plan) -> (String, Duration, u64) {
        match plan {
            Plan::Run {
                query,
                delay,
                generation,
            } => (query, delay, generation),
            Plan::Clear => panic!("expected a search"),
        }
    }

    #[test]
    fn typing_debounces_and_enter_is_immediate() {
        let mut d = Debounce::default();
        let (q, delay, _) = run(d.edit("  dune ", false));
        assert_eq!(q, "dune");
        assert_eq!(delay, DEBOUNCE);
        let (_, delay, _) = run(d.edit("dune", true));
        assert_eq!(delay, Duration::ZERO);
    }

    #[test]
    fn a_newer_edit_cancels_the_pending_search() {
        let mut d = Debounce::default();
        let (q1, _, g1) = run(d.edit("du", false));
        let (q2, _, g2) = run(d.edit("dune", false));
        assert!(!d.fire(g1, &q1));
        assert!(d.fire(g2, &q2));
        assert!(d.is_current(g2));
        d.edit("dunes", false);
        assert!(!d.is_current(g2), "results of an old query are dropped");
    }

    #[test]
    fn the_same_query_runs_once() {
        let mut d = Debounce::default();
        let (q, _, g) = run(d.edit("dune", false));
        assert!(d.fire(g, &q));
        let (q, _, g) = run(d.edit("dune", true));
        assert!(!d.fire(g, &q), "Enter after the debounce fired is free");
    }

    #[test]
    fn blank_query_clears_and_forgets() {
        let mut d = Debounce::default();
        let (q, _, g) = run(d.edit("dune", false));
        assert!(d.fire(g, &q));
        assert_eq!(d.edit("   ", false), Plan::Clear);
        let (q, _, g) = run(d.edit("dune", false));
        assert!(d.fire(g, &q), "the same query runs again after clearing");
        d.reset();
        assert!(!d.is_current(g));
    }

    #[test]
    fn merge_sorts_by_popularity_and_caps() {
        let movies = vec![
            title(1, MediaType::Movie, 5.0),
            title(2, MediaType::Movie, 50.0),
        ];
        let mut tv = vec![title(3, MediaType::Tv, 20.0), title(4, MediaType::Tv, 5.0)];
        let mut posterless = title(5, MediaType::Tv, 99.0);
        posterless.poster_path = None;
        tv.push(posterless);
        let ids: Vec<u64> = merge_by_popularity(movies.clone(), tv.clone(), 36)
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, [2, 3, 1, 4], "stable: the movie wins the tie");
        assert_eq!(merge_by_popularity(movies, tv, 2).len(), 2);
    }

    #[test]
    fn stamps() {
        assert_eq!(Results::Idle.stamp(), None);
        assert_eq!(Results::Loading.stamp(), Some("LOADING"));
        assert_eq!(Results::Found(0).stamp(), Some("NO RESULTS"));
        assert_eq!(Results::Found(3).stamp(), None);
        assert_eq!(Results::Failed.stamp(), Some("REQUEST FAILED"));
    }

    #[test]
    fn back_from_grid_returns_to_input() {
        assert_eq!(back(true, 5), Back::FocusInput);
        assert_eq!(back(false, 5), Back::Leave);
        assert_eq!(back(true, 0), Back::Leave);
    }

    #[test]
    fn columns_match_android() {
        assert_eq!(columns(1280.0), 6);
        assert_eq!(columns(1920.0), 10);
        assert_eq!(columns(200.0), 1);
    }
}
