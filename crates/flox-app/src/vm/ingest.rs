//! Details ingest mode and the ingest dialogs (paste links, local files, 4KHDHub).
//!
//! Pure logic, as the Mac's `TitleView`, `PasteLinksView`, `LocalFilesView` and
//! `HdHubView`: the episode selection, the action bar, the dialog rows and the jobs
//! they queue. The shell (`app.rs`, `ingest_shell.rs`) runs the picker, the 4KHDHub
//! lookups and the queue, and writes the results to the Slint globals.

use std::collections::BTreeSet;
use std::path::PathBuf;

use flox_core::model::{EpisodeKey, MediaType, TmdbId};
use flox_rip::hub::{default_variant, skip_uploaded, HubFile, Variant};
use flox_rip::job::{Job, Source, Tag};
use flox_rip::names::{episode_from_name, natural_cmp, tag_from_name};

use crate::app::{qualities, LibraryView};
use crate::focus::{Zone, ZoneId};

// ---------------------------------------------------------------------------
// Zones. Mirror `PasteZones`, `FilesZones` and `HubZones` in ui/components/.

/// The action bar's buttons, left to right, in zone `details::INGEST`.
pub const VIDLINK: usize = 0;
pub const PASTE_LINKS: usize = 1;
pub const LOCAL_FILES: usize = 2;
pub const HUB: usize = 3;
pub const BAR_LEN: usize = 4;

/// Dialog rows get one zone per cell so text fields and buttons can share a row:
/// cell `col` of row `row` is zone `base + row * ROW_STRIDE + col`. Up and Down step
/// through the cells in reading order, like Tab.
pub const ROW_STRIDE: i32 = 4;
/// Rows a dialog can hold, which keeps the zone ranges apart.
pub const MAX_ROWS: usize = 200;

/// Paste links: episode field (TV), URL field, REMOVE.
pub const PASTE_ROWS: i32 = 2000;
/// Local files: episode field (TV), tag, REMOVE.
pub const FILES_ROWS: i32 = 3000;

pub const PASTE_FOOTER: ZoneId = ZoneId(80);
pub const FILES_FOOTER: ZoneId = ZoneId(83);
pub const HUB_URL: ZoneId = ZoneId(86);
pub const HUB_LOAD: ZoneId = ZoneId(87);
pub const HUB_VARIANTS: ZoneId = ZoneId(88);
pub const HUB_SKIP: ZoneId = ZoneId(89);
pub const HUB_FOOTER: ZoneId = ZoneId(90);

/// Columns of a dialog row.
pub const COL_EPISODE: usize = 0;
pub const COL_MAIN: usize = 1;
pub const COL_REMOVE: usize = 2;

/// The zone of one dialog cell.
pub fn cell_zone(base: i32, row: usize, col: usize) -> ZoneId {
    let row = i32::try_from(row.min(MAX_ROWS)).unwrap_or(0);
    let col = i32::try_from(col).unwrap_or(0);
    ZoneId(base + row * ROW_STRIDE + col)
}

/// The `(row, col)` of a dialog cell zone under `base`.
pub fn zone_cell(base: i32, zone: ZoneId) -> Option<(usize, usize)> {
    let offset = zone.0.checked_sub(base)?;
    let span = i32::try_from(MAX_ROWS).ok()? * ROW_STRIDE;
    if offset < 0 || offset >= span {
        return None;
    }
    let row = usize::try_from(offset / ROW_STRIDE).ok()?;
    let col = usize::try_from(offset % ROW_STRIDE).ok()?;
    Some((row, col))
}

/// True for the zones the ingest dialogs own.
pub fn dialog_zone(zone: ZoneId) -> bool {
    zone_cell(PASTE_ROWS, zone).is_some()
        || zone_cell(FILES_ROWS, zone).is_some()
        || [
            PASTE_FOOTER,
            FILES_FOOTER,
            HUB_URL,
            HUB_LOAD,
            HUB_VARIANTS,
            HUB_SKIP,
            HUB_FOOTER,
        ]
        .contains(&zone)
}

// ---------------------------------------------------------------------------
// What the dialogs work on

/// The title, season and episodes an ingest action applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub id: TmdbId,
    pub media: MediaType,
    pub title: String,
    /// The selected season; 0 for movies.
    pub season: u32,
    /// The episodes listed for the season.
    pub episodes: Vec<u32>,
    /// The selected episodes, ascending.
    pub selected: Vec<u32>,
}

