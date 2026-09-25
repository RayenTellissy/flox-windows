//! A blocking `Read + Seek` over an entry's parts, fed by TDLib downloads with
//! Android's sliding-window rules. Called from mpv's stream thread.
//!
//! The parts are stitched into one virtual stream through prefix sums. Each
//! read pulls one chunk with `readFilePart`, never crossing a part boundary,
//! once TDLib reports the range as downloaded. Downloads run in a sliding
//! window that is moved when reads pass its middle, go backwards, or the
//! download stops, so TDLib never races to the end of a multi-gigabyte part
//! while the player still needs its beginning.
//!
//! Reads and [`TdStream::close`] block on the runtime handle, so they must be
//! called from a thread that is not driving the runtime (mpv's stream thread,
//! or `spawn_blocking`).

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use flox_core::error::{Error, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{broadcast, Notify};
use tokio_util::sync::CancellationToken;

use crate::library::Part;
use crate::transport::{json_i64, TdTransport};

/// Bytes per `readFilePart`.
pub const CHUNK: usize = 512 * 1024;
/// `downloadFile` window length.
pub const WINDOW: i64 = 256 << 20;
/// Start prefetching the next part when this much of the current part is left.
pub const PREFETCH_WHEN_LEFT: i64 = 512 << 20;
/// How much of the next part to prefetch.
pub const PREFETCH_BYTES: i64 = 64 << 20;
/// How long a read waits for TDLib to download the bytes it needs.
pub const COVERAGE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a single `readFilePart` may take.
pub const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// `downloadFile` priority of the read window.
const WINDOW_PRIORITY: i32 = 32;
/// `downloadFile` priority of the next-part prefetch.
const PREFETCH_PRIORITY: i32 = 16;

/// The tunables, scaled down by tests.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    pub chunk: usize,
    pub window: i64,
    pub prefetch_when_left: i64,
    pub prefetch_bytes: i64,
    pub coverage_timeout: Duration,
    pub read_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            chunk: CHUNK,
            window: WINDOW,
            prefetch_when_left: PREFETCH_WHEN_LEFT,
            prefetch_bytes: PREFETCH_BYTES,
            coverage_timeout: COVERAGE_TIMEOUT,
            read_timeout: READ_TIMEOUT,
        }
    }
}

/// A file's `local` download state as TDLib last reported it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Local {
    download_offset: i64,
    downloaded_prefix_size: i64,
    is_downloading_active: bool,
    is_downloading_completed: bool,
}

impl Local {
    /// Reads `file.local`, or `None` when `file` has no id or no local state.
    fn parse(file: &Value) -> Option<(i32, Local)> {
        let id = file
            .get("id")
            .and_then(json_i64)
            .and_then(|i| i32::try_from(i).ok())?;
        let l = file.get("local")?;
        let int = |k: &str| l.get(k).and_then(json_i64).unwrap_or(0);
        let flag = |k: &str| l.get(k).and_then(Value::as_bool).unwrap_or(false);
        Some((
            id,
            Local {
                download_offset: int("download_offset"),
                downloaded_prefix_size: int("downloaded_prefix_size"),
                is_downloading_active: flag("is_downloading_active"),
                is_downloading_completed: flag("is_downloading_completed"),
            },
        ))
    }

    /// Whether `[offset, offset + count)` is on disk.
    fn covers(&self, offset: i64, count: i64) -> bool {
        self.is_downloading_completed
            || (self.download_offset <= offset
                && offset + count <= self.download_offset + self.downloaded_prefix_size)
    }
}

/// Per-file state, with a counter bumped on every `updateFile` so a slower
/// `downloadFile` response never overwrites a newer update.
#[derive(Clone, Copy, Debug, Default)]
struct FileState {
    local: Option<Local>,
    updates: u64,
}

/// File states shared with the update listener.
#[derive(Default)]
struct Files {
    map: Mutex<HashMap<i32, FileState>>,
    changed: Notify,
}

impl Files {
    fn get(&self, id: i32) -> FileState {
        self.map.lock().get(&id).copied().unwrap_or_default()
    }

