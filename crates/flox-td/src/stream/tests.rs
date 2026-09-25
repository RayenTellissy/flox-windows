use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use async_trait::async_trait;

use super::*;

/// One file in the simulated TDLib store.
struct FakeFile {
    data: Vec<u8>,
    download_offset: i64,
    prefix: i64,
    active: bool,
    completed: bool,
    /// Bumped whenever the running download is replaced or stopped.
    generation: u64,
}

impl FakeFile {
    fn size(&self) -> i64 {
        self.data.len() as i64
    }

    fn json(&self, id: i32) -> Value {
        json!({
            "@type": "file",
            "id": id,
            "size": self.size(),
            "local": {
                "@type": "localFile",
                "download_offset": self.download_offset,
                "downloaded_prefix_size": self.prefix,
                "is_downloading_active": self.active,
                "is_downloading_completed": self.completed,
            },
        })
    }
}

/// A transport simulating TDLib's file store: downloads advance on a timer
/// and report `updateFile`, `readFilePart` serves in-memory bytes only once
/// they are downloaded, and every request is recorded.
struct Fake {
    files: Mutex<HashMap<i32, FakeFile>>,
    updates: broadcast::Sender<Arc<Value>>,
    requests: Mutex<Vec<Value>>,
    /// Downloads start but never make progress.
    stalled: AtomicBool,
}

/// Bytes a simulated download advances per millisecond.
const STEP: i64 = 16;

impl Fake {
    fn new(parts: &[(i32, Vec<u8>)]) -> Arc<Fake> {
        let (updates, _) = broadcast::channel(4096);
        let files = parts
            .iter()
            .map(|(id, data)| {
                (
                    *id,
                    FakeFile {
                        data: data.clone(),
                        download_offset: 0,
                        prefix: 0,
                        active: false,
                        completed: false,
                        generation: 0,
                    },
                )
            })
            .collect();
        Arc::new(Fake {
            files: Mutex::new(files),
            updates,
            requests: Mutex::new(Vec::new()),
            stalled: AtomicBool::new(false),
        })
    }

    fn push_file(&self, id: i32, f: &FakeFile) {
        let _ = self
            .updates
            .send(Arc::new(json!({"@type": "updateFile", "file": f.json(id)})));
    }

    fn sent(&self, kind: &str) -> Vec<Value> {
        self.requests
            .lock()
            .iter()
            .filter(|r| r["@type"] == kind)
            .cloned()
            .collect()
    }

    /// `downloadFile` requests for `id` at `priority`, as `(offset, limit)`.
    fn downloads(&self, id: i32, priority: i32) -> Vec<(i64, i64)> {
        self.sent("downloadFile")
            .iter()
            .filter(|r| r["file_id"] == id && r["priority"] == priority)
            .map(|r| (r["offset"].as_i64().unwrap(), r["limit"].as_i64().unwrap()))
            .collect()
    }

    fn ids(&self, kind: &str) -> Vec<i32> {
        self.sent(kind)
            .iter()
            .map(|r| r["file_id"].as_i64().unwrap() as i32)
            .collect()
    }

    fn start_download(self: &Arc<Self>, id: i32, offset: i64, limit: i64) -> Result<Value> {
        let mut files = self.files.lock();
        let f = files.get_mut(&id).ok_or_else(|| Error::Td {
            code: 400,
            message: "no such file".into(),
        })?;
        f.generation += 1;
        f.download_offset = offset;
        f.prefix = 0;
        f.active = true;
        let generation = f.generation;
        let end = if limit == 0 {
            f.size()
        } else {
            (offset + limit).min(f.size())
        };
        let r = f.json(id);
        if !self.stalled.load(Ordering::SeqCst) {
            let me = Arc::clone(self);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    let mut files = me.files.lock();
                    let Some(f) = files.get_mut(&id) else {
                        return;
                    };
                    if f.generation != generation {
                        return;
                    }
                    f.prefix = (f.prefix + STEP).min(end - offset);
                    let done = offset + f.prefix >= end;
                    if done {
                        f.active = false;
                        f.completed = offset == 0 && end == f.size();
                    }
                    me.push_file(id, f);
                    if done {
                        return;
                    }
                }
            });
        }
        Ok(r)
    }

    fn read_part(&self, id: i32, offset: i64, count: i64) -> Result<Value> {
        let files = self.files.lock();
        let f = files.get(&id).ok_or_else(|| Error::Td {
            code: 400,
            message: "no such file".into(),
        })?;
        let covered = f.completed
            || (f.download_offset <= offset && offset + count <= f.download_offset + f.prefix);
        if !covered || offset + count > f.size() {
            return Err(Error::Td {
                code: 400,
                message: format!("range {offset}+{count} of file {id} is not available"),
            });
        }
        let bytes = &f.data[offset as usize..(offset + count) as usize];
        Ok(json!({
            "@type": "filePart",
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
        }))
    }
}