impl Target {
    /// The library key of `episode` (ignored for movies).
    pub fn key(&self, episode: u32) -> EpisodeKey {
        match self.media {
            MediaType::Movie => EpisodeKey::movie(self.id),
            MediaType::Tv => EpisodeKey::episode(self.id, self.season, episode),
        }
    }

    /// The first episode the dialogs prefill: the lowest selected, else 1.
    pub fn start_episode(&self) -> u32 {
        self.selected.first().copied().unwrap_or(1)
    }

    /// The episodes a 4KHDHub download wants: the selection, else the whole season.
    pub fn wanted(&self) -> Vec<u32> {
        if self.selected.is_empty() {
            self.episodes.clone()
        } else {
            self.selected.clone()
        }
    }

    /// `Title · Season 2` or `Title`, the dialogs' subtitle.
    pub fn subtitle(&self) -> String {
        match self.media {
            MediaType::Tv => format!("{} · Season {}", self.title, self.season),
            MediaType::Movie => self.title.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Selection and the action bar

/// The selected episode numbers of the current season.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection(BTreeSet<u32>);

impl Selection {
    /// Flips `episode`; returns whether it is now selected.
    pub fn toggle(&mut self, episode: u32) -> bool {
        if self.0.remove(&episode) {
            false
        } else {
            self.0.insert(episode);
            true
        }
    }

    /// Selects every listed episode (A).
    pub fn select_all(&mut self, episodes: impl IntoIterator<Item = u32>) {
        self.0.extend(episodes);
    }

    /// Clears the selection (N, a new season, a closed Files or Hub dialog).
    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn contains(&self, episode: u32) -> bool {
        self.0.contains(&episode)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Ascending.
    pub fn sorted(&self) -> Vec<u32> {
        self.0.iter().copied().collect()
    }
}

/// How the action bar reads for the current title and selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bar {
    /// VIDLINK: always for a movie, only with a selection for TV.
    pub vidlink_enabled: bool,
    /// `4KHDHUB`, or `SEASON FROM 4KHDHUB` for TV with nothing selected.
    pub hub_label: &'static str,
    /// Off for TV while the season has no episodes.
    pub hub_enabled: bool,
    /// `3 SELECTED` for TV, empty for movies.
    pub selected: String,
}

pub fn bar(media: MediaType, selected: usize, episodes: usize) -> Bar {
    match media {
        MediaType::Movie => Bar {
            vidlink_enabled: true,
            hub_label: "4KHDHUB",
            hub_enabled: true,
            selected: String::new(),
        },
        MediaType::Tv => Bar {
            vidlink_enabled: selected > 0,
            hub_label: if selected == 0 {
                "SEASON FROM 4KHDHUB"
            } else {
                "4KHDHUB"
            },
            hub_enabled: episodes > 0,
            selected: format!("{selected} SELECTED"),
        },
    }
}

/// VIDLINK: page jobs for the selected episodes, or the movie.
pub fn page_jobs(t: &Target) -> Vec<Job> {
    match t.media {
        MediaType::Movie => vec![Job::new(t.key(0), &t.title, Source::Page, None)],
        MediaType::Tv => t
            .selected
            .iter()
            .map(|&e| Job::new(t.key(e), &t.title, Source::Page, None))
            .collect(),
    }
}

/// `QUEUE 3`.
pub fn queue_label(n: usize) -> String {
    format!("QUEUE {n}")
}

/// `3 QUEUED`, shown under the bar after an action queued jobs.
pub fn queued_note(n: usize) -> String {
    format!("{n} QUEUED")
}

/// An episode number typed into a field: a positive integer.
pub fn parse_episode(s: &str) -> Option<u32> {
    s.trim().parse::<u32>().ok().filter(|&e| e > 0)
}

/// The URL when it is an absolute http(s) URL with a host.
pub fn http_url(s: &str) -> Option<String> {
    let trimmed = s.trim();
    let url = url::Url::parse(trimmed).ok()?;
    let http = matches!(url.scheme(), "http" | "https");
    (http && url.host_str().is_some_and(|h| !h.is_empty())).then(|| trimmed.to_owned())
}

// ---------------------------------------------------------------------------
// Paste links

/// One row: episode number (TV) and the pasted URL, both as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRow {
    pub episode: String,
    pub url: String,
}

/// The paste links dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasteLinks {
    pub rows: Vec<LinkRow>,
}

impl PasteLinks {
    /// One empty row numbered `start` (the lowest selected episode, else 1).
    pub fn new(start: u32) -> Self {
        Self {
            rows: vec![LinkRow {
                episode: start.to_string(),
                url: String::new(),
            }],
        }
    }