    fn covered(&self, id: i32, offset: i64, count: i64) -> bool {
        self.get(id).local.is_some_and(|l| l.covers(offset, count))
    }

    /// Applies an `updateFile`.
    fn update(&self, file: &Value) {
        let Some((id, local)) = Local::parse(file) else {
            return;
        };
        {
            let mut map = self.map.lock();
            let Some(st) = map.get_mut(&id) else {
                return;
            };
            st.local = Some(local);
            st.updates += 1;
        }
        self.changed.notify_waiters();
    }

    /// Applies a `file` returned by a request sent when the update counter was
    /// `seen`, unless an update arrived in between.
    fn response(&self, file: &Value, seen: u64) {
        let Some((id, local)) = Local::parse(file) else {
            return;
        };
        {
            let mut map = self.map.lock();
            let Some(st) = map.get_mut(&id) else {
                return;
            };
            if st.updates != seen {
                return;
            }
            st.local = Some(local);
        }
        self.changed.notify_waiters();
    }
}

/// The stream.
pub struct TdStream {
    t: Arc<dyn TdTransport>,
    rt: tokio::runtime::Handle,
    cfg: Config,
    parts: Vec<Part>,
    /// Start offset of each part in the virtual stream.
    starts: Vec<u64>,
    total: u64,
    pos: u64,
    buf: Vec<u8>,
    buf_start: u64,
    active_part: Option<usize>,
    prefetched: bool,
    /// Where the current download window of each file starts.
    window_start: HashMap<i32, i64>,
    files: Arc<Files>,
    cancel: CancellationToken,
    /// Stops the update listener.
    shutdown: CancellationToken,
    closed: bool,
}

impl std::fmt::Debug for TdStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TdStream")
            .field("parts", &self.parts)
            .field("total", &self.total)
            .field("pos", &self.pos)
            .field("active_part", &self.active_part)
            .finish_non_exhaustive()
    }
}

impl TdStream {
    /// Prepares the stream; nothing downloads until the first read.
    pub fn open(
        t: Arc<dyn TdTransport>,
        rt: tokio::runtime::Handle,
        parts: Vec<Part>,
    ) -> Result<TdStream> {
        Self::open_with(t, rt, parts, Config::default())
    }

    /// [`TdStream::open`] with scaled-down tunables.
    #[cfg(test)]
    pub(crate) fn open_with_config(
        t: Arc<dyn TdTransport>,
        rt: tokio::runtime::Handle,
        parts: Vec<Part>,
        cfg: Config,
    ) -> Result<TdStream> {
        Self::open_with(t, rt, parts, cfg)
    }

    fn open_with(
        t: Arc<dyn TdTransport>,
        rt: tokio::runtime::Handle,
        parts: Vec<Part>,
        cfg: Config,
    ) -> Result<TdStream> {
        if parts.is_empty() {
            return Err(Error::Other("a library entry has no parts".into()));
        }
        let mut starts = Vec::with_capacity(parts.len());
        let mut total = 0u64;
        for p in &parts {
            starts.push(total);
            total = total
                .checked_add(p.size)
                .ok_or_else(|| Error::Other("part sizes overflow".into()))?;
        }
        if i64::try_from(total).is_err() {
            return Err(Error::Other("entry is too large to stream".into()));
        }

        let files = Arc::new(Files::default());
        {
            let mut map = files.map.lock();
            for p in &parts {
                map.insert(p.file_id, FileState::default());
            }
        }

        let shutdown = CancellationToken::new();
        let rx = t.updates();
        rt.spawn(listen(rx, Arc::clone(&files), shutdown.clone()));

        Ok(TdStream {
            t,
            rt,
            cfg,
            parts,
            starts,
            total,
            pos: 0,
            buf: Vec::new(),
            buf_start: 0,
            active_part: None,
            prefetched: false,
            window_start: HashMap::new(),
            files,
            cancel: CancellationToken::new(),
            shutdown,
            closed: false,
        })
    }

    /// Total size of all parts.
    pub fn size(&self) -> u64 {
        self.total
    }

