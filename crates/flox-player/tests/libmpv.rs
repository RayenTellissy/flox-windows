//! Live libmpv tests. They skip unless `FLOX_LIBMPV` names a libmpv build
//! (for example `/opt/homebrew/lib/libmpv.dylib` or `libmpv-2.dll`), and need
//! ffmpeg to generate the test clip (`FLOX_FFMPEG`, `deps/tools`, or `PATH`).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use flox_core::tools::{find_for_tests, Tool};
use flox_player::ffi::MpvLib;
use flox_player::mpv::{Format, Mpv, MpvEvent};
use flox_player::stream_cb::{self, StreamSource};
use tokio::sync::mpsc::Receiver;

fn libmpv() -> Option<PathBuf> {
    match std::env::var_os("FLOX_LIBMPV") {
        Some(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ => {
            eprintln!("skipping: FLOX_LIBMPV is not set");
            None
        }
    }
}

fn ffmpeg() -> Option<PathBuf> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let found = find_for_tests(Tool::Ffmpeg, &repo);
    if found.is_none() {
        eprintln!("skipping: ffmpeg not found (set FLOX_FFMPEG or put it on PATH)");
    }
    found
}

/// A 2 s 320x240 testsrc + sine clip in Matroska.
fn make_clip(ffmpeg: &Path, dir: &Path) -> PathBuf {
    let out = dir.join("clip.mkv");
    let status = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=320x240:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
            "-shortest",
            "-y",
        ])
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed");
    out
}

async fn wait_for(rx: &mut Receiver<MpvEvent>, pred: impl Fn(&MpvEvent) -> bool) -> MpvEvent {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let e = rx.recv().await.expect("event channel open");
            if pred(&e) {
                return e;
            }
        }
    })
    .await
    .expect("event in time")
}

struct CursorSource {
    inner: Cursor<Vec<u8>>,
    cancelled: Arc<AtomicBool>,
}

impl Read for CursorSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for CursorSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl StreamSource for CursorSource {
    fn size(&self) -> Option<u64> {
        Some(self.inner.get_ref().len() as u64)
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn plays_a_file_and_a_stream_callback_to_eof() {
    let Some(path) = libmpv() else { return };
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clip = make_clip(&ffmpeg, dir.path());
    let bytes = std::fs::read(&clip).unwrap();

    let lib = MpvLib::load(&path).unwrap();
    let mpv = Mpv::new(
        lib,
        &[
            ("vo", "null"),
            ("ao", "null"),
            ("keep-open", "no"),
            ("idle", "yes"),
        ],
    )
    .unwrap();
    let mut events = mpv.events();
    mpv.observe("time-pos", Format::Double).unwrap();
    mpv.observe("track-list", Format::Node).unwrap();

    // A plain file: load, read duration and track list.
    mpv.command(&["loadfile", clip.to_str().unwrap()]).unwrap();
    wait_for(&mut events, |e| *e == MpvEvent::FileLoaded).await;
    let duration: f64 = mpv.get_property("duration").unwrap();
    assert!((duration - 2.0).abs() < 0.2, "duration {duration}");
    let tracks: serde_json::Value = mpv.get_property("track-list").unwrap();
    assert_eq!(flox_player::tracks::audio_options(&tracks).len(), 1);
    mpv.set_property("pause", false).unwrap();
    assert!(!mpv.get_property::<bool>("pause").unwrap());
    mpv.command(&["stop"]).unwrap();
    wait_for(&mut events, |e| matches!(e, MpvEvent::EndFile { .. })).await;

    // The same bytes through the stream callback.
    let opened = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let seen = opened.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    stream_cb::register(
        &mpv,
        "flox",
        Box::new(move |uri| {
            seen.lock().push(uri.to_owned());
            (uri == "flox://x").then(|| {
                Box::new(CursorSource {
                    inner: Cursor::new(bytes.clone()),
                    cancelled: flag.clone(),
                }) as Box<dyn StreamSource>
            })
        }),
    )
    .unwrap();
    mpv.command(&["loadfile", "flox://x"]).unwrap();
    wait_for(&mut events, |e| *e == MpvEvent::FileLoaded).await;
    let d2: f64 = mpv.get_property("duration").unwrap();
    assert!((d2 - duration).abs() < 0.01);
    let end = wait_for(&mut events, |e| matches!(e, MpvEvent::EndFile { .. })).await;
    assert_eq!(
        end,
        MpvEvent::EndFile {
            reason: "eof".into(),
            error: 0
        }
    );
    assert!(opened.lock().iter().any(|u| u == "flox://x"));

    // Unknown URIs fail to load instead of crashing.
    mpv.command(&["loadfile", "flox://missing"]).unwrap();
    let end = wait_for(&mut events, |e| matches!(e, MpvEvent::EndFile { .. })).await;
    assert!(
        matches!(end, MpvEvent::EndFile { ref reason, .. } if reason == "error"),
        "{end:?}"
    );

    drop(mpv);
    let _ = cancelled.load(Ordering::SeqCst);
}

#[test]
fn bad_option_is_an_error() {
    let Some(path) = libmpv() else { return };
    let lib = MpvLib::load(&path).unwrap();
    assert!(Mpv::new(lib, &[("no-such-option", "1")]).is_err());
}
