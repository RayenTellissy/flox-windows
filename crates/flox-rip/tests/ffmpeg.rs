//! The media pieces against the real Homebrew ffmpeg and ffprobe. Each test is skipped
//! when the tools are not installed.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use flox_core::error::{Error, Result};
use flox_rip::{mux, probe, process, split};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
const FFPROBE: &str = "/opt/homebrew/bin/ffprobe";

fn tools() -> Option<(PathBuf, PathBuf)> {
    let (ffmpeg, ffprobe) = (PathBuf::from(FFMPEG), PathBuf::from(FFPROBE));
    if ffmpeg.is_file() && ffprobe.is_file() {
        Some((ffmpeg, ffprobe))
    } else {
        eprintln!("skipped: {FFMPEG} or {FFPROBE} is missing");
        None
    }
}

fn args(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

async fn ffmpeg(tool: &Path, list: Vec<OsString>) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    process::run(
        tool,
        &list,
        |l| lines.push(l.to_string()),
        CancellationToken::new(),
    )
    .await?;
    Ok(lines)
}

/// A 320x240 testsrc with a sine track, H.264 + AAC.
async fn make_clip(tool: &Path, out: &Path, seconds: &str) -> Result<()> {
    let mut list = args(&[
        "-y",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc=duration={seconds}:size=320x240:rate=25"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency=440:duration={seconds}"),
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-shortest",
    ]);
    list.push(out.into());
    ffmpeg(tool, list).await.map(|_| ())
}

fn sha256(path: &Path) -> Result<Vec<u8>> {
    Ok(Sha256::digest(fs::read(path)?).to_vec())
}

#[tokio::test]
async fn probe_reports_codec_height_and_duration() -> Result<()> {
    let Some((ffmpeg_path, ffprobe_path)) = tools() else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let clip = dir.path().join("media.mp4");
    make_clip(&ffmpeg_path, &clip, "3").await?;
    let p = probe::ffprobe(&ffprobe_path, &clip).await?;
    assert_eq!(p.codec, "h264");
    assert_eq!(p.height, 240);
    assert!((p.duration - 3.0).abs() < 0.1, "duration {}", p.duration);
    assert_eq!(probe::quality_label(p.height, None), "240p");
    Ok(())
}