    /// Cancels blocked reads. Once cancelled, every read fails with
    /// [`io::ErrorKind::Interrupted`].
    pub fn cancel_handle(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Cancels downloads and deletes every part's local file.
    pub fn close(mut self) -> Result<()> {
        self.closed = true;
        self.shutdown.cancel();
        let t = Arc::clone(&self.t);
        let ids: Vec<i32> = self.parts.iter().map(|p| p.file_id).collect();
        self.rt.block_on(release(t, ids))
    }

    /// Index of the part holding `pos` (which must be below the total size).
    fn part_at(&self, pos: u64) -> usize {
        self.starts.partition_point(|s| *s <= pos).saturating_sub(1)
    }

    /// Pulls the chunk at the current position, never crossing a part boundary.
    fn fill(&mut self) -> io::Result<()> {
        if self.cancel.is_cancelled() {
            return Err(interrupted());
        }
        let cancel = self.cancel.clone();
        let rt = self.rt.clone();
        rt.block_on(async {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(interrupted()),
                r = self.fill_async() => r,
            }
        })
    }

    async fn fill_async(&mut self) -> io::Result<()> {
        let idx = self.part_at(self.pos);
        let part = self.parts[idx].clone();
        let local = self.pos - self.starts[idx];
        let left = part.size - local;
        let count = left.min(u64::try_from(self.cfg.chunk).unwrap_or(u64::MAX));
        let (local, left, count) = (to_i64(local)?, to_i64(left)?, to_i64(count)?);

        if self.active_part != Some(idx) {
            if let Some(old) = self.active_part {
                let id = self.parts[old].file_id;
                self.window_start.remove(&id);
                cancel_download(self.t.as_ref(), id).await;
            }
            self.active_part = Some(idx);
            self.prefetched = false;
        }
        if !self.prefetched && idx + 1 < self.parts.len() && left <= self.cfg.prefetch_when_left {
            let next = self.parts[idx + 1].file_id;
            self.prefetch(next).await;
            self.prefetched = true;
        }

        self.ensure_downloaded(part.file_id, local, count).await?;
        let bytes = self.read_file_part(part.file_id, local, count).await?;
        if bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("empty read at {}", self.pos),
            ));
        }
        self.buf = bytes;
        self.buf_start = self.pos;
        Ok(())
    }

    /// Warms the start of the next part at low priority so a part switch does
    /// not stall the player.
    async fn prefetch(&self, id: i32) {
        let count = self.cfg.prefetch_bytes;
        if self.files.covered(id, 0, count) {
            return;
        }
        let seen = self.files.get(id).updates;
        match self
            .t
            .request(download_file(id, PREFETCH_PRIORITY, 0, count))
            .await
        {
            Ok(f) => self.files.response(&f, seen),
            Err(e) => tracing::debug!("prefetch of file {id} failed: {e}"),
        }
    }

    /// Waits until `[offset, offset + count)` of `id` is on disk, starting or
    /// moving the download window as needed.
    async fn ensure_downloaded(&mut self, id: i32, offset: i64, count: i64) -> io::Result<()> {
        if self.files.covered(id, offset, count) {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + self.cfg.coverage_timeout;
        let st = self.files.get(id);
        let active = st.local.is_some_and(|l| l.is_downloading_active);
        let stale = match self.window_start.get(&id) {
            None => true,
            Some(&start) => {
                offset < start || offset + count > start + self.cfg.window / 2 || !active
            }
        };
        if stale {
            self.window_start.insert(id, offset);
            let req = download_file(id, WINDOW_PRIORITY, offset, self.cfg.window);
            match tokio::time::timeout_at(deadline, self.t.request(req)).await {
                Ok(Ok(f)) => self.files.response(&f, st.updates),
                Ok(Err(e)) => tracing::debug!("downloadFile {id} at {offset} failed: {e}"),
                Err(_) => return Err(self.timed_out(id, offset)),
            }
        }
        loop {
            let changed = self.files.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.files.covered(id, offset, count) {
                return Ok(());
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return Err(self.timed_out(id, offset));
            }
        }
    }

    fn timed_out(&self, id: i32, offset: i64) -> io::Error {
        let l = self.files.get(id).local.unwrap_or_default();
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "download timeout for file {id} at {offset} (offset={} prefix={} active={})",
                l.download_offset, l.downloaded_prefix_size, l.is_downloading_active
            ),
        )
    }

    async fn read_file_part(&self, id: i32, offset: i64, count: i64) -> io::Result<Vec<u8>> {
        let req = json!({"@type": "readFilePart", "file_id": id, "offset": offset, "count": count});
        let r = tokio::time::timeout(self.cfg.read_timeout, self.t.request(req))
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("readFilePart of file {id} at {offset} timed out"),
                )
            })?
            .map_err(io::Error::other)?;
        let data = r.get("data").and_then(Value::as_str).unwrap_or_default();
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        bytes.truncate(usize::try_from(count).unwrap_or(usize::MAX));
        Ok(bytes)
    }
}