    /// ADD FIELD: a row numbered one past the last row. Returns it, or `None` when the
    /// dialog is full.
    pub fn add_field(&mut self) -> Option<LinkRow> {
        if self.rows.len() >= MAX_ROWS {
            return None;
        }
        let next = self
            .rows
            .last()
            .and_then(|r| parse_episode(&r.episode))
            .unwrap_or(0)
            + 1;
        let row = LinkRow {
            episode: next.to_string(),
            url: String::new(),
        };
        self.rows.push(row.clone());
        Some(row)
    }

    /// The last row cannot be removed.
    pub fn can_remove(&self) -> bool {
        self.rows.len() > 1
    }

    pub fn remove(&mut self, index: usize) -> bool {
        if !self.can_remove() || index >= self.rows.len() {
            return false;
        }
        self.rows.remove(index);
        true
    }

    pub fn set_episode(&mut self, index: usize, text: &str) {
        if let Some(r) = self.rows.get_mut(index) {
            r.episode = text.to_owned();
        }
    }

    pub fn set_url(&mut self, index: usize, text: &str) {
        if let Some(r) = self.rows.get_mut(index) {
            r.url = text.to_owned();
        }
    }

    /// Link jobs for every row with an http(s) URL (and, for TV, an episode number).
    pub fn jobs(&self, t: &Target) -> Vec<Job> {
        self.rows
            .iter()
            .filter_map(|r| {
                let url = http_url(&r.url)?;
                let episode = match t.media {
                    MediaType::Movie => 0,
                    MediaType::Tv => parse_episode(&r.episode)?,
                };
                Some(Job::new(t.key(episode), &t.title, Source::Link(url), None))
            })
            .collect()
    }

    /// The zones: per row the episode field (TV), URL field and REMOVE, then the
    /// footer (ADD FIELD for TV, CANCEL, QUEUE N).
    pub fn zones(&self, media: MediaType) -> Vec<Zone> {
        let mut zones = Vec::new();
        for i in 0..self.rows.len() {
            if media == MediaType::Tv {
                zones.push(Zone::row(cell_zone(PASTE_ROWS, i, COL_EPISODE), 1).text());
            }
            zones.push(Zone::row(cell_zone(PASTE_ROWS, i, COL_MAIN), 1).text());
            let remove = usize::from(self.can_remove());
            zones.push(Zone::row(cell_zone(PASTE_ROWS, i, COL_REMOVE), remove));
        }
        zones.push(Zone::row(PASTE_FOOTER, paste_footer(media).len()));
        zones
    }
}

/// A footer button of the paste and files dialogs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Footer {
    AddField,
    ChooseFiles,
    Load,
    Cancel,
    Queue,
}

pub fn paste_footer(media: MediaType) -> Vec<Footer> {
    match media {
        MediaType::Tv => vec![Footer::AddField, Footer::Cancel, Footer::Queue],
        MediaType::Movie => vec![Footer::Cancel, Footer::Queue],
    }
}

pub fn files_footer() -> Vec<Footer> {
    vec![Footer::ChooseFiles, Footer::Cancel, Footer::Queue]
}

pub fn hub_footer() -> Vec<Footer> {
    vec![Footer::Cancel, Footer::Queue]
}

// ---------------------------------------------------------------------------
// Local files

/// One picked file with its episode (as typed) and dynamic range tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub path: PathBuf,
    pub name: String,
    pub episode: String,
    pub tag: Tag,
}

/// The local files dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFiles {
    pub media: MediaType,
    pub start: u32,
    pub rows: Vec<FileRow>,
}

/// `SDR`, `HDR` or `DV`.
pub fn tag_label(tag: Tag) -> &'static str {
    match tag {
        Tag::Sdr => "SDR",
        Tag::Hdr => "HDR",
        Tag::Dv => "DV",
    }
}

/// SDR → HDR → DV → SDR.
pub fn next_tag(tag: Tag) -> Tag {
    match tag {
        Tag::Sdr => Tag::Hdr,
        Tag::Hdr => Tag::Dv,
        Tag::Dv => Tag::Sdr,
    }
}

