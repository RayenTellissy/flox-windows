//! Resolved external tool paths for a queue run.

use std::path::PathBuf;

/// ffmpeg and ffprobe are required; yt-dlp is optional (link fallback only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolPaths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub ytdlp: Option<PathBuf>,
}