impl Drop for TdStream {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if self.closed {
            return;
        }
        // Dropped without close: release the files in the background, since
        // blocking here could run inside the runtime.
        let ids = self.parts.iter().map(|p| p.file_id).collect();
        let t = Arc::clone(&self.t);
        self.rt.spawn(async move {
            if let Err(e) = release(t, ids).await {
                tracing::debug!("releasing dropped stream files failed: {e}");
            }
        });
    }
}

impl io::Read for TdStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.total {
            return Ok(0);
        }
        let end = self.buf_start + self.buf.len() as u64;
        if self.pos < self.buf_start || self.pos >= end {
            self.fill()?;
        }
        let at = usize::try_from(self.pos - self.buf_start)
            .map_err(|_| io::Error::other("buffer offset overflow"))?;
        let avail = &self.buf[at..];
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl io::Seek for TdStream {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        let target = match pos {
            io::SeekFrom::Start(p) => Some(p),
            io::SeekFrom::End(d) => self.total.checked_add_signed(d),
            io::SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        let Some(target) = target else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the stream",
            ));
        };
        self.pos = target;
        Ok(target)
    }
}

/// Mirrors `updateFile` into `files` until `shutdown`.
async fn listen(
    mut rx: broadcast::Receiver<Arc<Value>>,
    files: Arc<Files>,
    shutdown: CancellationToken,
) {
    loop {
        let u = tokio::select! {
            _ = shutdown.cancelled() => return,
            u = rx.recv() => u,
        };
        match u {
            Ok(u) => {
                if u.get("@type").and_then(Value::as_str) == Some("updateFile") {
                    if let Some(f) = u.get("file") {
                        files.update(f);
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!("stream update listener skipped {n} updates");
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn download_file(id: i32, priority: i32, offset: i64, limit: i64) -> Value {
    json!({
        "@type": "downloadFile",
        "file_id": id,
        "priority": priority,
        "offset": offset,
        "limit": limit,
        "synchronous": false,
    })
}

async fn cancel_download(t: &dyn TdTransport, id: i32) {
    let req = json!({"@type": "cancelDownloadFile", "file_id": id, "only_if_pending": false});
    if let Err(e) = t.request(req).await {
        tracing::debug!("cancelDownloadFile {id} failed: {e}");
    }
}

/// Cancels the download of and deletes every file in `ids`; returns the first error.
async fn release(t: Arc<dyn TdTransport>, ids: Vec<i32>) -> Result<()> {
    let mut first = None;
    for id in ids {
        cancel_download(t.as_ref(), id).await;
        if let Err(e) = t
            .request(json!({"@type": "deleteFile", "file_id": id}))
            .await
        {
            tracing::debug!("deleteFile {id} failed: {e}");
            first.get_or_insert(e);
        }
    }
    first.map_or(Ok(()), Err)
}

fn to_i64(v: u64) -> io::Result<i64> {
    i64::try_from(v).map_err(|_| io::Error::other("offset overflow"))
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "stream read cancelled")
}

#[cfg(test)]
mod tests;
