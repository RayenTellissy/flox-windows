//! Queue jobs and their observable state.

use std::path::PathBuf;

use flox_core::model::{EpisodeKey, MediaType};
use uuid::Uuid;

/// Where a job's video comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The VidLink page for the job's key, sniffed in rip mode.
    Page,
    /// A pasted direct link (yt-dlp fallback when it is not a file).
    Link(String),
    /// A HubCloud link from 4KHDHub, resolved before download.
    Hub { url: String, name: String },
    /// A local file.
    File(PathBuf),
}

/// Dynamic range tag appended to the quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tag {
    Sdr,
    Hdr,
    Dv,
}

impl Tag {
    /// The caption suffix: `"DV"`, `"HDR"`, or `""` for SDR.
    pub fn suffix(self) -> &'static str {
        match self {
            Tag::Sdr => "",
            Tag::Hdr => "HDR",
            Tag::Dv => "DV",
        }
    }
}

/// One unit of work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: Uuid,
    pub key: EpisodeKey,
    pub title: String,
    pub source: Source,
    pub tag: Option<Tag>,
}

impl Job {
    /// A job with a fresh id.
    pub fn new(key: EpisodeKey, title: &str, source: Source, tag: Option<Tag>) -> Self {
        Self {
            id: Uuid::new_v4(),
            key,
            title: title.to_string(),
            source,
            tag,
        }
    }

    /// `"Title · S01 E05"` or `"Title · MOVIE"`, as the Mac's job row.
    pub fn label(&self) -> String {
        let key = match self.key.media {
            MediaType::Tv => format!("S{:02} E{:02}", self.key.season, self.key.episode),
            MediaType::Movie => "MOVIE".to_string(),
        };
        format!("{} · {key}", self.title)
    }
}

/// Job lifecycle, as on the Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Resolving,
    Downloading,
    Muxing,
    Uploading,
    Done,
    Failed(String),
    Cancelled,
}

impl JobState {
    /// The state text of the Mac's job row: `"Resolving stream"`, `"Failed · <message>"`...
    pub fn label(&self) -> String {
        match self {
            JobState::Queued => "Queued".to_string(),
            JobState::Resolving => "Resolving stream".to_string(),
            JobState::Downloading => "Downloading".to_string(),
            JobState::Muxing => "Muxing".to_string(),
            JobState::Uploading => "Uploading".to_string(),
            JobState::Done => "Done".to_string(),
            JobState::Failed(m) => format!("Failed · {m}"),
            JobState::Cancelled => "Cancelled".to_string(),
        }
    }

    /// Done, Failed or Cancelled.
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            JobState::Done | JobState::Failed(_) | JobState::Cancelled
        )
    }
}

/// A job as the Queue screen shows it.
///
/// `progress` is `Some(0.0..=1.0)` while the job downloads or uploads (the Mac shows its
/// progress bar only then) and `None` otherwise. `attempt` counts failed attempts: a job
/// gets two in total, and a retry from the screen starts again at 0.
#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub job: Job,
    pub state: JobState,
    pub detail: String,
    pub progress: Option<f32>,
    pub attempt: u32,
}

impl JobView {
    /// A freshly queued job.
    pub fn queued(job: Job) -> Self {
        Self {
            job,
            state: JobState::Queued,
            detail: String::new(),
            progress: None,
            attempt: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_the_mac() {
        let tv = Job::new(EpisodeKey::episode(1399, 1, 5), "Show", Source::Page, None);
        assert_eq!(tv.label(), "Show · S01 E05");
        let movie = Job::new(EpisodeKey::movie(603), "Film", Source::Page, None);
        assert_eq!(movie.label(), "Film · MOVIE");
        assert_eq!(JobState::Resolving.label(), "Resolving stream");
        assert_eq!(JobState::Failed("boom".into()).label(), "Failed · boom");
        assert!(JobState::Cancelled.is_finished());
        assert!(!JobState::Uploading.is_finished());
    }
}