/// Shortens `name` to `max` characters by cutting out its middle.
pub fn middle_elide(name: &str, max: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max || max < 5 {
        return name.to_owned();
    }
    let keep = max - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

impl LocalFiles {
    pub fn new(media: MediaType, start: u32) -> Self {
        Self {
            media,
            start,
            rows: Vec::new(),
        }
    }

    /// Adds picked files in natural order, skipping ones already listed. Episodes come
    /// from the name, else follow on from the previous file (the first from the start
    /// episode); tags come from the name. A movie keeps only the last file.
    pub fn append(&mut self, paths: Vec<PathBuf>) {
        let name_of = |p: &PathBuf| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned())
        };
        let mut sorted = paths;
        sorted.sort_by(|a, b| natural_cmp(&name_of(a), &name_of(b)));
        let mut next = self
            .rows
            .iter()
            .filter_map(|r| parse_episode(&r.episode))
            .max()
            .unwrap_or(self.start.saturating_sub(1))
            + 1;
        for path in sorted {
            if self.rows.len() >= MAX_ROWS || self.rows.iter().any(|r| r.path == path) {
                continue;
            }
            let name = name_of(&path);
            let episode = episode_from_name(&name).unwrap_or(next);
            next = episode + 1;
            self.rows.push(FileRow {
                tag: tag_from_name(&name),
                episode: episode.to_string(),
                name,
                path,
            });
        }
        if self.media == MediaType::Movie && self.rows.len() > 1 {
            let last = self.rows.split_off(self.rows.len() - 1);
            self.rows = last;
        }
    }

    pub fn cycle_tag(&mut self, index: usize) -> Option<Tag> {
        let row = self.rows.get_mut(index)?;
        row.tag = next_tag(row.tag);
        Some(row.tag)
    }

    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.rows.len() {
            return false;
        }
        self.rows.remove(index);
        true
    }

    pub fn set_episode(&mut self, index: usize, text: &str) {
        if let Some(r) = self.rows.get_mut(index) {
            r.episode = text.to_owned();
        }
    }

    /// File jobs with their tag; TV rows need an episode number.
    pub fn jobs(&self, t: &Target) -> Vec<Job> {
        self.rows
            .iter()
            .filter_map(|r| {
                let episode = match t.media {
                    MediaType::Movie => 0,
                    MediaType::Tv => parse_episode(&r.episode)?,
                };
                Some(Job::new(
                    t.key(episode),
                    &t.title,
                    Source::File(r.path.clone()),
                    Some(r.tag),
                ))
            })
            .collect()
    }

    /// Per row the episode field (TV), the tag and REMOVE, then the footer.
    pub fn zones(&self) -> Vec<Zone> {
        let mut zones = Vec::new();
        for i in 0..self.rows.len() {
            if self.media == MediaType::Tv {
                zones.push(Zone::row(cell_zone(FILES_ROWS, i, COL_EPISODE), 1).text());
            }
            zones.push(Zone::row(cell_zone(FILES_ROWS, i, COL_MAIN), 1));
            zones.push(Zone::row(cell_zone(FILES_ROWS, i, COL_REMOVE), 1));
        }
        zones.push(Zone::row(FILES_FOOTER, files_footer().len()));
        zones
    }
}

// ---------------------------------------------------------------------------
// 4KHDHub

pub const HUB_SEARCHING: &str = "SEARCHING 4KHDHUB…";
pub const HUB_NOT_FOUND: &str = "NOT FOUND ON 4KHDHUB. PASTE THE TITLE'S PAGE URL:";
pub const HUB_SEARCH_FAILED: &str = "SEARCH FAILED. PASTE THE TITLE'S PAGE URL:";
pub const HUB_READ_FAILED: &str = "COULD NOT READ THE PAGE. PASTE ANOTHER URL:";

/// `READING <last path segment>…`.
pub fn hub_reading(page: &str) -> String {
    let last = url::Url::parse(page)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.rfind(|p| !p.is_empty()).map(str::to_owned))
        })
        .unwrap_or_else(|| page.to_owned());
    format!("READING {}…", last.to_uppercase())
}

/// `NO FILES FOR SEASON 2 ON THAT PAGE. PASTE ANOTHER URL:`.
pub fn hub_empty(media: MediaType, season: u32) -> String {
    let what = match media {
        MediaType::Tv => format!("SEASON {season}"),
        MediaType::Movie => "THIS TITLE".to_owned(),
    };
    format!("NO FILES FOR {what} ON THAT PAGE. PASTE ANOTHER URL:")
}

/// The page's host, shown under the list.
pub fn host(page: &str) -> String {
    url::Url::parse(page)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Bytes as gigabytes with one decimal (the site's sizes are binary).
pub fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

/// A variant row as the dialog shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubRow {
    pub label: String,
    /// The file name, for movies.
    pub name: String,
    /// `3 UPLOADED` (TV) or `UPLOADED` (movie), empty when nothing is.
    pub badge: String,
    /// `8 OF 10` for TV, empty for movies.
    pub count: String,
    /// True when the variant has every wanted episode (the count reads muted).
    pub complete: bool,
    /// Size of the wanted files.
    pub size: String,
}

/// The 4KHDHub dialog once a page is loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubPicker {
    pub media: MediaType,
    pub season: u32,
    /// Episodes asked for; empty for movies.
    pub wanted: Vec<u32>,
    pub page: String,
    /// The variants for the season (all of them for a movie), best first.
    pub variants: Vec<Variant>,
    pub chosen: Option<usize>,
    /// SKIP EPISODES ALREADY UPLOADED AT THIS QUALITY.
    pub skip_uploaded: bool,
}

