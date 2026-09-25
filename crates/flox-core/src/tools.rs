//! Locating third-party binaries.

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

    /// Whether this is a program (as opposed to a shared library).
    pub fn is_program(self) -> bool {
        matches!(self, Tool::Ffmpeg | Tool::Ffprobe | Tool::YtDlp)
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

/// Finds `tool` following [`RESOLVE_ORDER`].
///
/// - The override may name the file itself or a directory holding it. On Windows an
///   override without an extension also matches with `.exe` added (programs only).
/// - The app directory is searched as `<app>` then `<app>/tools`.
/// - `path_env` is a `PATH`-style list in the platform's syntax; empty entries are
///   skipped and surrounding quotes are removed.
/// - Ffprobe is derived from the resolved ffmpeg by replacing the file stem (`override_path`
///   is then the ffmpeg override). When no ffprobe sits beside that ffmpeg, ffprobe is
///   looked up by its own name in the app directory and on `PATH`.
pub fn resolve(
    tool: Tool,
    app_dir: &Path,
    path_env: Option<&OsStr>,
    override_path: Option<&Path>,
) -> Option<PathBuf> {
    if tool == Tool::Ffprobe {
        let beside = resolve(Tool::Ffmpeg, app_dir, path_env, override_path)
            .map(|ffmpeg| ffprobe_beside(&ffmpeg))
            .filter(|p| p.is_file());
        return beside.or_else(|| search(tool, app_dir, path_env, None, cfg!(windows)));
    }
    search(tool, app_dir, path_env, override_path, cfg!(windows))
}

fn search(
    tool: Tool,
    app_dir: &Path,
    path_env: Option<&OsStr>,
    override_path: Option<&Path>,
    windows: bool,
) -> Option<PathBuf> {
    let name = tool.file_name();
    RESOLVE_ORDER.iter().find_map(|source| match source {
        Source::Override => override_path.and_then(|p| from_override(tool, p, windows)),
        Source::AppDir => [app_dir.join(name), app_dir.join("tools").join(name)]
            .into_iter()
            .find(|p| p.is_file()),
        Source::Path => path_entries(path_env)
            .into_iter()
            .map(|dir| dir.join(name))
            .find(|p| p.is_file()),
    })
}

fn from_override(tool: Tool, p: &Path, windows: bool) -> Option<PathBuf> {
    if p.as_os_str().is_empty() {
        return None;
    }
    if p.is_dir() {
        return Some(p.join(tool.file_name())).filter(|f| f.is_file());
    }
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    if windows && tool.is_program() && p.extension().is_none() {
        return Some(p.with_extension("exe")).filter(|f| f.is_file());
    }
    None
}

fn path_entries(path_env: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(env) = path_env else {
        return Vec::new();
    };
    std::env::split_paths(env)
        .filter_map(|p| {
            let s = p.to_string_lossy();
            let trimmed = s.trim().trim_matches('"');
            if trimmed.is_empty() {
                None
            } else if trimmed.len() == s.len() {
                Some(p)
            } else {
                Some(PathBuf::from(trimmed))
            }
        })
        .collect()
}

/// The ffprobe that sits beside `ffmpeg`: the `ffmpeg` part of the file stem becomes
/// `ffprobe` (the whole stem when it has no `ffmpeg` in it) and the extension is kept.
pub fn ffprobe_beside(ffmpeg: &Path) -> PathBuf {
    let stem = ffmpeg
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let new_stem = match stem.to_ascii_lowercase().find("ffmpeg") {
        Some(i) => format!(
            "{}ffprobe{}",
            stem.get(..i).unwrap_or(""),
            stem.get(i + "ffmpeg".len()..).unwrap_or("")
        ),
        None => "ffprobe".to_owned(),
    };
    let mut file = std::ffi::OsString::from(new_stem);
    if let Some(ext) = ffmpeg.extension() {
        file.push(".");
        file.push(ext);
    }
    ffmpeg.with_file_name(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn override_first() {
        assert_eq!(RESOLVE_ORDER.first(), Some(&Source::Override));
        assert_eq!(RESOLVE_ORDER.len(), 3);
    }

    #[test]
    fn ffprobe_stem_replacement() {
        assert_eq!(
            ffprobe_beside(Path::new("/x/tools/ffmpeg.exe")),
            PathBuf::from("/x/tools/ffprobe.exe")
        );
        assert_eq!(
            ffprobe_beside(Path::new("/opt/bin/ffmpeg")),
            PathBuf::from("/opt/bin/ffprobe")
        );
        assert_eq!(
            ffprobe_beside(Path::new("/opt/bin/ffmpeg-7")),
            PathBuf::from("/opt/bin/ffprobe-7")
        );
        assert_eq!(
            ffprobe_beside(Path::new("/opt/bin/FFmpeg.exe")),
            PathBuf::from("/opt/bin/ffprobe.exe")
        );
        assert_eq!(
            ffprobe_beside(Path::new("/opt/bin/avconv")),
            PathBuf::from("/opt/bin/ffprobe")
        );
    }

    #[test]
    fn windows_override_without_extension() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ffmpeg.exe"), b"").unwrap();
        let bare = dir.path().join("ffmpeg");
        assert_eq!(
            from_override(Tool::Ffmpeg, &bare, true),
            Some(dir.path().join("ffmpeg.exe"))
        );
        assert_eq!(from_override(Tool::Ffmpeg, &bare, false), None);
        fs::write(dir.path().join("tdjson.exe"), b"").unwrap();
        assert_eq!(
            from_override(Tool::TdJson, &dir.path().join("tdjson"), true),
            None
        );
    }

    #[test]
    fn quoted_and_empty_path_entries() {
        let a = tempfile::tempdir().unwrap();
        let joined = std::env::join_paths([a.path()]).unwrap();
        let mut quoted = std::ffi::OsString::from("\"");
        quoted.push(&joined);
        quoted.push("\"");
        assert_eq!(path_entries(Some(&quoted)), vec![a.path().to_path_buf()]);
        assert!(path_entries(Some(OsStr::new(""))).is_empty());
        assert!(path_entries(None).is_empty());
    }
}
