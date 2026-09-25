//! The queue end to end against a scripted TDLib, a fake sniffer, wiremock and the real
//! Homebrew ffmpeg and ffprobe. Each test is skipped when the tools are not installed.

#![allow(clippy::unwrap_used)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use flox_core::error::{Error, Result};
use flox_core::model::EpisodeKey;
use flox_core::settings::{Settings, SettingsStore};
use flox_core::sniff::{Caption as PageCaption, SniffMode, SniffResult, Sniffer, StreamKind};
use flox_core::tools::{find_for_tests, Tool};
use flox_rip::job::{Job, JobState, JobView, Source};
use flox_rip::queue::{Queue, QueueDeps, QueueHooks, QueueOptions};
use flox_rip::tools::ToolPaths;
use flox_rip::{process, temp};
use flox_td::caption::{encode, Caption};
use flox_td::transport::TdTransport;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT: i64 = -100_777;
const MOVIE: u64 = 603;

fn tools() -> Option<ToolPaths> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let found = (
        find_for_tests(Tool::Ffmpeg, &repo),
        find_for_tests(Tool::Ffprobe, &repo),
    );
    if let (Some(ffmpeg), Some(ffprobe)) = found {
        Some(ToolPaths {
            ffmpeg,
            ffprobe,
            ytdlp: None,
        })
    } else {
        eprintln!("skipped: ffmpeg or ffprobe not found (set FLOX_FFMPEG / FLOX_FFPROBE or put them on PATH)");
        None
    }
}

/// A 3 s 320x240 H.264 + AAC clip.
async fn clip(ffmpeg: &Path, dir: &Path) -> Vec<u8> {
    let out = dir.join("clip.mp4");
    let args: Vec<OsString> = [
        "-y",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc=duration=3:size=320x240:rate=25",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=3",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-shortest",
    ]
    .iter()
    .map(OsString::from)
    .chain(std::iter::once(out.clone().into_os_string()))
    .collect();
    process::run(ffmpeg, &args, |_| {}, CancellationToken::new())
        .await
        .unwrap();
    std::fs::read(&out).unwrap()
}

/// One `sendMessage` the fake saw.
#[derive(Clone, Debug)]
struct Sent {
    caption: String,
    reply_to: Option<i64>,
    /// The server id the fake answered with (0 when it failed the send).
    final_id: i64,
    /// Hard links to the uploaded file at send time.
    links: u64,
}

#[cfg(unix)]
fn link_count(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).map(|m| m.nlink()).unwrap_or(0)
}

#[cfg(not(unix))]
fn link_count(_: &Path) -> u64 {
    0
}

/// A scripted TDLib: one channel, a canned library, uploads that finish at once.
struct FakeTd {
    updates: broadcast::Sender<Arc<Value>>,
    library: Vec<Value>,
    fail_sends: AtomicU32,
    next: AtomicI64,
    sent: Mutex<Vec<Sent>>,
    deleted: Mutex<Vec<Vec<i64>>>,
}

impl FakeTd {
    fn new(library: Vec<Value>, fail_sends: u32) -> Arc<Self> {
        Arc::new(Self {
            updates: broadcast::channel(256).0,
            library,
            fail_sends: AtomicU32::new(fail_sends),
            next: AtomicI64::new(0),
            sent: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
        })
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().clone()
    }
}