impl HubPicker {
    /// Filters `all` to the target's season and preselects the default variant.
    pub fn new(t: &Target, page: String, all: Vec<Variant>) -> Self {
        let wanted = match t.media {
            MediaType::Tv => t.wanted(),
            MediaType::Movie => Vec::new(),
        };
        let variants: Vec<Variant> = all
            .into_iter()
            .filter(|v| t.media == MediaType::Movie || v.season == Some(t.season))
            .collect();
        let chosen = default_variant(&variants, &wanted);
        Self {
            media: t.media,
            season: t.season,
            wanted,
            page,
            variants,
            chosen,
            skip_uploaded: true,
        }
    }

    fn key(&self, t: &Target, f: &HubFile) -> EpisodeKey {
        t.key(f.episode.unwrap_or(0))
    }

    /// The variant's files for the wanted episodes (every file for a movie).
    pub fn wanted_files<'a>(&self, v: &'a Variant) -> Vec<&'a HubFile> {
        v.files
            .iter()
            .filter(|f| match self.media {
                MediaType::Movie => true,
                MediaType::Tv => f.episode.is_some_and(|e| self.wanted.contains(&e)),
            })
            .collect()
    }

    /// True when the channel already has this file's episode at the variant's quality.
    pub fn uploaded(&self, t: &Target, lib: &dyn LibraryView, v: &Variant, f: &HubFile) -> bool {
        skip_uploaded(v, &qualities(lib, self.key(t, f)))
    }

    pub fn row(&self, t: &Target, lib: &dyn LibraryView, v: &Variant) -> HubRow {
        let files = self.wanted_files(v);
        let have = files.iter().filter(|f| self.uploaded(t, lib, v, f)).count();
        let badge = match (self.media, have) {
            (_, 0) => String::new(),
            (MediaType::Movie, _) => "UPLOADED".to_owned(),
            (MediaType::Tv, n) => format!("{n} UPLOADED"),
        };
        let (count, complete) = match self.media {
            MediaType::Movie => (String::new(), true),
            MediaType::Tv => (
                format!("{} OF {}", files.len(), self.wanted.len()),
                files.len() == self.wanted.len(),
            ),
        };
        HubRow {
            label: v.label.clone(),
            name: match self.media {
                MediaType::Movie => v.files.first().map(|f| f.name.clone()).unwrap_or_default(),
                MediaType::Tv => String::new(),
            },
            badge,
            count,
            complete,
            size: gb(files.iter().map(|f| f.size_bytes).sum()),
        }
    }

    pub fn current(&self) -> Option<&Variant> {
        self.chosen.and_then(|i| self.variants.get(i))
    }

    /// The files QUEUE N would queue.
    pub fn queued<'a>(&'a self, t: &Target, lib: &dyn LibraryView) -> Vec<&'a HubFile> {
        let Some(v) = self.current() else {
            return Vec::new();
        };
        self.wanted_files(v)
            .into_iter()
            .filter(|f| !(self.skip_uploaded && self.uploaded(t, lib, v, f)))
            .collect()
    }

    /// `3 ALREADY UPLOADED AT THIS QUALITY`, or empty.
    pub fn status(&self, t: &Target, lib: &dyn LibraryView) -> String {
        let Some(v) = self.current() else {
            return String::new();
        };
        let have = self
            .wanted_files(v)
            .iter()
            .filter(|f| self.uploaded(t, lib, v, f))
            .count();
        match (self.media, have) {
            (_, 0) => String::new(),
            (MediaType::Movie, _) => "ALREADY UPLOADED AT THIS QUALITY".to_owned(),
            (MediaType::Tv, n) => format!("{n} ALREADY UPLOADED AT THIS QUALITY"),
        }
    }

    /// Hub jobs for the queued files, tagged with the variant's range.
    pub fn jobs(&self, t: &Target, lib: &dyn LibraryView) -> Vec<Job> {
        let Some(tag) = self.current().map(Variant::tag) else {
            return Vec::new();
        };
        self.queued(t, lib)
            .into_iter()
            .map(|f| {
                Job::new(
                    self.key(t, f),
                    &t.title,
                    Source::Hub {
                        url: f.link.clone(),
                        name: f.name.clone(),
                    },
                    Some(tag),
                )
            })
            .collect()
    }
}

