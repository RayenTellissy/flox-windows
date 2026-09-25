//! One job end to end (resolve, download, mux, probe, replace, split, upload), as the
//! Mac's `Queue.run`. Private to the queue.
//!
//! Every step takes the job's cancel token: ffmpeg and yt-dlp are killed, downloads
//! stop, the sniff is torn down and pending sends are deleted. Steps without a token of
//! their own are raced against it.

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flox_core::error::{Error, Result};
use flox_core::sniff::{self, SniffMode, SniffResult, StreamKind};
use flox_td::caption::{self, Caption};
use flox_td::library::{self, LibraryIndex};
use flox_td::upload::{self, UploadRequest};
use futures::stream::{FuturesUnordered, StreamExt};
use parking_lot::Mutex;
use reqwest::header::USER_AGENT as USER_AGENT_HEADER;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::download::{dash, file};
use crate::job::{Job, JobState, Source};
use crate::queue::Queue;
use crate::{hub, mux, probe, process, split};

/// The finished file every source ends up as.
const MEDIA: &str = "media.mp4";

/// Connect timeout for manifest, caption and HubCloud requests.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// Races `f` against `cancel`.
async fn guard<T>(cancel: &CancellationToken, f: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::Cancelled),
        r = f => r,
    }
}

fn check(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(CONNECT_TIMEOUT)
        .build()?)
}

/// The last path segment of a URL, `""` when there is none.
fn last_segment(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or_default()
        .to_string()
}

/// The extension of a URL's last path segment.
fn url_extension(url: &Url) -> Option<String> {
    Path::new(&last_segment(url))
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .filter(|e| !e.is_empty())
}

/// The quality caption field: `"<height>p"` plus the tag, as the Mac joins them.
fn quality(height: u32, job: &Job) -> String {
    if height > 0 {
        probe::quality_label(height, job.tag)
    } else {
        job.tag.map(|t| t.suffix()).unwrap_or_default().to_string()
    }
}

/// Runs `job` in `dir` (created here; the queue removes it afterwards).
pub(crate) async fn run(
    q: &Queue,
    job: &Job,
    dir: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let media = dir.join(MEDIA);
    let mut subtitle = None;

    match &job.source {
        Source::Page => subtitle = page(q, job, dir, &media, cancel).await?,
        Source::Link(url) => {
            let parsed = Url::parse(url)?;
            pull(q, job, &parsed, &last_segment(&parsed), dir, cancel).await?;
        }
        Source::Hub { url, name } => {
            q.set_state(job.id, JobState::Resolving, None);
            q.set_detail(job.id, name.as_str());
            let direct = guard(cancel, async {
                let http = http_client()?;
                hub::resolve(&http, url).await
            })
            .await?;
            check(cancel)?;
            pull(q, job, &Url::parse(&direct)?, name, dir, cancel).await?;
        }
        Source::File(path) => stage(q, job, path, &media, cancel).await?,
    }

    check(cancel)?;
    let probe = guard(cancel, probe::ffprobe(&q.tools().ffprobe, &media)).await?;
    let quality = quality(probe.height, job);
    let part_size = q.options.part_size;
    let split_media = media.clone();
    let parts = tokio::task::spawn_blocking(move || split::split(&split_media, part_size))
        .await
        .map_err(|e| Error::Other(format!("split: {e}")))??;
    check(cancel)?;

    q.set_state(job.id, JobState::Uploading, Some(0.0));
    let chat_id = guard(cancel, q.chat_id()).await?;
    replace(q, job, chat_id, &quality, &probe.codec, cancel).await?;
    q.set_detail(
        job.id,
        if parts.len() > 1 {
            format!("{} parts at once", parts.len())
        } else {
            String::new()
        },
    );
    let ids = upload_parts(q, job, chat_id, &parts, &quality, &probe.codec, cancel).await?;
    if let (Some(sub), Some(first)) = (subtitle, ids.first()) {
        let req = UploadRequest {
            chat_id,
            path: sub,
            caption: String::new(),
            reply_to: Some(*first),
        };
        upload::send_document(q.deps.td.as_ref(), req, |_| {}, cancel.clone()).await?;
    }
    Ok(())
}