#[async_trait]
impl TdTransport for FakeTd {
    async fn request(&self, req: Value) -> Result<Value> {
        match req["@type"].as_str().unwrap_or_default() {
            "loadChats" => Err(Error::Td {
                code: 404,
                message: "Not Found".into(),
            }),
            "getChats" => Ok(json!({"@type": "chats", "chat_ids": [CHAT]})),
            "getChat" => Ok(json!({"@type": "chat", "id": CHAT, "title": "Flox Library"})),
            "searchChatMessages" => Ok(json!({
                "@type": "foundChatMessages",
                "messages": self.library,
                "next_from_message_id": 0,
            })),
            "deleteMessages" => {
                let ids = req["message_ids"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default();
                self.deleted.lock().push(ids);
                // Long enough for a snapshot reader to see the "replacing" detail.
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(json!({"@type": "ok"}))
            }
            "sendMessage" => {
                let content = &req["input_message_content"];
                let file = PathBuf::from(
                    content["document"]["document"]["path"]
                        .as_str()
                        .unwrap_or_default(),
                );
                let mut sent = Sent {
                    caption: content["caption"]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    reply_to: req["reply_to"]["message_id"].as_i64(),
                    final_id: 0,
                    links: link_count(&file),
                };
                let fail = self
                    .fail_sends
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok();
                if fail {
                    self.sent.lock().push(sent);
                    return Err(Error::Td {
                        code: 400,
                        message: "FILE_PARTS_INVALID".into(),
                    });
                }
                let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
                let (temp_id, file_id, final_id) = (-n, 7000 + n, 1000 + n);
                sent.final_id = final_id;
                self.sent.lock().push(sent);
                let tx = self.updates.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = tx.send(Arc::new(json!({"@type": "updateFile", "file": {
                        "@type": "file", "id": file_id, "size": 1000, "expected_size": 1000,
                        "remote": {"@type": "remoteFile", "uploaded_size": 500}
                    }})));
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = tx.send(Arc::new(json!({
                        "@type": "updateMessageSendSucceeded",
                        "old_message_id": temp_id,
                        "message": {"@type": "message", "id": final_id, "chat_id": CHAT}
                    })));
                });
                Ok(json!({
                    "@type": "message",
                    "id": temp_id,
                    "chat_id": CHAT,
                    "sending_state": {"@type": "messageSendingStatePending"},
                    "content": {"@type": "messageDocument", "document": {
                        "@type": "document",
                        "document": {"@type": "file", "id": file_id, "size": 1000}
                    }}
                }))
            }
            other => Err(Error::Other(format!("fake: unexpected {other}"))),
        }
    }

    fn updates(&self) -> broadcast::Receiver<Arc<Value>> {
        self.updates.subscribe()
    }
}

/// Serves one sniff result.
struct FakeSniffer(SniffResult);

#[async_trait]
impl Sniffer for FakeSniffer {
    async fn sniff(&self, _: &str, mode: SniffMode, _: CancellationToken) -> Result<SniffResult> {
        assert_eq!(mode, SniffMode::Rip);
        // Long enough for a snapshot reader to see "Resolving stream".
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(self.0.clone())
    }
}

#[derive(Default)]
struct Hooks(Mutex<Vec<String>>);

impl QueueHooks for Hooks {
    fn busy_changed(&self, busy: bool) {
        self.0.lock().push(format!("busy:{busy}"));
    }
    fn drained(&self, any_failed: bool) {
        self.0.lock().push(format!("drained:{any_failed}"));
    }
}

struct Rig {
    queue: Arc<Queue>,
    td: Arc<FakeTd>,
    hooks: Arc<Hooks>,
    temp_root: PathBuf,
    /// Every (state, detail) the snapshot showed for the first job.
    seen: Arc<Mutex<Vec<(JobState, String)>>>,
    _dir: tempfile::TempDir,
}

fn rig(
    tools: ToolPaths,
    td: Arc<FakeTd>,
    sniffer: Option<Arc<dyn Sniffer>>,
    part_size: Option<u64>,
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let temp_root = dir.path().join("flox");
    let hooks = Arc::new(Hooks::default());
    let deps = QueueDeps {
        td: td.clone(),
        sniffer,
        tools,
        temp_root: temp_root.clone(),
        settings: SettingsStore::new(dir.path().join("settings.json"), Settings::default()),
        hooks: hooks.clone(),
    };
    let mut options = QueueOptions::default();
    if let Some(p) = part_size {
        options.part_size = p;
    }
    let queue = Queue::with_options(deps, options);
    let seen: Arc<Mutex<Vec<(JobState, String)>>> = Arc::default();
    let mut rx = queue.snapshot();
    let log = seen.clone();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let first = rx.borrow_and_update().first().cloned();
            if let Some(v) = first {
                let entry = (v.state, v.detail);
                let mut log = log.lock();
                if log.last() != Some(&entry) {
                    log.push(entry);
                }
            }
        }
    });
    Rig {
        queue,
        td,
        hooks,
        temp_root,
        seen,
        _dir: dir,
    }
}

