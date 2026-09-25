//! Resumable single-file download with is-a-file detection, as on the Mac.
//!
//! A partial file at `out` is resumed with a `Range` request. The response must
//! look like a media file (see [`is_file`]), otherwise the caller gets
//! [`Error::NotAFile`] and can fall back to another tool.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use flox_core::error::{Error, Result};
use futures::StreamExt;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE, RANGE, USER_AGENT as USER_AGENT_HEADER};
use reqwest::{Client, StatusCode};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::split::PART_SIZE;

/// The browser user agent sent with every download request.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

/// Connect and read-idle timeout.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Upper bound for a whole file transfer.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Bytes collected in memory before each write to disk.
pub const WRITE_BUFFER: usize = 4 << 20;

/// Minimum interval between progress reports.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

const MB: u64 = 1_000_000;
const GB: u64 = 1_000_000_000;

/// The URL served a web page (or other non-media content), not a file.
/// [`download`] reports this case as [`Error::NotAFile`] with the detail.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[error("not a plain file")]
pub struct NotAFile;

impl From<NotAFile> for Error {
    fn from(_: NotAFile) -> Self {
        Error::NotAFile(String::new())
    }
}

/// Downloads `url` to `out` with Range resume. `progress` gets (bytes, total).
pub async fn download(
    url: &str,
    headers: &[(String, String)],
    out: &Path,
    progress: impl Fn(u64, Option<u64>) + Send,
    cancel: CancellationToken,
) -> Result<()> {
    download_with(url, headers, out, progress, cancel, |p: &Path| {
        fs4::available_space(p)
    })
    .await
}

/// [`download`] with the free-space probe injected.
pub(crate) async fn download_with(
    url: &str,
    headers: &[(String, String)],
    out: &Path,
    progress: impl Fn(u64, Option<u64>) + Send,
    cancel: CancellationToken,
    space: impl Fn(&Path) -> io::Result<u64> + Send,
) -> Result<()> {
    tokio::select! {
        _ = cancel.cancelled() => Err(Error::Cancelled),
        r = transfer(url, headers, out, progress, space) => r,
    }
}

/// A client with the idle timeouts and, when given, a total timeout.
pub(crate) fn client(total: Option<Duration>) -> Result<Client> {
    let mut b = Client::builder()
        .connect_timeout(IDLE_TIMEOUT)
        .read_timeout(IDLE_TIMEOUT)
        .no_gzip();
    if let Some(t) = total {
        b = b.timeout(t);
    }
    Ok(b.build()?)
}

/// True when the response headers describe a downloadable file.
pub fn is_file(content_type: &str, content_disposition: &str) -> bool {
    let t = content_type.to_ascii_lowercase();
    let mime = t.split(';').next().unwrap_or_default().trim();
    mime.starts_with("video/")
        || mime == "application/octet-stream"
        || mime.contains("matroska")
        || mime.contains("download")
        || content_disposition
            .to_ascii_lowercase()
            .contains("filename")
}

/// Refuses to start when the volume cannot hold `need` bytes plus one upload part.
fn check_free_space(out: &Path, need: u64, space: impl Fn(&Path) -> io::Result<u64>) -> Result<()> {
    if need == 0 {
        return Ok(());
    }
    let dir = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let Ok(free) = space(dir) else {
        return Ok(());
    };
    let required = need.saturating_add(PART_SIZE);
    if free < required {
        return Err(Error::Other(format!(
            "not enough disk space: need {} GB, have {} GB",
            required / GB,
            free / GB
        )));
    }
    Ok(())
}

