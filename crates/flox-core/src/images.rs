//! Poster and still cache: memory LRU of decoded RGBA plus a disk cache of the
//! encoded bytes named by `sha1(url)`.
//!
//! Mirrors the Android `ImageLoader.kt`: at most four fetches run at once, a
//! download that misses the disk is written as `<dir>/<sha1 hex>`, and every
//! 16th write trims the directory to 75% of its budget, oldest mtime first.
//! A decoded file has its mtime refreshed so trimming behaves as an LRU.
//! Decoding (JPEG and PNG) runs on the blocking pool; the UI converts the
//! returned [`Rgba`] into a toolkit image on its own thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use lru::LruCache;
use parking_lot::Mutex;
use sha1::{Digest, Sha1};
use tokio::sync::{OnceCell, Semaphore};

use crate::error::{Error, Result};

/// Default memory budget for decoded images (desktop; Android used min(maxMem/8, 24 MB)).
pub const MEM_BYTES: usize = 96 * 1024 * 1024;
/// Default disk budget.
pub const DISK_BYTES: u64 = 40 * 1024 * 1024;
/// The disk cache is trimmed to this percentage of its budget, oldest mtime first.
pub const TRIM_TO_PERCENT: u64 = 75;
/// The disk trim runs every this many writes.
pub const TRIM_EVERY_WRITES: u32 = 16;
/// At most this many downloads run at once.
pub const MAX_CONCURRENT_DOWNLOADS: usize = 4;
/// Connect and read timeouts for image downloads.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10);

/// Suffix of in-progress downloads; trimming ignores these files.
const TMP_SUFFIX: &str = ".tmp";

/// A decoded image, 8-bit RGBA, row-major, no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The disk file name for `url`: the lowercase hex SHA-1 of its UTF-8 bytes.
pub fn cache_file_name(url: &str) -> String {
    let digest = Sha1::digest(url.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Trims `dir` when its files add up to more than `cap` bytes, deleting the
/// oldest by mtime until the total is at most [`TRIM_TO_PERCENT`] of `cap`.
/// In-progress downloads are skipped. Returns the total size left.
pub fn trim_disk(dir: &Path, cap: u64) -> std::io::Result<u64> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().ends_with(TMP_SUFFIX) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        files.push((mtime, meta.len(), entry.path()));
    }
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= cap {
        return Ok(total);
    }
    let target = cap / 100 * TRIM_TO_PERCENT + cap % 100 * TRIM_TO_PERCENT / 100;
    files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    for (_, len, path) in files {
        if total <= target {
            break;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => total = total.saturating_sub(len),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => total = total.saturating_sub(len),
            Err(e) => tracing::warn!("image cache: cannot remove {}: {e}", path.display()),
        }
    }
    Ok(total)
}

struct Memory {
    lru: LruCache<String, Arc<Rgba>>,
    bytes: usize,
}

/// The image cache. Share it behind an `Arc`.
pub struct ImageCache {
    dir: PathBuf,
    mem_bytes: usize,
    disk_bytes: u64,
    memory: Mutex<Memory>,
    gate: Semaphore,
    writes: AtomicU32,
    tmp_seq: AtomicU64,
    http: OnceCell<reqwest::Client>,
    active_downloads: AtomicUsize,
    peak_downloads: AtomicUsize,
}

impl ImageCache {
    /// A cache rooted at `dir` with the given budgets. Nothing touches the disk until `get`.
    pub fn new(dir: PathBuf, mem_bytes: usize, disk_bytes: u64) -> Self {
        Self {
            dir,
            mem_bytes,
            disk_bytes,
            memory: Mutex::new(Memory {
                lru: LruCache::unbounded(),
                bytes: 0,
            }),
            gate: Semaphore::new(MAX_CONCURRENT_DOWNLOADS),
            writes: AtomicU32::new(0),
            tmp_seq: AtomicU64::new(0),
            http: OnceCell::new(),
            active_downloads: AtomicUsize::new(0),
            peak_downloads: AtomicUsize::new(0),
        }
    }

    /// The disk directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Memory, then disk, then network; decoded off the calling task.
    pub async fn get(&self, url: &str) -> Result<Arc<Rgba>> {
        if let Some(img) = self.memory.lock().lru.get(url) {
            return Ok(img.clone());
        }
        let permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| Error::Other("image cache is closed".into()))?;
        // Another fetch of the same URL may have finished while this one waited.
        if let Some(img) = self.memory.lock().lru.get(url) {
            return Ok(img.clone());
        }

