//! The Queue screen, as the Mac's `QueueView`.
//!
//! Pure logic: job rows (label, state, detail, progress, actions), the footer, the
//! top bar count and drain detection. Each job row is one focus zone holding its
//! action buttons, so Up/Down move between jobs and Left/Right between actions.

use flox_rip::job::{JobState, JobView};
use uuid::Uuid;

use crate::focus::{Focus, Zone, ZoneId};

// Mirrors `QueueZones` in ui/screens/queue.slint.
/// Job row `i` is zone `ROWS + i`.
pub const ROWS: i32 = 1000;
/// Rows the screen shows (and zones it allocates).
pub const MAX_ROWS: usize = 500;
pub const FOOTER: ZoneId = ZoneId(30);

pub const NOTHING_QUEUED: &str = "NOTHING QUEUED";
pub const WORKING: &str = "WORKING";
pub const IDLE: &str = "IDLE";
pub const CLEAR_FINISHED: &str = "CLEAR FINISHED";

/// A button on a job row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Retry,
    Remove,
    Cancel,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Retry => "RETRY",
            Action::Remove => "REMOVE",
            Action::Cancel => "CANCEL",
        }
    }
}

/// How the state text is coloured: Done in terminal green (the success stamp),
/// Failed in primary text (the palette has no red), the rest secondary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Success,
    Alert,
}

/// One job as the screen shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: Uuid,
    /// `Title · S01 E05` / `Title · MOVIE`.
    pub label: String,
    /// `DOWNLOADING`, `FAILED · NOT ENOUGH DISK SPACE`...
    pub state: String,
    pub tone: Tone,
    /// ffmpeg stats, `retrying: …` and the like, uppercased; may be empty.
    pub detail: String,
    /// 0..1 while downloading or uploading, else negative (hidden).
    pub progress: f32,
    pub actions: Vec<Action>,
}

/// RETRY + REMOVE for a failed job, REMOVE for a done or cancelled one, else CANCEL.
pub fn actions(state: &JobState) -> Vec<Action> {
    match state {
        JobState::Failed(_) => vec![Action::Retry, Action::Remove],
        JobState::Done | JobState::Cancelled => vec![Action::Remove],
        _ => vec![Action::Cancel],
    }
}

pub fn row(v: &JobView) -> Row {
    let tone = match v.state {
        JobState::Done => Tone::Success,
        JobState::Failed(_) => Tone::Alert,
        _ => Tone::Normal,
    };
    let progress = match (&v.state, v.progress) {
        (JobState::Downloading | JobState::Uploading, Some(p)) => p.clamp(0.0, 1.0),
        (JobState::Downloading | JobState::Uploading, None) => 0.0,
        _ => -1.0,
    };
    Row {
        id: v.job.id,
        label: v.job.label(),
        state: v.state.label().to_uppercase(),
        tone,
        detail: v.detail.trim().to_uppercase(),
        progress,
        actions: actions(&v.state),
    }
}

pub fn rows(views: &[JobView]) -> Vec<Row> {
    views.iter().take(MAX_ROWS).map(row).collect()
}

/// Jobs not finished yet: the top bar's `QUEUE (n)`.
pub fn pending(views: &[JobView]) -> usize {
    views.iter().filter(|v| !v.state.is_finished()).count()
}

/// `WORKING` while any job is unfinished, else `IDLE`.
pub fn footer(views: &[JobView]) -> &'static str {
    if pending(views) > 0 {
        WORKING
    } else {
        IDLE
    }
}

/// True when the list went from having unfinished jobs to having none: the queue
/// drained, so the library is refreshed.
pub fn drained(before: &[JobView], after: &[JobView]) -> bool {
    pending(before) > 0 && pending(after) == 0
}

/// The zone of job row `i`.
pub fn row_zone(i: usize) -> ZoneId {
    ZoneId(ROWS + i32::try_from(i.min(MAX_ROWS)).unwrap_or(0))
}

/// The job row index of a zone.
pub fn zone_row(zone: ZoneId) -> Option<usize> {
    let i = usize::try_from(zone.0.checked_sub(ROWS)?).ok()?;
    (i < MAX_ROWS).then_some(i)
}

/// One zone per job row (its actions), then the footer (CLEAR FINISHED).
pub fn zones(rows: &[Row]) -> Vec<Zone> {
    let mut zones: Vec<Zone> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| Zone::row(row_zone(i), r.actions.len()))
        .collect();
    zones.push(Zone::row(FOOTER, 1));
    zones
}

