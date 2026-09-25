//! The Library manager, as the Mac's `LibraryView`: every uploaded print grouped by
//! title, with DELETE (behind a confirm dialog on Windows) and REFRESH.
//!
//! Pure logic: grouping, row text, the state stamp and the confirm text. The shell
//! reads the channel, resolves the titles through the catalog and deletes.

use std::collections::HashMap;

use flox_core::fmt::bytes;
use flox_core::model::{EpisodeKey, MediaType, TmdbId};
use flox_td::library::Entry;

use crate::focus::{Zone, ZoneId};

// Mirrors `LibraryZones` in ui/screens/library.slint.
pub const REFRESH: ZoneId = ZoneId(32);
pub const ENTRIES: ZoneId = ZoneId(33);
/// The confirm dialog: CANCEL, DELETE.
pub const CONFIRM: ZoneId = ZoneId(34);
pub const CONFIRM_CANCEL: usize = 0;
pub const CONFIRM_DELETE: usize = 1;

pub const READING: &str = "READING THE CHANNEL";
pub const EMPTY: &str = "LIBRARY IS EMPTY";
pub const NOT_CONNECTED: &str = "CONNECT TELEGRAM";

/// Where the manager's data stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    /// Telegram is not set up or not signed in.
    NotConnected,
    Reading,
    Failed(String),
    Loaded,
}

/// The stamp shown in place of an empty list; empty when the list shows.
pub fn stamp(load: &Load, entries: usize) -> String {
    match load {
        Load::NotConnected => NOT_CONNECTED.to_owned(),
        Load::Failed(e) if entries == 0 => e.to_uppercase(),
        Load::Reading if entries == 0 => READING.to_owned(),
        Load::Loaded if entries == 0 => EMPTY.to_owned(),
        _ => String::new(),
    }
}

/// The footer line: the error after a failed refresh of a shown list, or
/// READING THE CHANNEL while a shown list refreshes.
pub fn status(load: &Load, entries: usize) -> String {
    match load {
        Load::Failed(e) if entries > 0 => e.to_uppercase(),
        Load::Reading if entries > 0 => READING.to_owned(),
        _ => String::new(),
    }
}

/// `S01 E05` or `MOVIE`.
pub fn key_label(key: EpisodeKey) -> String {
    match key.media {
        MediaType::Tv => format!("S{:02} E{:02}", key.season, key.episode),
        MediaType::Movie => "MOVIE".to_owned(),
    }
}

/// One print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryRow {
    /// Position in the flat list (the focus index).
    pub index: usize,
    /// `S01 E05` or `MOVIE`.
    pub key: String,
    /// `2160P DV HEVC`.
    pub quality: String,
    /// `42.0 GB`.
    pub size: String,
}

/// One title and its prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    pub rows: Vec<EntryRow>,
}

/// The name of a title: the resolved one, else `TMDB <id>`.
pub fn title_name(
    names: &HashMap<(TmdbId, MediaType), String>,
    id: TmdbId,
    media: MediaType,
) -> String {
    names
        .get(&(id, media))
        .filter(|n| !n.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| format!("TMDB {id}"))
}

/// Groups the prints by title (sorted by name), each sorted by season, episode and
/// label. Returns the groups and the prints in the same flat order as the row indices.
pub fn groups(
    entries: &[Entry],
    names: &HashMap<(TmdbId, MediaType), String>,
) -> (Vec<Group>, Vec<Entry>) {
    let mut by_title: HashMap<(TmdbId, MediaType), Vec<&Entry>> = HashMap::new();
    for e in entries {
        by_title
            .entry((e.key.tmdb, e.key.media))
            .or_default()
            .push(e);
    }
    let mut titles: Vec<((TmdbId, MediaType), String, Vec<&Entry>)> = by_title
        .into_iter()
        .map(|((id, media), mut list)| {
            list.sort_by_key(|e| (e.key.season, e.key.episode, e.label()));
            ((id, media), title_name(names, id, media), list)
        })
        .collect();
    titles.sort_by(|a, b| {
        a.1.to_lowercase()
            .cmp(&b.1.to_lowercase())
            .then_with(|| a.0 .0.cmp(&b.0 .0))
    });

    let mut flat = Vec::new();
    let mut out = Vec::new();
    for (_, title, list) in titles {
        let rows = list
            .into_iter()
            .map(|e| {
                flat.push(e.clone());
                EntryRow {
                    index: flat.len() - 1,
                    key: key_label(e.key),
                    quality: e.label().trim().to_uppercase(),
                    size: bytes(e.size),
                }
            })
            .collect();
        out.push(Group { title, rows });
    }
    (out, flat)
}