/// Page jobs: sniff, then a file, DASH or HLS pull, then the English caption.
/// Returns the caption file when one was fetched.
async fn page(
    q: &Queue,
    job: &Job,
    dir: &Path,
    media: &Path,
    cancel: &CancellationToken,
) -> Result<Option<PathBuf>> {
    q.set_state(job.id, JobState::Resolving, None);
    let sniffer = q
        .deps
        .sniffer
        .clone()
        .ok_or_else(|| Error::Unavailable("page sniffing needs WebView2".into()))?;
    let page_url = sniff::vidlink_url(job.key, None);
    let info = sniffer
        .sniff(&page_url, SniffMode::Rip, cancel.clone())
        .await?;
    check(cancel)?;
    q.set_state(job.id, JobState::Downloading, Some(0.0));

    let is_mpd = Url::parse(&info.url)
        .ok()
        .and_then(|u| url_extension(&u))
        .is_some_and(|e| e.eq_ignore_ascii_case("mpd"));
    if info.kind == StreamKind::File {
        file::download(
            &info.url,
            &info.headers,
            media,
            |got, total| report_bytes(q, job, got, total),
            cancel.clone(),
        )
        .await?;
    } else if info.kind == StreamKind::Dash || is_mpd {
        pull_dash(q, job, &info, dir, media, cancel).await?;
    } else {
        pull_hls(q, job, &info, media, cancel).await?;
    }
    english_caption(&info, dir, cancel).await
}

fn report_bytes(q: &Queue, job: &Job, got: u64, total: Option<u64>) {
    if let Some(total) = total.filter(|t| *t > 0) {
        #[allow(clippy::cast_precision_loss)]
        let frac = got as f64 / total as f64;
        #[allow(clippy::cast_possible_truncation)]
        q.set_progress(job.id, frac as f32);
    }
}

/// DASH: both tracks, then an ffmpeg copy-mux into `media` (hvc1 tag only for HEVC).
async fn pull_dash(
    q: &Queue,
    job: &Job,
    info: &SniffResult,
    dir: &Path,
    media: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let base = Url::parse(&info.url)?;
    let xml = guard(cancel, async {
        let mut req = http_client()?
            .get(base.clone())
            .header(USER_AGENT_HEADER, file::USER_AGENT);
        for (k, v) in &info.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Other(format!(
                "http {} for the manifest",
                status.as_u16()
            )));
        }
        Ok(resp.text().await?)
    })
    .await?;
    let mpd = dash::parse(&xml, &base)?;
    let (video, audio) = dash::download(
        &mpd,
        &info.headers,
        dir,
        |p| q.set_progress(job.id, p),
        cancel.clone(),
    )
    .await?;
    q.set_state(job.id, JobState::Muxing, None);
    let codec = video_codec(q, &video, cancel).await?;
    let args = mux::dash_args_for(&video, audio.as_deref(), media, &codec);
    process::run(&q.tools().ffmpeg, &args, |_| {}, cancel.clone()).await?;
    let _ = tokio::fs::remove_file(&video).await;
    if let Some(a) = audio {
        let _ = tokio::fs::remove_file(&a).await;
    }
    Ok(())
}

/// The codec of the first video stream of `file` (no duration check: a bare track may
/// report none).
async fn video_codec(q: &Queue, file: &Path, cancel: &CancellationToken) -> Result<String> {
    let mut out = String::new();
    process::run(
        &q.tools().ffprobe,
        &probe::probe_args(file),
        |line| {
            out.push_str(line);
            out.push('\n')
        },
        cancel.clone(),
    )
    .await?;
    Ok(probe::parse_probe(&out)?.codec)
}

/// HLS: ffmpeg pulls the playlist; its stats line is the job detail.
async fn pull_hls(
    q: &Queue,
    job: &Job,
    info: &SniffResult,
    media: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let args = with_download_user_agent(mux::hls_args(&info.url, &info.headers, media));
    process::run(
        &q.tools().ffmpeg,
        &args,
        |line| {
            if let Some(stats) = mux::parse_stats(line) {
                q.set_detail(job.id, stats);
            }
        },
        cancel.clone(),
    )
    .await
}

/// Swaps the `-user_agent` value for the downloaders' user agent, so every request of a
/// job presents the same browser.
fn with_download_user_agent(mut args: Vec<OsString>) -> Vec<OsString> {
    if let Some(i) = args.iter().position(|a| a == "-user_agent") {
        if let Some(value) = args.get_mut(i + 1) {
            *value = file::USER_AGENT.into();
        }
    }
    args
}