#[tokio::test]
async fn probe_rejects_a_clip_of_one_second_or_less() -> Result<()> {
    let Some((ffmpeg_path, ffprobe_path)) = tools() else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let clip = dir.path().join("media.mp4");
    make_clip(&ffmpeg_path, &clip, "0.5").await?;
    match probe::ffprobe(&ffprobe_path, &clip).await {
        Err(Error::Other(m)) => assert_eq!(m, "download produced no video"),
        other => panic!("expected no video, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn probe_of_a_missing_file_is_a_tool_error() -> Result<()> {
    let Some((_, ffprobe_path)) = tools() else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    match probe::ffprobe(&ffprobe_path, &dir.path().join("nothing.mp4")).await {
        Err(Error::Tool(m)) => assert!(m.starts_with("ffprobe exit "), "{m}"),
        other => panic!("expected a tool error, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn dash_args_mux_hevc_video_and_aac_audio() -> Result<()> {
    let Some((ffmpeg_path, ffprobe_path)) = tools() else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let clip = dir.path().join("clip.mp4");
    make_clip(&ffmpeg_path, &clip, "3").await?;

    // Split the clip into DASH-like elementary tracks: HEVC video only, AAC audio only.
    let video = dir.path().join("v.mp4");
    let audio = dir.path().join("a.m4a");
    let mut v = args(&["-y", "-loglevel", "error", "-i"]);
    v.push(clip.clone().into());
    v.extend(args(&[
        "-an",
        "-c:v",
        "libx265",
        "-x265-params",
        "log-level=error",
    ]));
    v.push(video.clone().into());
    ffmpeg(&ffmpeg_path, v).await?;
    let mut a = args(&["-y", "-loglevel", "error", "-i"]);
    a.push(clip.clone().into());
    a.extend(args(&["-vn", "-c:a", "copy"]));
    a.push(audio.clone().into());
    ffmpeg(&ffmpeg_path, a).await?;

    let out = dir.path().join("media.mp4");
    ffmpeg(&ffmpeg_path, mux::dash_args(&video, Some(&audio), &out)).await?;
    let p = probe::ffprobe(&ffprobe_path, &out).await?;
    assert_eq!(p.codec, "hevc");
    assert_eq!(p.height, 240);
    assert!((p.duration - 3.0).abs() < 0.1, "duration {}", p.duration);

    // Video only, no audio track.
    let silent = dir.path().join("silent.mp4");
    ffmpeg(&ffmpeg_path, mux::dash_args(&video, None, &silent)).await?;
    assert_eq!(probe::ffprobe(&ffprobe_path, &silent).await?.codec, "hevc");

    // The Mac's `-tag:v hvc1` only fits HEVC: an H.264 track fails, with the tail in the error.
    let bad = dir.path().join("bad.mp4");
    match ffmpeg(&ffmpeg_path, mux::dash_args(&clip, None, &bad)).await {
        Err(Error::Tool(m)) => {
            assert!(m.starts_with("ffmpeg exit "), "{m}");
            assert!(m.contains("hvc1"), "{m}");
        }
        other => panic!("expected an ffmpeg failure, got {other:?}"),
    }

    // The codec-aware variant leaves the tag off for H.264, and the remux works.
    let good = dir.path().join("good.mp4");
    ffmpeg(&ffmpeg_path, mux::dash_args_for(&clip, None, &good, "h264")).await?;
    let p = probe::ffprobe(&ffprobe_path, &good).await?;
    assert_eq!((p.codec.as_str(), p.height), ("h264", 240));
    Ok(())
}

#[tokio::test]
async fn hls_args_pull_a_served_playlist_with_the_headers() -> Result<()> {
    let Some((ffmpeg_path, ffprobe_path)) = tools() else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let clip = dir.path().join("clip.mp4");
    make_clip(&ffmpeg_path, &clip, "3").await?;
    let hls = dir.path().join("hls");
    fs::create_dir(&hls)?;
    let mut h = args(&["-y", "-loglevel", "error", "-i"]);
    h.push(clip.into());
    h.extend(args(&[
        "-c",
        "copy",
        "-f",
        "hls",
        "-hls_time",
        "1",
        "-hls_playlist_type",
        "vod",
    ]));
    h.push(hls.join("pl.m3u8").into());
    ffmpeg(&ffmpeg_path, h).await?;

    // Serve the playlist and its segments; ffmpeg's http input is what takes the headers.
    let server = MockServer::start().await;
    for entry in fs::read_dir(&hls)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        Mock::given(method("GET"))
            .and(path(format!("/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(fs::read(entry.path())?))
            .mount(&server)
            .await;
    }

    let out = dir.path().join("media.mp4");
    let headers = vec![
        ("Origin".to_string(), "https://vidlink.pro".to_string()),
        (
            "Referer".to_string(),
            "https://elsewhere.example/".to_string(),
        ),
    ];
    let input = format!("{}/pl.m3u8", server.uri());
    let lines = ffmpeg(&ffmpeg_path, mux::hls_args(&input, &headers, &out)).await?;

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.len() >= 2, "{} requests", requests.len());
    for r in &requests {
        let header = |k: &str| r.headers.get(k).and_then(|v| v.to_str().ok());
        assert_eq!(header("user-agent"), Some(mux::USER_AGENT), "{}", r.url);
        assert_eq!(header("referer"), Some(mux::VIDLINK_REFERER), "{}", r.url);
        assert_eq!(header("origin"), Some("https://vidlink.pro"), "{}", r.url);
    }
    let details: Vec<String> = lines.iter().filter_map(|l| mux::parse_stats(l)).collect();
    assert!(!details.is_empty(), "no stats in {lines:?}");
    assert!(
        details.iter().any(|d| d.starts_with("00:00:0")),
        "{details:?}"
    );
    let p = probe::ffprobe(&ffprobe_path, &out).await?;
    assert_eq!((p.codec.as_str(), p.height), ("h264", 240));
    Ok(())
}

#[tokio::test]
async fn split_parts_concatenate_to_the_original() -> Result<()> {
    let Some((ffmpeg_path, _)) = tools() else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let clip = dir.path().join("media.mp4");
    make_clip(&ffmpeg_path, &clip, "3").await?;
    let size = fs::metadata(&clip)?.len();
    let before = sha256(&clip)?;
    let part_size = size / 4 + 1;

    let parts = split::split(&clip, part_size)?;
    assert_eq!(parts.len() as u64, size.div_ceil(part_size));
    assert!(parts.len() >= 4);
    assert!(!clip.exists());
    let mut joined = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let name = part.file_name().map(|n| n.to_string_lossy().into_owned());
        assert_eq!(name, Some(format!("media.part{}.mp4", i + 1)));
        joined.extend(fs::read(part)?);
    }
    assert_eq!(joined.len() as u64, size);
    assert_eq!(Sha256::digest(&joined).to_vec(), before);

    // A file at the part size stays as it is.
    let whole = dir.path().join("whole.mp4");
    fs::write(&whole, &joined)?;
    assert_eq!(split::split(&whole, size)?, vec![whole.clone()]);
    assert_eq!(sha256(&whole)?, before);
    Ok(())
}

fn running(marker: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", marker])
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn cancel_kills_the_process() -> Result<()> {
    let Some((ffmpeg_path, _)) = tools() else {
        return Ok(());
    };
    // A unique metadata value makes the process findable by its command line.
    let marker = format!("flox-cancel-{}", uuid::Uuid::new_v4());
    let list = args(&[
        "-re",
        "-f",
        "lavfi",
        "-i",
        "testsrc=duration=600:size=320x240:rate=25",
        "-metadata",
        &format!("title={marker}"),
        "-f",
        "null",
        "-",
    ]);
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move { process::run(&ffmpeg_path, &list, |_| {}, cancel).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(running(&marker), "ffmpeg did not start");
    let at = Instant::now();
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .map_err(|_| Error::Other("run did not return within 1 s of cancel".to_string()))?
        .map_err(|e| Error::Other(e.to_string()))?;
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert!(at.elapsed() < Duration::from_secs(1));
    assert!(!running(&marker), "ffmpeg is still running");
    Ok(())
}

#[tokio::test]
async fn nonzero_exit_carries_the_last_lines() -> Result<()> {
    let Some((ffmpeg_path, _)) = tools() else {
        return Ok(());
    };
    let list = args(&["-hide_banner", "-i", "/nonexistent/flox/input.mp4"]);
    match process::run(&ffmpeg_path, &list, |_| {}, CancellationToken::new()).await {
        Err(Error::Tool(m)) => {
            assert!(m.starts_with("ffmpeg exit "), "{m}");
            assert!(m.contains("/nonexistent/flox/input.mp4"), "{m}");
        }
        other => panic!("expected a tool error, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn missing_tool_is_a_tool_error() {
    let r = process::run(
        Path::new("/nonexistent/flox/ffmpeg"),
        &[],
        |_| {},
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(r, Err(Error::Tool(_))), "{r:?}");
}