/// Where focus goes after the list changed: the same job (its action clamped), else
/// the row now at the old position, else the footer.
pub fn refocus(before: &[Uuid], focus: Option<Focus>, after: &[Row]) -> Option<Focus> {
    let focus = focus?;
    let Some(old) = zone_row(focus.zone) else {
        return Some(focus);
    };
    let clamp = |i: usize| {
        let len = after.get(i).map_or(0, |r| r.actions.len());
        (len > 0).then(|| Focus::new(row_zone(i), focus.index.min(len - 1)))
    };
    let same = before
        .get(old)
        .and_then(|id| after.iter().position(|r| r.id == *id));
    same.and_then(clamp)
        .or_else(|| clamp(old.min(after.len().saturating_sub(1))))
        .or(Some(Focus::new(FOOTER, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flox_core::model::EpisodeKey;
    use flox_rip::job::{Job, Source};

    fn view(state: JobState, progress: Option<f32>) -> JobView {
        let job = Job::new(EpisodeKey::episode(1, 1, 5), "Show", Source::Page, None);
        JobView {
            job,
            state,
            detail: "speed=2.1x".to_owned(),
            progress,
            attempt: 0,
        }
    }

    #[test]
    fn rows_per_state() {
        let r = row(&view(JobState::Downloading, Some(0.4)));
        assert_eq!(r.label, "Show · S01 E05");
        assert_eq!(r.state, "DOWNLOADING");
        assert_eq!(r.detail, "SPEED=2.1X");
        assert!((r.progress - 0.4).abs() < f32::EPSILON);
        assert_eq!(r.actions, vec![Action::Cancel]);
        assert_eq!(r.tone, Tone::Normal);

        let r = row(&view(JobState::Uploading, None));
        assert_eq!(r.progress, 0.0);
        let r = row(&view(JobState::Muxing, Some(0.9)));
        assert!(r.progress < 0.0, "no line while muxing");

        let r = row(&view(JobState::Failed("no space".into()), None));
        assert_eq!(r.state, "FAILED · NO SPACE");
        assert_eq!(r.tone, Tone::Alert);
        assert_eq!(r.actions, vec![Action::Retry, Action::Remove]);

        let r = row(&view(JobState::Done, None));
        assert_eq!(r.tone, Tone::Success);
        assert_eq!(r.actions, vec![Action::Remove]);
        assert_eq!(actions(&JobState::Cancelled), vec![Action::Remove]);
        assert_eq!(actions(&JobState::Queued), vec![Action::Cancel]);
        assert_eq!(
            row(&view(JobState::Resolving, None)).state,
            "RESOLVING STREAM"
        );
        assert_eq!(Action::Retry.label(), "RETRY");
    }

    #[test]
    fn counts_footer_and_drain() {
        let busy = vec![
            view(JobState::Done, None),
            view(JobState::Queued, None),
            view(JobState::Uploading, Some(0.1)),
        ];
        assert_eq!(pending(&busy), 2);
        assert_eq!(footer(&busy), WORKING);
        let idle = vec![
            view(JobState::Done, None),
            view(JobState::Failed("x".into()), None),
        ];
        assert_eq!(pending(&idle), 0);
        assert_eq!(footer(&idle), IDLE);
        assert_eq!(footer(&[]), IDLE);
        assert!(drained(&busy, &idle));
        assert!(!drained(&idle, &idle));
        assert!(!drained(&busy, &busy));
    }

    #[test]
    fn zones_and_refocus() {
        let views = vec![
            view(JobState::Failed("x".into()), None),
            view(JobState::Queued, None),
            view(JobState::Done, None),
        ];
        let before = rows(&views);
        let zones = zones(&before);
        assert_eq!(zones.len(), 4);
        assert_eq!(zones[0].len(), 2);
        assert_eq!(zone_row(row_zone(2)), Some(2));
        assert_eq!(zone_row(FOOTER), None);
        let ids: Vec<Uuid> = before.iter().map(|r| r.id).collect();

        // RETRY on the failed job: it becomes Queued (one action) and stays focused.
        let mut after_views = views.clone();
        after_views[0].state = JobState::Queued;
        let after = rows(&after_views);
        let f = refocus(&ids, Some(Focus::new(row_zone(0), 1)), &after);
        assert_eq!(f, Some(Focus::new(row_zone(0), 0)));

        // The focused job was removed: the row now in its place.
        let after = rows(&views[1..]);
        let f = refocus(&ids, Some(Focus::new(row_zone(0), 1)), &after);
        assert_eq!(f, Some(Focus::new(row_zone(0), 0)));
        // The last row was removed: the new last row.
        let after = rows(&views[..2]);
        let f = refocus(&ids, Some(Focus::new(row_zone(2), 0)), &after);
        assert_eq!(f, Some(Focus::new(row_zone(1), 0)));
        // Everything was cleared: the footer.
        let f = refocus(&ids, Some(Focus::new(row_zone(1), 0)), &[]);
        assert_eq!(f, Some(Focus::new(FOOTER, 0)));
        // Focus outside the rows is kept.
        let f = refocus(&ids, Some(Focus::new(FOOTER, 0)), &[]);
        assert_eq!(f, Some(Focus::new(FOOTER, 0)));
    }
}