        let file = self.dir.join(cache_file_name(url));
        let cached = tokio::fs::metadata(&file)
            .await
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if !cached {
            self.download(url, &file).await?;
            let n = self.writes.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            if n.is_multiple_of(TRIM_EVERY_WRITES) {
                let dir = self.dir.clone();
                let cap = self.disk_bytes;
                match tokio::task::spawn_blocking(move || trim_disk(&dir, cap)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => tracing::warn!("image cache: trim failed: {e}"),
                    Err(e) => tracing::warn!("image cache: trim task failed: {e}"),
                }
            }
        }

        let img = tokio::task::spawn_blocking(move || decode_file(&file))
            .await
            .map_err(|e| Error::Image(format!("decode task failed: {e}")))??;
        drop(permit);
        let img = Arc::new(img);
        self.remember(url, img.clone());
        Ok(img)
    }

    /// Entries and decoded bytes currently held in memory.
    pub fn memory_usage(&self) -> (usize, usize) {
        let m = self.memory.lock();
        (m.lru.len(), m.bytes)
    }

    /// Drops every decoded image from memory (the disk cache is kept).
    pub fn clear_memory(&self) {
        let mut m = self.memory.lock();
        m.lru.clear();
        m.bytes = 0;
    }

    fn remember(&self, url: &str, img: Arc<Rgba>) {
        let size = img.pixels.len();
        if size > self.mem_bytes {
            return;
        }
        let mut m = self.memory.lock();
        if let Some(old) = m.lru.put(url.to_owned(), img) {
            m.bytes = m.bytes.saturating_sub(old.pixels.len());
        }
        m.bytes += size;
        while m.bytes > self.mem_bytes {
            match m.lru.pop_lru() {
                Some((_, old)) => m.bytes = m.bytes.saturating_sub(old.pixels.len()),
                None => {
                    m.bytes = 0;
                    break;
                }
            }
        }
    }

    async fn client(&self) -> Result<&reqwest::Client> {
        self.http
            .get_or_try_init(|| async {
                reqwest::Client::builder()
                    .connect_timeout(DOWNLOAD_TIMEOUT)
                    .read_timeout(DOWNLOAD_TIMEOUT)
                    .build()
                    .map_err(Error::Http)
            })
            .await
    }

    async fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let active = self.active_downloads.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_downloads.fetch_max(active, Ordering::SeqCst);
        let result = self.download_inner(url, dest).await;
        self.active_downloads.fetch_sub(1, Ordering::SeqCst);
        result
    }

    async fn download_inner(&self, url: &str, dest: &Path) -> Result<()> {
        let client = self.client().await?;
        let resp = client.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Image(format!("HTTP {} for {url}", status.as_u16())));
        }
        let bytes = resp.bytes().await?;
        if bytes.is_empty() {
            return Err(Error::Image(format!("empty body for {url}")));
        }
        tokio::fs::create_dir_all(&self.dir).await?;
        let seq = self.tmp_seq.fetch_add(1, Ordering::Relaxed);
        let mut tmp = dest.as_os_str().to_owned();
        tmp.push(format!(".{seq}{TMP_SUFFIX}"));
        let tmp = PathBuf::from(tmp);
        let written = async {
            tokio::fs::write(&tmp, &bytes).await?;
            tokio::fs::rename(&tmp, dest).await
        }
        .await;
        if let Err(e) = written {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e.into());
        }
        Ok(())
    }

    #[cfg(test)]
    fn peak_downloads(&self) -> usize {
        self.peak_downloads.load(Ordering::SeqCst)
    }
}

