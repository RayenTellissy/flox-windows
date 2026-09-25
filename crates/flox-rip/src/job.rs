//! Queue jobs and their observable state.

use std::path::PathBuf;

use flox_core::model::EpisodeKey;
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

/// One unit of work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: Uuid,
    pub key: EpisodeKey,
    pub title: String,
    pub source: Source,
    pub tag: Option<Tag>,
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

/// A job as the Queue screen shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub job: Job,
    pub state: JobState,
    pub detail: String,
    pub progress: Option<f32>,
    pub attempt: u32,
}