/// The first caption whose language starts with "en", saved as `en.<ext>`. A failed
/// fetch is logged and skipped, as on the Mac.
async fn english_caption(
    info: &SniffResult,
    dir: &Path,
    cancel: &CancellationToken,
) -> Result<Option<PathBuf>> {
    let Some(en) = info
        .captions
        .iter()
        .find(|c| c.language.to_lowercase().starts_with("en"))
    else {
        return Ok(None);
    };
    let ext = if en.kind.is_empty() {
        Url::parse(&en.url)
            .ok()
            .and_then(|u| url_extension(&u))
            .unwrap_or_else(|| "srt".to_string())
    } else {
        en.kind.clone()
    };
    let path = dir.join(format!("en.{ext}"));
    let fetched = guard(cancel, async {
        let resp = http_client()?
            .get(&en.url)
            .header(USER_AGENT_HEADER, file::USER_AGENT)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Other(format!("http {}", status.as_u16())));
        }
        let body = resp.bytes().await?;
        tokio::fs::write(&path, &body).await?;
        Ok(())
    })
    .await;
    match fetched {
        Ok(()) => Ok(Some(path)),
        Err(Error::Cancelled) => Err(Error::Cancelled),
        Err(e) => {
            tracing::warn!("queue: English caption not fetched: {e}");
            Ok(None)
        }
    }
}

/// Link and Hub jobs: a direct download into `media.<ext>`, falling back to yt-dlp when
/// the URL is not a plain file, then renamed to `media.mp4`.
async fn pull(
    q: &Queue,
    job: &Job,
    url: &Url,
    name: &str,
    dir: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    q.set_state(job.id, JobState::Downloading, Some(0.0));
    q.set_detail(job.id, name);
    let ext = url_extension(url).unwrap_or_else(|| "mkv".to_string());
    let mut target = dir.join(format!("media.{ext}"));
    let got = file::download(
        url.as_str(),
        &[],
        &target,
        |got, total| report_bytes(q, job, got, total),
        cancel.clone(),
    )
    .await;
    match got {
        Ok(()) => {}
        Err(Error::NotAFile(why)) => {
            tracing::info!("queue: {url} is not a plain file ({why}), trying yt-dlp");
            let _ = tokio::fs::remove_file(&target).await;
            q.set_detail(job.id, "not a plain file, trying yt-dlp");
            ytdlp(q, job, url, &target, cancel).await?;
            // yt-dlp may name a merged download after the merge format.
            let merged = dir.join("media.mkv");
            if !target.exists() && merged.exists() {
                target = merged;
            }
        }
        Err(e) => return Err(e),
    }
    let media = dir.join(MEDIA);
    if target != media {
        tokio::fs::rename(&target, &media).await?;
    }
    Ok(())
}

/// `yt-dlp -f "bv*+ba/b" --merge-output-format mkv --ffmpeg-location <ffmpeg> -o <out> <url>`.
async fn ytdlp(
    q: &Queue,
    job: &Job,
    url: &Url,
    out: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let tool = q
        .tools()
        .ytdlp
        .ok_or_else(|| Error::Tool("yt-dlp not installed".into()))?;
    let args: Vec<OsString> = vec![
        "-f".into(),
        "bv*+ba/b".into(),
        "--merge-output-format".into(),
        "mkv".into(),
        "--ffmpeg-location".into(),
        q.tools().ffmpeg.into(),
        "-o".into(),
        out.into(),
        url.as_str().into(),
    ];
    process::run(
        &tool,
        &args,
        |line| q.set_detail(job.id, line),
        cancel.clone(),
    )
    .await
}

/// File jobs: a file that fits one part is hard-linked into the job folder (when on the
/// same volume); anything else is copied, since splitting truncates its source.
async fn stage(
    q: &Queue,
    job: &Job,
    source: &Path,
    media: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    q.set_state(job.id, JobState::Downloading, Some(0.0));
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    q.set_detail(job.id, name.as_str());
    let meta = match tokio::fs::metadata(source).await {
        Ok(m) if m.is_file() => m,
        _ => {
            return Err(Error::Other(format!(
                "file not found: {}",
                source.display()
            )))
        }
    };
    let linked = meta.len() <= q.options.part_size && std::fs::hard_link(source, media).is_ok();
    if !linked {
        q.set_detail(job.id, format!("copying {name}"));
        let (from, to) = (source.to_path_buf(), media.to_path_buf());
        guard(cancel, async move {
            tokio::task::spawn_blocking(move || std::fs::copy(from, to))
                .await
                .map_err(|e| Error::Other(format!("copy: {e}")))??;
            Ok(())
        })
        .await?;
        q.set_detail(job.id, name);
    }
    q.set_progress(job.id, 1.0);
    Ok(())
}