/// Reads and decodes `file`. An undecodable file is deleted; a good one has its
/// mtime refreshed so the disk trim keeps recently used images.
fn decode_file(file: &Path) -> Result<Rgba> {
    let bytes = std::fs::read(file)?;
    match image::load_from_memory(&bytes) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            if let Ok(f) = std::fs::File::options().write(true).open(file) {
                let _ = f.set_modified(SystemTime::now());
            }
            Ok(Rgba {
                width: rgba.width(),
                height: rgba.height(),
                pixels: rgba.into_raw(),
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(file);
            Err(Error::Image(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_image(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x * 40 % 256) as u8, (y * 60 % 256) as u8, 7, 255])
        })
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        test_image(w, h)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn png_response(bytes: &[u8]) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(bytes.to_vec(), "image/png")
    }

    async fn requests(server: &MockServer) -> usize {
        server.received_requests().await.unwrap_or_default().len()
    }

    #[test]
    fn get_is_send_and_the_cache_is_sync() {
        fn send<T: Send>(_: &T) {}
        fn sync<T: Send + Sync>() {}
        sync::<ImageCache>();
        let cache = ImageCache::new(PathBuf::from("unused"), 1, 1);
        send(&cache.get("http://127.0.0.1:9/x"));
    }

    #[test]
    fn file_names_are_sha1_hex() {
        assert_eq!(
            cache_file_name(""),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            cache_file_name("abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[tokio::test]
    async fn downloads_decodes_and_names_the_file_by_sha1() {
        let server = MockServer::start().await;
        let body = png(4, 3);
        Mock::given(method("GET"))
            .and(path("/t/p/w342/a.png"))
            .respond_with(png_response(&body))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("images");
        let url = format!("{}/t/p/w342/a.png", server.uri());

        let cache = ImageCache::new(cache_dir.clone(), MEM_BYTES, DISK_BYTES);
        let img = cache.get(&url).await.unwrap();
        assert_eq!((img.width, img.height), (4, 3));
        assert_eq!(img.pixels, test_image(4, 3).into_raw());
        let file = cache_dir.join(cache_file_name(&url));
        assert_eq!(std::fs::read(&file).unwrap(), body);
        assert_eq!(std::fs::read_dir(&cache_dir).unwrap().count(), 1);

        // Memory hit: the same Arc, no new request.
        let again = cache.get(&url).await.unwrap();
        assert!(Arc::ptr_eq(&img, &again));

        // A fresh cache over the same directory reads the disk, not the network.
        let cold = ImageCache::new(cache_dir, MEM_BYTES, DISK_BYTES);
        assert_eq!(*cold.get(&url).await.unwrap(), *img);
        assert_eq!(requests(&server).await, 1);
    }

    #[tokio::test]
    async fn decodes_jpeg() {
        let server = MockServer::start().await;
        let mut jpeg = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(test_image(8, 8))
            .into_rgb8()
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .unwrap();
        Mock::given(path("/s.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(jpeg.into_inner(), "image/jpeg"))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().to_path_buf(), MEM_BYTES, DISK_BYTES);
        let img = cache.get(&format!("{}/s.jpg", server.uri())).await.unwrap();
        assert_eq!((img.width, img.height), (8, 8));
        assert_eq!(img.pixels.len(), 8 * 8 * 4);
    }

    #[tokio::test]
    async fn undecodable_bytes_fail_and_leave_no_file() {
        let server = MockServer::start().await;
        Mock::given(path("/bad"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(b"not an image".to_vec(), "image/png"),
            )
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().to_path_buf(), MEM_BYTES, DISK_BYTES);
        let err = cache
            .get(&format!("{}/bad", server.uri()))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Image(_)), "{err:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn http_errors_fail_and_leave_no_file() {
        let server = MockServer::start().await;
        Mock::given(path("/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"), MEM_BYTES, DISK_BYTES);
        let err = cache
            .get(&format!("{}/missing", server.uri()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("HTTP 404"), "{err}");
        assert!(!dir.path().join("images").exists());
    }

    #[tokio::test]
    async fn memory_lru_evicts_by_decoded_bytes() {
        let server = MockServer::start().await;
        let body = png(4, 3);
        Mock::given(path_regex("^/img/.*$"))
            .respond_with(png_response(&body))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let one = 4 * 3 * 4;
        let cache = ImageCache::new(dir.path().to_path_buf(), 2 * one, DISK_BYTES);
        let url = |n: u32| format!("{}/img/{n}.png", server.uri());

        cache.get(&url(1)).await.unwrap();
        cache.get(&url(2)).await.unwrap();
        assert_eq!(cache.memory_usage(), (2, 2 * one));
        cache.get(&url(1)).await.unwrap(); // 1 becomes most recent
        cache.get(&url(3)).await.unwrap(); // evicts 2
        assert_eq!(cache.memory_usage(), (2, 2 * one));
        assert_eq!(requests(&server).await, 3);

        // 2 comes back from disk, not the network.
        cache.get(&url(2)).await.unwrap();
        assert_eq!(requests(&server).await, 3);
        assert_eq!(cache.memory_usage(), (2, 2 * one));

        // An image larger than the whole budget is returned but not kept.
        let tiny = ImageCache::new(dir.path().to_path_buf(), one - 1, DISK_BYTES);
        tiny.get(&url(1)).await.unwrap();
        assert_eq!(tiny.memory_usage(), (0, 0));
    }

    #[tokio::test]
    async fn at_most_four_downloads_run_at_once() {
        let server = MockServer::start().await;
        let body = png(2, 2);
        Mock::given(path_regex("^/slow/.*$"))
            .respond_with(png_response(&body).set_delay(Duration::from_millis(250)))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().to_path_buf(), MEM_BYTES, DISK_BYTES);
        let urls: Vec<String> = (0..12)
            .map(|n| format!("{}/slow/{n}.png", server.uri()))
            .collect();
        let started = std::time::Instant::now();
        let results = futures::future::join_all(urls.iter().map(|u| cache.get(u))).await;
        assert!(results.iter().all(|r| r.is_ok()));
        assert_eq!(cache.peak_downloads(), MAX_CONCURRENT_DOWNLOADS);
        // 12 downloads through 4 slots take at least three rounds.
        assert!(started.elapsed() >= Duration::from_millis(3 * 250));
        assert_eq!(requests(&server).await, 12);
    }

    fn set_age(file: &Path, secs_ago: u64) {
        let f = std::fs::File::options().write(true).open(file).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
            .unwrap();
    }

    #[test]
    fn trim_leaves_a_directory_under_budget_alone() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..4 {
            std::fs::write(dir.path().join(format!("f{n}")), [0u8; 100]).unwrap();
        }
        assert_eq!(trim_disk(dir.path(), 400).unwrap(), 400);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
    }

    #[test]
    fn trim_removes_oldest_to_75_percent_and_skips_tmp() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..10u64 {
            let f = dir.path().join(format!("f{n}"));
            std::fs::write(&f, [0u8; 100]).unwrap();
            set_age(&f, 1000 - n * 10); // f0 oldest, f9 newest
        }
        let tmp = dir.path().join(format!("partial.1{TMP_SUFFIX}"));
        std::fs::write(&tmp, [0u8; 500]).unwrap();
        set_age(&tmp, 5000);

        // 1000 bytes over an 800 cap: trim to 600, dropping f0..f3.
        assert_eq!(trim_disk(dir.path(), 800).unwrap(), 600);
        for n in 0..10 {
            assert_eq!(dir.path().join(format!("f{n}")).exists(), n >= 4, "f{n}");
        }
        assert!(tmp.exists());
    }

    #[tokio::test]
    async fn every_16th_write_trims_the_disk_by_mtime() {
        let server = MockServer::start().await;
        let body = png(4, 3);
        let p = body.len() as u64;
        Mock::given(path_regex("^/img/.*$"))
            .respond_with(png_response(&body))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        // 20 old files of 100 bytes, old0 the oldest.
        for n in 0..20u64 {
            let f = dir.path().join(format!("old{n:02}"));
            std::fs::write(&f, [0u8; 100]).unwrap();
            set_age(&f, 10_000 - n * 10);
        }
        let cap = 16 * p + 1200;
        let target = cap * TRIM_TO_PERCENT / 100;
        assert!(16 * p <= target, "fixture PNG too large: {p} bytes");
        let cache = ImageCache::new(dir.path().to_path_buf(), MEM_BYTES, cap);
        let url = |n: u32| format!("{}/img/{n}.png", server.uri());

        for n in 0..15 {
            cache.get(&url(n)).await.unwrap();
        }
        // 15 writes: over budget but no trim yet.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 35);

        cache.get(&url(15)).await.unwrap();
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let total: u64 = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= target, "{total} > {target}");
        // Every fresh download survives; the newest old files fill the rest.
        for n in 0..16 {
            assert!(
                left.contains(&cache_file_name(&url(n))),
                "download {n} trimmed"
            );
        }
        let keep = ((target - 16 * p) / 100).min(20);
        for n in 0..20u64 {
            let name = format!("old{n:02}");
            assert_eq!(left.contains(&name), n >= 20 - keep, "{name}");
        }
    }
}