/// The confirm dialog's line: `Severance · S01 E05 · 2160P DV HEVC · 42.0 GB`.
pub fn confirm_text(title: &str, e: &Entry) -> String {
    format!(
        "{title} · {} · {} · {}",
        key_label(e.key),
        e.label().trim().to_uppercase(),
        bytes(e.size)
    )
}

/// REFRESH, then the prints.
pub fn zones(entries: usize) -> Vec<Zone> {
    vec![Zone::row(REFRESH, 1), Zone::list(ENTRIES, entries)]
}

/// The confirm dialog: CANCEL, DELETE (CANCEL focused first).
pub fn confirm_zones() -> Vec<Zone> {
    vec![Zone::row(CONFIRM, 2)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use flox_td::library::Part;

    fn entry(key: EpisodeKey, quality: &str, size: u64) -> Entry {
        Entry {
            key,
            quality: quality.to_owned(),
            codec: "hevc".to_owned(),
            parts: vec![Part {
                message_id: 1,
                file_id: 1,
                size,
            }],
            subtitle: None,
            size,
            newest_message_id: 1,
        }
    }

    #[test]
    fn groups_by_title_sorted() {
        let entries = vec![
            entry(EpisodeKey::episode(95396, 2, 1), "1080p", 2_000_000_000),
            entry(EpisodeKey::movie(693134), "2160p DV", 42_000_000_000),
            entry(EpisodeKey::episode(95396, 1, 5), "2160p DV", 9_000_000_000),
            entry(EpisodeKey::episode(95396, 1, 5), "1080p", 3_000_000_000),
            entry(EpisodeKey::movie(1), "720p", 500_000_000),
        ];
        let mut names = HashMap::new();
        names.insert((95396, MediaType::Tv), "Severance".to_owned());
        names.insert((693134, MediaType::Movie), "Dune: Part Two".to_owned());
        let (groups, flat) = groups(&entries, &names);
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, vec!["Dune: Part Two", "Severance", "TMDB 1"]);
        let sev = &groups[1];
        let keys: Vec<(&str, &str)> = sev
            .rows
            .iter()
            .map(|r| (r.key.as_str(), r.quality.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("S01 E05", "1080P HEVC"),
                ("S01 E05", "2160P DV HEVC"),
                ("S02 E01", "1080P HEVC")
            ]
        );
        assert_eq!(groups[0].rows[0].key, "MOVIE");
        assert_eq!(groups[0].rows[0].size, "42.0 GB");
        assert_eq!(flat.len(), 5);
        for g in &groups {
            for r in &g.rows {
                assert_eq!(key_label(flat[r.index].key), r.key);
            }
        }
        assert_eq!(
            confirm_text("Severance", &flat[1]),
            "Severance · S01 E05 · 1080P HEVC · 3.0 GB"
        );
    }

    #[test]
    fn a_movie_and_a_show_with_one_id_stay_apart() {
        let entries = vec![
            entry(EpisodeKey::movie(5), "1080p", 1),
            entry(EpisodeKey::episode(5, 1, 1), "1080p", 1),
        ];
        let (groups, _) = groups(&entries, &HashMap::new());
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.title == "TMDB 5"));
    }

    #[test]
    fn stamps_and_status() {
        assert_eq!(stamp(&Load::Reading, 0), READING);
        assert_eq!(stamp(&Load::Reading, 3), "");
        assert_eq!(status(&Load::Reading, 3), READING);
        assert_eq!(stamp(&Load::Loaded, 0), EMPTY);
        assert_eq!(stamp(&Load::Loaded, 2), "");
        assert_eq!(stamp(&Load::NotConnected, 0), NOT_CONNECTED);
        assert_eq!(stamp(&Load::Failed("timed out".into()), 0), "TIMED OUT");
        assert_eq!(status(&Load::Failed("timed out".into()), 1), "TIMED OUT");
        assert_eq!(status(&Load::Loaded, 1), "");
        assert_eq!(zones(4)[1].len(), 4);
        assert_eq!(confirm_zones()[0].len(), 2);
    }
}