fn header_str(r: &reqwest::Response, name: reqwest::header::HeaderName) -> String {
    r.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn transfer(
    url: &str,
    headers: &[(String, String)],
    out: &Path,
    progress: impl Fn(u64, Option<u64>) + Send,
    space: impl Fn(&Path) -> io::Result<u64> + Send,
) -> Result<()> {
    let mut have = match tokio::fs::metadata(out).await {
        Ok(m) if m.is_file() => m.len(),
        _ => 0,
    };
    let mut req = client(Some(TOTAL_TIMEOUT))?
        .get(url)
        .header(USER_AGENT_HEADER, USER_AGENT);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if have > 0 {
        req = req.header(RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().await?;
    match resp.status() {
        StatusCode::OK => have = 0,
        StatusCode::PARTIAL_CONTENT => {}
        s => return Err(Error::NotAFile(format!("http {}", s.as_u16()))),
    }
    let content_type = header_str(&resp, CONTENT_TYPE);
    if !is_file(&content_type, &header_str(&resp, CONTENT_DISPOSITION)) {
        return Err(Error::NotAFile(format!(
            "not a video: {}",
            content_type.to_ascii_lowercase()
        )));
    }

    let length = resp.content_length().filter(|&l| l > 0);
    let total = length.map(|l| have + l);
    check_free_space(out, length.unwrap_or(0), space)?;

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(have > 0)
        .truncate(have == 0)
        .open(out)
        .await?;
    let mut buffer: Vec<u8> = Vec::with_capacity(WRITE_BUFFER);
    let mut written = have;
    let mut last_report: Option<Instant> = None;
    let mut stream = resp.bytes_stream();
    let mut failure = None;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        buffer.extend_from_slice(&chunk);
        if buffer.len() >= WRITE_BUFFER {
            file.write_all(&buffer).await?;
            written += buffer.len() as u64;
            buffer.clear();
        }
        if last_report.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL) {
            last_report = Some(Instant::now());
            progress(written + buffer.len() as u64, total);
        }
    }
    if !buffer.is_empty() {
        file.write_all(&buffer).await?;
        written += buffer.len() as u64;
    }
    file.flush().await?;
    drop(file);

    if let Some(total) = total {
        if written < total {
            return Err(Error::Other(format!(
                "connection closed at {} of {} MB",
                written / MB,
                total / MB
            )));
        }
    }
    if let Some(e) = failure {
        return Err(e.into());
    }
    progress(written, total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    use parking_lot::Mutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const PLENTY: fn(&Path) -> io::Result<u64> = |_| Ok(u64::MAX);

    fn body(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    /// Serves one canned response per connection, in order, recording each request head.
    fn raw_server(responses: Vec<Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let seen = heads.clone();
        std::thread::spawn(move || {
            for resp in responses {
                let (mut sock, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if sock.read(&mut byte).unwrap() == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                seen.lock()
                    .push(String::from_utf8_lossy(&head).to_lowercase());
                sock.write_all(&resp).unwrap();
                sock.flush().unwrap();
                let _ = sock.shutdown(std::net::Shutdown::Both);
            }
        });
        (format!("http://{addr}/movie.mkv"), heads)
    }

    #[tokio::test]
    async fn resumes_after_a_mid_stream_drop() {
        let full = body(1000);
        let mut first =
            b"HTTP/1.1 200 OK\r\nContent-Type: video/x-matroska\r\nContent-Length: 1000\r\n\r\n"
                .to_vec();
        first.extend_from_slice(&full[..400]);
        let mut second = b"HTTP/1.1 206 Partial Content\r\nContent-Type: video/x-matroska\r\nContent-Range: bytes 400-999/1000\r\nContent-Length: 600\r\n\r\n".to_vec();
        second.extend_from_slice(&full[400..]);
        let (url, heads) = raw_server(vec![first, second]);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("media.mkv");

        let err = download_with(&url, &[], &out, |_, _| {}, CancellationToken::new(), PLENTY)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "connection closed at 0 of 0 MB");
        assert_eq!(std::fs::read(&out).unwrap(), full[..400]);

        let reports = Mutex::new(Vec::new());
        download_with(
            &url,
            &[("Referer".into(), "https://example.com/".into())],
            &out,
            |b, t| reports.lock().push((b, t)),
            CancellationToken::new(),
            PLENTY,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), full);
        assert_eq!(reports.lock().last(), Some(&(1000, Some(1000))));

        let heads = heads.lock();
        assert!(!heads[0].contains("range:"));
        assert!(heads[1].contains("range: bytes=400-\r\n"));
        assert!(heads[1].contains("referer: https://example.com/\r\n"));
        assert!(heads[1].contains("user-agent: mozilla/5.0"));
    }

    #[tokio::test]
    async fn full_response_restarts_a_partial_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/f"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/octet-stream")
                    .set_body_bytes(body(300)),
            )
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("media.mp4");
        std::fs::write(&out, b"stale bytes").unwrap();
        let url = format!("{}/f", server.uri());
        download_with(&url, &[], &out, |_, _| {}, CancellationToken::new(), PLENTY)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), body(300));
    }

    #[tokio::test]
    async fn html_is_not_a_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<html></html>", "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let url = format!("{}/page", server.uri());
        let err = download_with(
            &url,
            &[],
            &dir.path().join("x.mp4"),
            |_, _| {},
            CancellationToken::new(),
            PLENTY,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, Error::NotAFile(ref m) if m == "not a video: text/html; charset=utf-8")
        );
    }

    #[tokio::test]
    async fn http_error_is_not_a_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let err = download_with(
            &server.uri(),
            &[],
            &dir.path().join("x.mp4"),
            |_, _| {},
            CancellationToken::new(),
            PLENTY,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::NotAFile(ref m) if m == "http 403"));
    }

    #[tokio::test]
    async fn not_enough_disk_space() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/plain")
                    .insert_header("Content-Disposition", "attachment; filename=\"a.mkv\"")
                    .set_body_bytes(body(500)),
            )
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("x.mkv");
        let asked = Mutex::new(None);
        let err = download_with(
            &server.uri(),
            &[],
            &out,
            |_, _| {},
            CancellationToken::new(),
            |p: &Path| {
                *asked.lock() = Some(p.to_path_buf());
                Ok(1_500_000_000)
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "not enough disk space: need 2 GB, have 1 GB"
        );
        assert_eq!(asked.lock().as_deref(), Some(dir.path()));
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn cancel_stops_the_transfer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "video/mp4")
                    .set_body_bytes(body(10))
                    .set_delay(Duration::from_secs(30)),
            )
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.cancel();
        });
        let err = download(&server.uri(), &[], &dir.path().join("x"), |_, _| {}, cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }

    #[test]
    fn detects_files() {
        assert!(is_file("video/mp4", ""));
        assert!(is_file("Application/Octet-Stream", ""));
        assert!(is_file("application/x-matroska", ""));
        assert!(is_file("application/force-download", ""));
        assert!(is_file("text/plain", "attachment; FILENAME=a.mkv"));
        assert!(!is_file("text/html", "inline"));
        assert!(!is_file("", ""));
    }

    #[test]
    fn download_future_is_send() {
        fn assert_send<T: Send>(_: T) {}
        assert_send(download(
            "http://localhost/",
            &[],
            Path::new("x"),
            |_, _| {},
            CancellationToken::new(),
        ));
    }
}
