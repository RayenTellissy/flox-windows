//! Locating third-party binaries. The resolver itself is filled in by piece P2.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// A third-party binary Flox runs or loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Ffmpeg,
    Ffprobe,
    YtDlp,
    TdJson,
    LibMpv,
}

impl Tool {
    /// The file name looked for on this platform.
    pub fn file_name(self) -> &'static str {
        if cfg!(windows) {
            match self {
                Tool::Ffmpeg => "ffmpeg.exe",
                Tool::Ffprobe => "ffprobe.exe",
                Tool::YtDlp => "yt-dlp.exe",
                Tool::TdJson => "tdjson.dll",
                Tool::LibMpv => "libmpv-2.dll",
            }
        } else if cfg!(target_os = "macos") {
            match self {
                Tool::Ffmpeg => "ffmpeg",
                Tool::Ffprobe => "ffprobe",
                Tool::YtDlp => "yt-dlp",
                Tool::TdJson => "libtdjson.dylib",
                Tool::LibMpv => "libmpv.dylib",
            }
        } else {
            match self {
                Tool::Ffmpeg => "ffmpeg",
                Tool::Ffprobe => "ffprobe",
                Tool::YtDlp => "yt-dlp",
                Tool::TdJson => "libtdjson.so",
                Tool::LibMpv => "libmpv.so",
            }
        }
    }
}

/// A place a tool can be found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// `<app>\` and `<app>\tools\`.
    AppDir,
    /// Each entry of `PATH`.
    Path,
    /// The settings override (`ffmpeg_path`, `ytdlp_path`, `tdjson_path`, `libmpv_path`).
    Override,
}

/// Search order: the override wins when it is set and the file exists, then the app
/// directory, then `PATH`.
pub const RESOLVE_ORDER: &[Source] = &[Source::Override, Source::AppDir, Source::Path];

/// Finds `tool` following [`RESOLVE_ORDER`]. Ffprobe is derived from the resolved ffmpeg
/// by replacing the file stem. Filled in by P2.
#[allow(clippy::unimplemented)]
pub fn resolve(
    _tool: Tool,
    _app_dir: &Path,
    _path_env: Option<&OsStr>,
    _override_path: Option<&Path>,
) -> Option<PathBuf> {
    unimplemented!("flox_core::tools::resolve (P2)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_first() {
        assert_eq!(RESOLVE_ORDER.first(), Some(&Source::Override));
        assert_eq!(RESOLVE_ORDER.len(), 3);
    }
}