/// The hub dialog's zones for its phase: the URL field and LOAD while nothing is
/// loaded, else the variants and the skip toggle; the footer always.
pub fn hub_zones(url_phase: bool, variants: usize) -> Vec<Zone> {
    vec![
        Zone::row(HUB_URL, usize::from(url_phase)).text(),
        Zone::row(HUB_LOAD, usize::from(url_phase)),
        Zone::list(HUB_VARIANTS, variants),
        Zone::row(HUB_SKIP, usize::from(variants > 0)),
        Zone::row(HUB_FOOTER, hub_footer().len()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{EmptyLibrary, Print};
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

    fn tv(selected: &[u32]) -> Target {
        Target {
            id: 7,
            media: MediaType::Tv,
            title: "Show".to_owned(),
            season: 2,
            episodes: (1..=5).collect(),
            selected: selected.to_vec(),
        }
    }

    fn movie() -> Target {
        Target {
            id: 9,
            media: MediaType::Movie,
            title: "Film".to_owned(),
            season: 0,
            episodes: Vec::new(),
            selected: Vec::new(),
        }
    }

    fn file(episode: Option<u32>, gib: u64) -> HubFile {
        HubFile {
            episode,
            name: format!("Show.S02E{:02}.mkv", episode.unwrap_or(0)),
            size_bytes: gib << 30,
            link: format!("https://hubcloud.test/{}", episode.unwrap_or(0)),
        }
    }

    fn variant(label: &str, season: u32, episodes: &[u32]) -> Variant {
        Variant::new(
            label.to_owned(),
            Some(season),
            episodes.iter().map(|&e| file(Some(e), 2)).collect(),
        )
    }

    #[test]
    fn selection_toggles_all_and_none() {
        let mut s = Selection::default();
        assert!(s.toggle(3));
        assert!(s.toggle(1));
        assert!(!s.toggle(3));
        assert_eq!(s.sorted(), vec![1]);
        s.select_all([4, 2, 1]);
        assert_eq!(s.sorted(), vec![1, 2, 4]);
        assert!(s.contains(4));
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn bar_rules() {
        let b = bar(MediaType::Tv, 0, 10);
        assert!(!b.vidlink_enabled);
        assert_eq!(b.hub_label, "SEASON FROM 4KHDHUB");
        assert_eq!(b.selected, "0 SELECTED");
        let b = bar(MediaType::Tv, 2, 10);
        assert!(b.vidlink_enabled);
        assert_eq!(b.hub_label, "4KHDHUB");
        assert!(!bar(MediaType::Tv, 0, 0).hub_enabled);
        let b = bar(MediaType::Movie, 0, 0);
        assert!(b.vidlink_enabled && b.hub_enabled);
        assert_eq!(b.hub_label, "4KHDHUB");
        assert_eq!(b.selected, "");
    }

    #[test]
    fn vidlink_queues_selected_pages() {
        let jobs = page_jobs(&tv(&[1, 4]));
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[1].key, EpisodeKey::episode(7, 2, 4));
        assert_eq!(jobs[1].source, Source::Page);
        assert_eq!(jobs[0].label(), "Show · S02 E01");
        let jobs = page_jobs(&movie());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].key, EpisodeKey::movie(9));
    }

    #[test]
    fn target_prefill_and_wanted() {
        assert_eq!(tv(&[3, 5]).start_episode(), 3);
        assert_eq!(tv(&[]).start_episode(), 1);
        assert_eq!(tv(&[]).wanted(), vec![1, 2, 3, 4, 5]);
        assert_eq!(tv(&[2]).wanted(), vec![2]);
        assert_eq!(tv(&[]).subtitle(), "Show · Season 2");
    }

    #[test]
    fn cell_zones_round_trip() {
        let z = cell_zone(PASTE_ROWS, 3, COL_REMOVE);
        assert_eq!(z, ZoneId(2014));
        assert_eq!(zone_cell(PASTE_ROWS, z), Some((3, COL_REMOVE)));
        assert_eq!(zone_cell(FILES_ROWS, z), None);
        assert!(dialog_zone(z));
        assert!(dialog_zone(HUB_FOOTER));
        assert!(!dialog_zone(ZoneId(20)));
    }

    #[test]
    fn paste_links_prefill_add_remove() {
        let mut p = PasteLinks::new(tv(&[4, 6]).start_episode());
        assert_eq!(p.rows[0].episode, "4");
        assert!(!p.can_remove());
        assert!(!p.remove(0));
        assert_eq!(p.add_field().map(|r| r.episode), Some("5".to_owned()));
        p.set_episode(1, "x");
        assert_eq!(p.add_field().map(|r| r.episode), Some("1".to_owned()));
        assert!(p.remove(1));
        assert_eq!(p.rows.len(), 2);
    }

    #[test]
    fn paste_links_queue_only_http_urls() {
        let t = tv(&[]);
        let mut p = PasteLinks::new(1);
        p.set_url(0, " https://cdn.test/e1.mkv ");
        p.add_field();
        p.set_url(1, "ftp://cdn.test/e2.mkv");
        p.add_field();
        p.set_url(2, "http://cdn.test/e3.mkv");
        p.add_field();
        p.set_url(3, "https://cdn.test/e4.mkv");
        p.set_episode(3, "");
        p.add_field();
        p.set_url(4, "not a url");
        let jobs = p.jobs(&t);
        assert_eq!(jobs.len(), 2);
        assert_eq!(
            jobs[0].source,
            Source::Link("https://cdn.test/e1.mkv".to_owned())
        );
        assert_eq!(jobs[1].key, EpisodeKey::episode(7, 2, 3));
        assert_eq!(queue_label(jobs.len()), "QUEUE 2");
        let zones = p.zones(MediaType::Tv);
        assert_eq!(zones.len(), 5 * 3 + 1);
        assert_eq!(PasteLinks::new(1).zones(MediaType::Movie).len(), 3);

        let mut m = PasteLinks::new(1);
        m.set_url(0, "https://cdn.test/film.mkv");
        m.set_episode(0, "");
        let jobs = m.jobs(&movie());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].key, EpisodeKey::movie(9));
    }

    #[test]
    fn local_files_parse_names_and_sort() {
        let mut f = LocalFiles::new(MediaType::Tv, 3);
        f.append(vec![
            PathBuf::from("/v/Show.S02E10.2160p.DV.mkv"),
            PathBuf::from("/v/extra 2.mkv"),
            PathBuf::from("/v/Show.S02E02.HDR10.mkv"),
            PathBuf::from("/v/extra 10.mkv"),
        ]);
        let rows: Vec<(&str, &str, Tag)> = f
            .rows
            .iter()
            .map(|r| (r.name.as_str(), r.episode.as_str(), r.tag))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("extra 2.mkv", "3", Tag::Sdr),
                ("extra 10.mkv", "4", Tag::Sdr),
                ("Show.S02E02.HDR10.mkv", "2", Tag::Hdr),
                ("Show.S02E10.2160p.DV.mkv", "10", Tag::Dv),
            ]
        );
        // Picking the same file again is ignored; new files continue after the highest.
        f.append(vec![
            PathBuf::from("/v/extra 2.mkv"),
            PathBuf::from("/v/bonus.mkv"),
        ]);
        assert_eq!(f.rows.len(), 5);
        assert_eq!(f.rows[4].episode, "11");
        assert_eq!(f.cycle_tag(0), Some(Tag::Hdr));
        assert_eq!(f.cycle_tag(0), Some(Tag::Dv));
        assert_eq!(f.cycle_tag(0), Some(Tag::Sdr));
        f.set_episode(1, "");
        let jobs = f.jobs(&tv(&[]));
        assert_eq!(jobs.len(), 4);
        assert_eq!(jobs[2].tag, Some(Tag::Dv));
        assert_eq!(
            jobs[2].source,
            Source::File(PathBuf::from("/v/Show.S02E10.2160p.DV.mkv"))
        );
        assert!(f.remove(0));
        assert_eq!(f.zones().len(), 4 * 3 + 1);
    }

    #[test]
    fn local_files_movie_keeps_one() {
        let mut f = LocalFiles::new(MediaType::Movie, 1);
        f.append(vec![
            PathBuf::from("/v/b.mkv"),
            PathBuf::from("/v/a.dolby.vision.mkv"),
        ]);
        assert_eq!(f.rows.len(), 1);
        assert_eq!(f.rows[0].name, "b.mkv");
        let jobs = f.jobs(&movie());
        assert_eq!(jobs[0].key, EpisodeKey::movie(9));
        assert_eq!(jobs[0].tag, Some(Tag::Sdr));
        assert_eq!(f.zones().len(), 3);
    }

    #[test]
    fn middle_elision() {
        assert_eq!(middle_elide("short.mkv", 20), "short.mkv");
        assert_eq!(middle_elide("abcdefghijkl", 7), "abc…jkl");
        assert_eq!(tag_label(Tag::Dv), "DV");
    }

    #[test]
    fn hub_filters_to_the_season_and_picks_a_complete_default() {
        let t = tv(&[2, 3]);
        let all = vec![
            variant("S02 SDR 2160p WEB-DL H265", 2, &[1, 2]),
            variant("S01 SDR 2160p WEB-DL H265", 1, &[1, 2, 3]),
            variant("S02 SDR 1080p WEB-DL H264", 2, &[1, 2, 3, 4]),
        ];
        let p = HubPicker::new(&t, "https://4khdhub.one/show-series-1/".into(), all);
        assert_eq!(p.variants.len(), 2);
        assert_eq!(p.chosen, Some(1), "the first variant with 2 and 3");
        let lib = EmptyLibrary;
        let row = p.row(&t, &lib, &p.variants[0]);
        assert_eq!(row.count, "1 OF 2");
        assert!(!row.complete);
        assert_eq!(row.size, "2.0 GB");
        assert_eq!(row.badge, "");
        let row = p.row(&t, &lib, &p.variants[1]);
        assert_eq!(row.count, "2 OF 2");
        assert!(row.complete);
        assert_eq!(row.size, "4.0 GB");
        assert_eq!(host(&p.page), "4khdhub.one");
    }

    #[test]
    fn hub_skips_uploaded_at_the_same_quality() {
        let t = tv(&[]);
        let lib = FakeLibrary::default()
            .with(EpisodeKey::episode(7, 2, 1), "1080p")
            .with(EpisodeKey::episode(7, 2, 2), "2160p DV");
        let all = vec![variant("S02 SDR 1080p WEB-DL", 2, &[1, 2, 3])];
        let mut p = HubPicker::new(&t, "https://4khdhub.one/x/".into(), all);
        assert_eq!(p.wanted, vec![1, 2, 3, 4, 5]);
        let row = p.row(&t, &lib, &p.variants[0]);
        assert_eq!(row.badge, "1 UPLOADED");
        assert_eq!(row.count, "3 OF 5");
        assert_eq!(p.queued(&t, &lib).len(), 2);
        assert_eq!(p.status(&t, &lib), "1 ALREADY UPLOADED AT THIS QUALITY");
        let jobs = p.jobs(&t, &lib);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].key, EpisodeKey::episode(7, 2, 2));
        assert_eq!(jobs[0].tag, Some(Tag::Sdr));
        assert!(matches!(&jobs[0].source, Source::Hub { url, .. } if url.ends_with("/2")));
        p.skip_uploaded = false;
        assert_eq!(p.jobs(&t, &lib).len(), 3);
    }

    #[test]
    fn hub_movie_rows() {
        let t = movie();
        let lib = FakeLibrary::default().with(EpisodeKey::movie(9), "2160p DV");
        let dv = Variant::new(
            "2160p DoVi REMUX".to_owned(),
            None,
            vec![HubFile {
                episode: None,
                name: "Film.2160p.DV.mkv".to_owned(),
                size_bytes: 40 << 30,
                link: "https://hubcloud.test/f".to_owned(),
            }],
        );
        let p = HubPicker::new(&t, "https://4khdhub.one/film-movie/".into(), vec![dv]);
        assert_eq!(p.chosen, Some(0));
        let row = p.row(&t, &lib, &p.variants[0]);
        assert_eq!(row.name, "Film.2160p.DV.mkv");
        assert_eq!(row.badge, "UPLOADED");
        assert_eq!(row.count, "");
        assert!(p.jobs(&t, &lib).is_empty());
        assert_eq!(p.status(&t, &lib), "ALREADY UPLOADED AT THIS QUALITY");
    }

    #[test]
    fn hub_status_lines() {
        assert_eq!(
            hub_reading("https://4khdhub.one/severance-series-123/"),
            "READING SEVERANCE-SERIES-123…"
        );
        assert_eq!(
            hub_empty(MediaType::Tv, 2),
            "NO FILES FOR SEASON 2 ON THAT PAGE. PASTE ANOTHER URL:"
        );
        assert_eq!(
            http_url("https://4khdhub.one/x"),
            Some("https://4khdhub.one/x".into())
        );
        assert_eq!(http_url("4khdhub.one/x"), None);
        assert_eq!(parse_episode(" 12 "), Some(12));
        assert_eq!(parse_episode("0"), None);
        let zones = hub_zones(true, 0);
        assert_eq!(zones[0].len(), 1);
        assert_eq!(zones[2].len(), 0);
    }
}