/// `TdTransport` over `Arc<Fake>`, so downloads can spawn tasks holding the store.
struct Transport(Arc<Fake>);

#[async_trait]
impl TdTransport for Transport {
    async fn request(&self, req: Value) -> Result<Value> {
        let fake = &self.0;
        fake.requests.lock().push(req.clone());
        let id = req["file_id"].as_i64().unwrap_or(0) as i32;
        let int = |k: &str| req[k].as_i64().unwrap_or(0);
        match req["@type"].as_str().unwrap_or_default() {
            "downloadFile" => fake.start_download(id, int("offset"), int("limit")),
            "readFilePart" => fake.read_part(id, int("offset"), int("count")),
            "cancelDownloadFile" => {
                let mut files = fake.files.lock();
                if let Some(f) = files.get_mut(&id) {
                    f.generation += 1;
                    f.active = false;
                    fake.push_file(id, f);
                }
                Ok(json!({"@type": "ok"}))
            }
            "deleteFile" => {
                let mut files = fake.files.lock();
                if let Some(f) = files.get_mut(&id) {
                    f.generation += 1;
                    f.active = false;
                    f.completed = false;
                    f.prefix = 0;
                    f.download_offset = 0;
                }
                Ok(json!({"@type": "ok"}))
            }
            _ => Ok(json!({"@type": "ok"})),
        }
    }

    fn updates(&self) -> broadcast::Receiver<Arc<Value>> {
        self.0.updates.subscribe()
    }
}