impl Rig {
    /// Waits for the `drained` hook, then returns the job list.
    async fn drained(&self) -> Vec<JobView> {
        let start = Instant::now();
        while !self.hooks.0.lock().iter().any(|h| h.starts_with("drained")) {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "queue did not drain"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.queue.snapshot().borrow().clone()
    }

    async fn wait_for(&self, what: &str, f: impl Fn(&[JobView]) -> bool) {
        let start = Instant::now();
        while !f(&self.queue.snapshot().borrow()) {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "timed out: {what}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

async fn serve_clip(server: &MockServer, bytes: &[u8]) -> String {
    Mock::given(method("GET"))
        .and(path("/clip.mp4"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(bytes.to_vec(), "video/mp4"))
        .mount(server)
        .await;
    format!("{}/clip.mp4", server.uri())
}

fn movie_job(source: Source) -> Job {
    Job::new(EpisodeKey::movie(MOVIE), "Film", source, None)
}

fn caption(part: u32, parts: u32) -> String {
    encode(&Caption::new(
        EpisodeKey::movie(MOVIE),
        "240p",
        "h264",
        part,
        parts,
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn link_job_splits_in_three_and_reaches_done() {
    let Some(tools) = tools() else { return };
    let work = tempfile::tempdir().unwrap();
    let bytes = clip(&tools.ffmpeg, work.path()).await;
    let server = MockServer::start().await;
    let url = serve_clip(&server, &bytes).await;
    let part_size = (bytes.len() as u64).div_ceil(3);
    let r = rig(tools, FakeTd::new(Vec::new(), 0), None, Some(part_size));
    let job = movie_job(Source::Link(url));
    let id = job.id;
    r.queue.add(vec![job]);

    let views = r.drained().await;
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].state, JobState::Done);
    assert_eq!(views[0].detail, "uploaded");
    assert_eq!(views[0].attempt, 0);

    let mut captions: Vec<String> = r.td.sent().into_iter().map(|s| s.caption).collect();
    captions.sort();
    assert_eq!(captions, vec![caption(1, 3), caption(2, 3), caption(3, 3)]);
    assert!(r.td.sent().iter().all(|s| s.reply_to.is_none()));
    assert!(r
        .seen
        .lock()
        .contains(&(JobState::Uploading, "3 parts at once".to_string())));
    assert!(r
        .seen
        .lock()
        .contains(&(JobState::Downloading, "clip.mp4".to_string())));
    assert_eq!(
        *r.hooks.0.lock(),
        vec!["busy:true", "busy:false", "drained:false"]
    );
    assert!(!temp::job_dir(&r.temp_root, id).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_job_uploads_the_english_caption_as_a_reply() {
    let Some(tools) = tools() else { return };
    let work = tempfile::tempdir().unwrap();
    let bytes = clip(&tools.ffmpeg, work.path()).await;
    let server = MockServer::start().await;
    let url = serve_clip(&server, &bytes).await;
    Mock::given(method("GET"))
        .and(path("/en.srt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("1\n00:00:00,000 --> 00:00:01,000\nHi\n"),
        )
        .mount(&server)
        .await;
    let sniffed = SniffResult {
        url,
        kind: StreamKind::File,
        headers: vec![("Origin".into(), "https://vidlink.pro".into())],
        captions: vec![
            PageCaption {
                url: format!("{}/fr.srt", server.uri()),
                language: "fr".into(),
                kind: "srt".into(),
            },
            PageCaption {
                url: format!("{}/en.srt", server.uri()),
                language: "English".into(),
                kind: "srt".into(),
            },
        ],
    };
    let r = rig(
        tools,
        FakeTd::new(Vec::new(), 0),
        Some(Arc::new(FakeSniffer(sniffed))),
        None,
    );
    r.queue.add(vec![movie_job(Source::Page)]);

    let views = r.drained().await;
    assert_eq!(views[0].state, JobState::Done);
    let sent = r.td.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].caption, caption(1, 1));
    assert_eq!(sent[1].caption, "");
    assert_eq!(sent[1].reply_to, Some(sent[0].final_id));
    assert!(r.seen.lock().iter().any(|(s, _)| *s == JobState::Resolving));
}

/// A TDLib 1.8.67 document message.
fn doc_msg(id: i64, name: &str, caption: &str, reply_to: Option<i64>) -> Value {
    let mut m = json!({
        "@type": "message",
        "id": id,
        "chat_id": CHAT,
        "reply_to": null,
        "content": {
            "@type": "messageDocument",
            "document": {
                "@type": "document",
                "file_name": name,
                "document": {"@type": "file", "id": id + 50, "size": 1234, "expected_size": 1234}
            },
            "caption": {"@type": "formattedText", "text": caption, "entities": []}
        }
    });
    if let Some(to) = reply_to {
        m["reply_to"] =
            json!({"@type": "messageReplyToMessage", "chat_id": CHAT, "message_id": to});
    }
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_deletes_the_previous_print() {
    let Some(tools) = tools() else { return };
    let work = tempfile::tempdir().unwrap();
    let bytes = clip(&tools.ffmpeg, work.path()).await;
    let server = MockServer::start().await;
    let url = serve_clip(&server, &bytes).await;
    let other = encode(&Caption::new(
        EpisodeKey::movie(MOVIE),
        "1080p",
        "hevc",
        1,
        1,
    ));
    let library = vec![
        doc_msg(502, "media.mp4", &other, None),
        doc_msg(501, "en.srt", "", Some(500)),
        doc_msg(500, "media.mp4", &caption(1, 1), None),
    ];
    let r = rig(tools, FakeTd::new(library, 0), None, None);
    r.queue.add(vec![movie_job(Source::Link(url))]);

    let views = r.drained().await;
    assert_eq!(views[0].state, JobState::Done);
    assert_eq!(*r.td.deleted.lock(), vec![vec![500, 501]]);
    assert!(r.seen.lock().contains(&(
        JobState::Uploading,
        "replacing previous 240p h264 upload".to_string()
    )));
    assert_eq!(r.td.sent().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_failure_is_retried() {
    let Some(tools) = tools() else { return };
    let work = tempfile::tempdir().unwrap();
    let bytes = clip(&tools.ffmpeg, work.path()).await;
    let server = MockServer::start().await;
    let url = serve_clip(&server, &bytes).await;
    let r = rig(tools, FakeTd::new(Vec::new(), 1), None, None);
    r.queue.add(vec![movie_job(Source::Link(url))]);

    let views = r.drained().await;
    assert_eq!(views[0].state, JobState::Done);
    assert_eq!(views[0].attempt, 1);
    let sent = r.td.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].final_id, 0);
    assert_eq!(sent[1].caption, caption(1, 1));
    assert_eq!(
        *r.hooks.0.lock(),
        vec!["busy:true", "busy:false", "drained:false"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_failure_fails_the_job() {
    let Some(tools) = tools() else { return };
    let work = tempfile::tempdir().unwrap();
    let bytes = clip(&tools.ffmpeg, work.path()).await;
    let server = MockServer::start().await;
    let url = serve_clip(&server, &bytes).await;
    let r = rig(tools, FakeTd::new(Vec::new(), 2), None, None);
    r.queue.add(vec![movie_job(Source::Link(url))]);

    let views = r.drained().await;
    assert_eq!(
        views[0].state,
        JobState::Failed("telegram error 400: FILE_PARTS_INVALID".to_string())
    );
    assert_eq!(views[0].detail, "");
    assert_eq!(views[0].attempt, 2);
    assert_eq!(
        *r.hooks.0.lock(),
        vec!["busy:true", "busy:false", "drained:true"]
    );

    // Retry re-queues with fresh attempts; clear_finished keeps failed jobs.
    r.queue.clear_finished();
    assert_eq!(r.queue.snapshot().borrow().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_download_leaves_no_temp_dir() {
    let Some(tools) = tools() else { return };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(vec![0u8; 1024], "video/mp4")
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&server)
        .await;
    let r = rig(tools, FakeTd::new(Vec::new(), 0), None, None);
    let running = movie_job(Source::Link(format!("{}/slow.mp4", server.uri())));
    let waiting = movie_job(Source::Link(format!("{}/slow.mp4", server.uri())));
    let (running_id, waiting_id) = (running.id, waiting.id);
    r.queue.add(vec![running, waiting]);
    r.wait_for("downloading", |v| v[0].state == JobState::Downloading)
        .await;
    let dir = temp::job_dir(&r.temp_root, running_id);
    assert!(dir.is_dir());

    // A queued job is removed outright.
    r.queue.cancel(waiting_id);
    assert_eq!(r.queue.snapshot().borrow().len(), 1);

    let started = Instant::now();
    r.queue.cancel(running_id);
    assert_eq!(r.queue.snapshot().borrow()[0].detail, "stopping");
    let views = r.drained().await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].state, JobState::Cancelled);
    assert_eq!(views[0].detail, "");
    assert!(!dir.exists());
    assert!(r.td.sent().is_empty());
    assert_eq!(
        *r.hooks.0.lock(),
        vec!["busy:true", "busy:false", "drained:false"]
    );

    // Cancelled jobs are cleared.
    r.queue.clear_finished();
    assert!(r.queue.snapshot().borrow().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_jobs_hard_link_small_files_and_copy_large_ones() {
    let Some(tools) = tools() else { return };
    let work = tempfile::tempdir().unwrap();
    let bytes = clip(&tools.ffmpeg, work.path()).await;
    let source = work.path().join("Film.2024.1080p.mp4");
    std::fs::write(&source, &bytes).unwrap();

    // Fits one part: hard-linked, so the upload sees two links to the same file.
    let r = rig(tools.clone(), FakeTd::new(Vec::new(), 0), None, None);
    r.queue.add(vec![movie_job(Source::File(source.clone()))]);
    let views = r.drained().await;
    assert_eq!(views[0].state, JobState::Done);
    let sent = r.td.sent();
    assert_eq!(sent.len(), 1);
    if cfg!(unix) {
        assert_eq!(sent[0].links, 2);
    }

    // Larger than a part: copied, then split without touching the original.
    let part_size = (bytes.len() as u64).div_ceil(2);
    let r = rig(tools, FakeTd::new(Vec::new(), 0), None, Some(part_size));
    r.queue.add(vec![movie_job(Source::File(source.clone()))]);
    let views = r.drained().await;
    assert_eq!(views[0].state, JobState::Done);
    let sent = r.td.sent();
    assert_eq!(sent.len(), 2);
    if cfg!(unix) {
        assert!(sent.iter().all(|s| s.links == 1));
    }
    assert!(r.seen.lock().contains(&(
        JobState::Downloading,
        "copying Film.2024.1080p.mp4".to_string()
    )));
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_file_fails_after_two_attempts() {
    let Some(tools) = tools() else { return };
    let r = rig(tools, FakeTd::new(Vec::new(), 0), None, None);
    let missing = PathBuf::from("/nonexistent/flox/Film.mp4");
    r.queue.add(vec![movie_job(Source::File(missing.clone()))]);
    let views = r.drained().await;
    assert_eq!(
        views[0].state,
        JobState::Failed(format!("file not found: {}", missing.display()))
    );

    // Retry starts it again from scratch.
    r.hooks.0.lock().clear();
    let id = views[0].job.id;
    r.queue.retry(id);
    assert_eq!(r.queue.snapshot().borrow()[0].attempt, 0);
    let views = r.drained().await;
    assert!(matches!(views[0].state, JobState::Failed(_)));
    assert_eq!(views[0].attempt, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_job_pulls_hls_with_ffmpeg() {
    let Some(tools) = tools() else { return };
    let work = tempfile::tempdir().unwrap();
    clip(&tools.ffmpeg, work.path()).await;
    let hls = work.path().join("hls");
    std::fs::create_dir_all(&hls).unwrap();
    let args: Vec<OsString> = [
        "-y",
        "-loglevel",
        "error",
        "-i",
        &work.path().join("clip.mp4").to_string_lossy(),
        "-c",
        "copy",
        "-hls_time",
        "1",
        "-hls_list_size",
        "0",
        "-f",
        "hls",
        &hls.join("index.m3u8").to_string_lossy(),
    ]
    .iter()
    .map(OsString::from)
    .collect();
    process::run(&tools.ffmpeg, &args, |_| {}, CancellationToken::new())
        .await
        .unwrap();
    let server = MockServer::start().await;
    for entry in std::fs::read_dir(&hls).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let mime = if name.ends_with(".m3u8") {
            "application/vnd.apple.mpegurl"
        } else {
            "video/mp2t"
        };
        Mock::given(method("GET"))
            .and(path(format!("/hls/{name}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(std::fs::read(entry.path()).unwrap(), mime),
            )
            .mount(&server)
            .await;
    }
    let sniffed = SniffResult {
        url: format!("{}/hls/index.m3u8", server.uri()),
        kind: StreamKind::Hls,
        headers: Vec::new(),
        captions: Vec::new(),
    };
    let r = rig(
        tools,
        FakeTd::new(Vec::new(), 0),
        Some(Arc::new(FakeSniffer(sniffed))),
        None,
    );
    r.queue.add(vec![movie_job(Source::Page)]);

    let views = r.drained().await;
    assert_eq!(views[0].state, JobState::Done, "{views:?}");
    let sent = r.td.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].caption, caption(1, 1));
    let requests = server.received_requests().await.unwrap_or_default();
    let playlist = requests
        .iter()
        .find(|q| q.url.path().ends_with(".m3u8"))
        .unwrap();
    let agent = playlist
        .headers
        .get("user-agent")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(agent, flox_rip::download::file::USER_AGENT);
    let referer = playlist.headers.get("referer").unwrap().to_str().unwrap();
    assert_eq!(referer, "https://vidlink.pro/");
}
