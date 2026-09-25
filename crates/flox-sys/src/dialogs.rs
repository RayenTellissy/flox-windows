//! Native file pickers (rfd).
//!
//! Windows offers a "Video" filter and an "All files" filter (the Mac app's
//! movie/video/data types). The macOS development build shows every file,
//! because rfd merges all filters into one allow-list there.

use std::path::PathBuf;

/// Extensions offered by the "Video" filter.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "mov", "avi", "webm", "ts", "m2ts", "mts", "wmv", "flv", "mpg", "mpeg",
    "3gp", "ogv",
];

/// Video files chosen by the user; empty when cancelled. `multi` allows
/// several files (TV episodes); otherwise one file (a movie).
pub async fn pick_files(multi: bool) -> Vec<PathBuf> {
    let dialog = dialog(multi);
    let picked = if multi {
        dialog.pick_files().await.unwrap_or_default()
    } else {
        dialog.pick_file().await.into_iter().collect()
    };
    picked
        .into_iter()
        .map(|file| file.path().to_path_buf())
        .collect()
}

fn dialog(multi: bool) -> rfd::AsyncFileDialog {
    let title = if multi {
        "Choose video files"
    } else {
        "Choose a video file"
    };
    let dialog = rfd::AsyncFileDialog::new().set_title(title);
    if cfg!(windows) {
        dialog
            .add_filter("Video", VIDEO_EXTENSIONS)
            .add_filter("All files", &["*"])
    } else {
        dialog
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_extensions_are_lowercase_and_bare() {
        for ext in VIDEO_EXTENSIONS {
            assert_eq!(*ext, ext.to_ascii_lowercase());
            assert!(!ext.starts_with('.'));
        }
        assert!(VIDEO_EXTENSIONS.contains(&"mkv"));
    }
}