/// Deletes the library's print with the same key, quality and codec before uploading.
/// Reading or deleting failures are logged, not fatal, as on the Mac.
async fn replace(
    q: &Queue,
    job: &Job,
    chat_id: i64,
    quality: &str,
    codec: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let td = q.deps.td.as_ref();
    let messages = match guard(cancel, library::list_documents(td, chat_id)).await {
        Ok(m) => m,
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(e) => {
            tracing::warn!("queue: could not read the library before uploading: {e}");
            return Ok(());
        }
    };
    let index = LibraryIndex::build(&messages);
    let Some(old) = index.find(job.key, quality, codec) else {
        return Ok(());
    };
    q.set_detail(job.id, format!("replacing previous {} upload", old.label()));
    let deleted = guard(
        cancel,
        library::replace_existing(td, chat_id, &index, job.key, quality, codec),
    )
    .await;
    match deleted {
        Ok(_) => Ok(()),
        Err(Error::Cancelled) => Err(Error::Cancelled),
        Err(e) => {
            tracing::warn!("queue: could not delete the previous upload: {e}");
            Ok(())
        }
    }
}

/// Uploads every part at once (Telegram caps one file's upload speed), reporting the mean
/// progress and deleting each part of a multi-part split once it is sent. Returns the
/// message ids in part order. The first failure stops the other sends.
async fn upload_parts(
    q: &Queue,
    job: &Job,
    chat_id: i64,
    parts: &[PathBuf],
    quality: &str,
    codec: &str,
    cancel: &CancellationToken,
) -> Result<Vec<i64>> {
    let count = parts.len();
    let parts_u32 = u32::try_from(count).map_err(|_| Error::Other("too many parts".into()))?;
    let fractions = Mutex::new(vec![0.0f32; count]);
    let fractions = &fractions;
    let stop = cancel.child_token();
    let mut sends = FuturesUnordered::new();
    for (i, (part, index)) in parts.iter().zip(1..=parts_u32).enumerate() {
        let caption = caption::encode(&Caption::new(job.key, quality, codec, index, parts_u32));
        let req = UploadRequest {
            chat_id,
            path: part.clone(),
            caption,
            reply_to: None,
        };
        let stop = stop.clone();
        sends.push(async move {
            let progress = |p: f32| {
                let mean = {
                    let mut f = fractions.lock();
                    if let Some(slot) = f.get_mut(i) {
                        *slot = p;
                    }
                    #[allow(clippy::cast_precision_loss)]
                    let n = count as f32;
                    f.iter().sum::<f32>() / n
                };
                q.set_progress(job.id, mean);
            };
            let id = upload::send_document(q.deps.td.as_ref(), req, progress, stop).await?;
            if count > 1 {
                let _ = tokio::fs::remove_file(part).await;
            }
            Ok::<_, Error>((i, id))
        });
    }
    let mut ids = vec![0i64; count];
    let mut failure = None;
    while let Some(r) = sends.next().await {
        match r {
            Ok((i, id)) => {
                if let Some(slot) = ids.get_mut(i) {
                    *slot = id;
                }
            }
            Err(e) => {
                if failure.is_none() {
                    stop.cancel();
                    failure = Some(e);
                }
            }
        }
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(ids),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::Tag;
    use flox_core::model::EpisodeKey;

    #[test]
    fn hls_user_agent_is_the_downloaders() {
        let args = with_download_user_agent(mux::hls_args(
            "https://x/m.m3u8",
            &[],
            Path::new("/t/media.mp4"),
        ));
        let i = args.iter().position(|a| a == "-user_agent").unwrap();
        assert_eq!(args[i + 1], OsString::from(file::USER_AGENT));
    }

    #[test]
    fn quality_joins_height_and_tag() {
        let mut job = Job::new(EpisodeKey::movie(1), "t", Source::Page, Some(Tag::Dv));
        assert_eq!(quality(2160, &job), "2160p DV");
        assert_eq!(quality(0, &job), "DV");
        job.tag = None;
        assert_eq!(quality(0, &job), "");
        assert_eq!(quality(1080, &job), "1080p");
    }

    #[test]
    fn url_names() {
        let u = Url::parse("https://h/a/b/Show.S01E02.mkv?x=1").unwrap();
        assert_eq!(last_segment(&u), "Show.S01E02.mkv");
        assert_eq!(url_extension(&u).as_deref(), Some("mkv"));
        let bare = Url::parse("https://pixeldrain.dev/api/file/abc").unwrap();
        assert_eq!(url_extension(&bare), None);
    }
}