fn cfg() -> Config {
    Config {
        chunk: 16,
        window: 64,
        prefetch_when_left: 32,
        prefetch_bytes: 8,
        coverage_timeout: Duration::from_secs(5),
        read_timeout: Duration::from_secs(5),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// Three parts of 300, 250 and 170 bytes (170 is not a chunk multiple).
fn parts() -> Vec<(i32, Vec<u8>)> {
    [(11, 300usize), (12, 250), (13, 170)]
        .into_iter()
        .map(|(id, n)| {
            let data = (0..n)
                .map(|i| (i as u32 * 7 + id as u32 * 31) as u8)
                .collect();
            (id, data)
        })
        .collect()
}

fn setup(cfg: Config) -> (tokio::runtime::Runtime, Arc<Fake>, TdStream, Vec<u8>) {
    let rt = runtime();
    let ps = parts();
    let fake = Fake::new(&ps);
    let concat = ps.iter().flat_map(|(_, d)| d.clone()).collect();
    let list = ps
        .iter()
        .map(|(id, d)| Part {
            message_id: i64::from(*id) * 100,
            file_id: *id,
            size: d.len() as u64,
        })
        .collect();
    let stream = TdStream::open_with_config(
        Arc::new(Transport(Arc::clone(&fake))),
        rt.handle().clone(),
        list,
        cfg,
    )
    .unwrap();
    (rt, fake, stream, concat)
}

#[test]
fn sequential_read_of_three_parts_equals_the_concatenation() {
    let (_rt, fake, mut s, concat) = setup(cfg());
    assert_eq!(s.size(), 720);
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    assert_eq!(out, concat);

    // Every chunk stays inside its part.
    for r in fake.sent("readFilePart") {
        let size = match r["file_id"].as_i64().unwrap() {
            11 => 300,
            12 => 250,
            _ => 170,
        };
        let (off, count) = (r["offset"].as_i64().unwrap(), r["count"].as_i64().unwrap());
        assert!(count <= 16 && off + count <= size, "{r}");
    }
    // The window moved forward through part 1.
    let windows = fake.downloads(11, 32);
    assert_eq!(windows[0], (0, 64));
    assert!(windows.len() > 1, "{windows:?}");
    assert!(windows.windows(2).all(|w| w[0].0 < w[1].0), "{windows:?}");
}

#[test]
fn seek_into_part_two_issues_the_window_at_the_local_offset() {
    let (_rt, fake, mut s, concat) = setup(cfg());
    assert_eq!(s.seek(SeekFrom::Start(300 + 100)).unwrap(), 400);
    let mut got = [0u8; 10];
    s.read_exact(&mut got).unwrap();
    assert_eq!(&got[..], &concat[400..410]);
    assert_eq!(fake.downloads(12, 32).last(), Some(&(100, 64)));
    assert!(fake.downloads(11, 32).is_empty());
}

#[test]
fn backwards_seek_reissues_the_window() {
    let (_rt, fake, mut s, concat) = setup(cfg());
    let mut got = vec![0u8; 160];
    s.read_exact(&mut got).unwrap();
    assert_eq!(got, concat[..160]);
    let before = fake.downloads(11, 32).len();

    s.seek(SeekFrom::Current(-150)).unwrap();
    let mut again = [0u8; 16];
    s.read_exact(&mut again).unwrap();
    assert_eq!(&again[..], &concat[10..26]);
    let windows = fake.downloads(11, 32);
    assert!(windows.len() > before);
    assert_eq!(windows.last(), Some(&(10, 64)));
}

#[test]
fn prefetch_fires_when_the_part_nears_its_end() {
    let (_rt, fake, mut s, _) = setup(cfg());
    // Chunks start at 256 (44 left) and 272 (28 left, at or under 32).
    let mut got = vec![0u8; 272];
    s.read_exact(&mut got).unwrap();
    assert!(fake.downloads(12, 16).is_empty());

    let mut one = [0u8; 1];
    s.read_exact(&mut one).unwrap();
    assert_eq!(fake.downloads(12, 16), vec![(0, 8)]);

    let mut rest = vec![0u8; 300 - 273];
    s.read_exact(&mut rest).unwrap();
    assert_eq!(
        fake.downloads(12, 16).len(),
        1,
        "prefetch fires once per part"
    );
}

#[test]
fn part_change_cancels_the_old_download() {
    let (_rt, fake, mut s, _) = setup(cfg());
    let mut got = vec![0u8; 300];
    s.read_exact(&mut got).unwrap();
    assert!(fake.ids("cancelDownloadFile").is_empty());

    let mut one = [0u8; 1];
    s.read_exact(&mut one).unwrap();
    assert_eq!(fake.ids("cancelDownloadFile"), vec![11]);

    // Going back into part 1 cancels part 2.
    s.seek(SeekFrom::Start(5)).unwrap();
    s.read_exact(&mut one).unwrap();
    assert_eq!(fake.ids("cancelDownloadFile"), vec![11, 12]);
}

#[test]
fn close_cancels_and_deletes_every_part() {
    let (_rt, fake, mut s, _) = setup(cfg());
    let mut got = [0u8; 40];
    s.read_exact(&mut got).unwrap();
    s.close().unwrap();
    assert_eq!(fake.ids("deleteFile"), vec![11, 12, 13]);
    assert_eq!(fake.ids("cancelDownloadFile"), vec![11, 12, 13]);
}

#[test]
fn a_stalled_download_times_out() {
    let (_rt, fake, mut s, _) = setup(Config {
        coverage_timeout: Duration::from_millis(200),
        ..cfg()
    });
    fake.stalled.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let mut buf = [0u8; 8];
    let err = s.read(&mut buf).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(fake.sent("readFilePart").is_empty());
}

#[test]
fn cancelling_interrupts_a_blocked_read() {
    let (_rt, fake, mut s, _) = setup(Config {
        coverage_timeout: Duration::from_secs(30),
        ..cfg()
    });
    fake.stalled.store(true, Ordering::SeqCst);
    let token = s.cancel_handle();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        token.cancel();
    });
    let started = Instant::now();
    let mut buf = [0u8; 8];
    let err = s.read(&mut buf).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(5));
    canceller.join().unwrap();

    // The stream stays cancelled.
    assert_eq!(
        s.read(&mut buf).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn seeks_resolve_against_start_current_and_end() {
    let (_rt, _fake, mut s, concat) = setup(cfg());
    assert_eq!(s.seek(SeekFrom::End(-4)).unwrap(), 716);
    let mut tail = Vec::new();
    s.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, concat[716..]);
    assert_eq!(s.seek(SeekFrom::End(10)).unwrap(), 730);
    let mut buf = [0u8; 4];
    assert_eq!(s.read(&mut buf).unwrap(), 0);
    assert_eq!(
        s.seek(SeekFrom::Current(-800)).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn coverage_follows_the_download_window() {
    let l = Local {
        download_offset: 100,
        downloaded_prefix_size: 50,
        is_downloading_active: true,
        is_downloading_completed: false,
    };
    assert!(l.covers(100, 50));
    assert!(l.covers(120, 30));
    assert!(!l.covers(99, 10));
    assert!(!l.covers(140, 11));
    let done = Local {
        is_downloading_completed: true,
        ..Local::default()
    };
    assert!(done.covers(1 << 40, 1));
}
