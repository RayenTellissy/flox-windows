//! Running a tool with merged, line-split output and kill-on-cancel
//! (`CREATE_NO_WINDOW` on Windows).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;

use flox_core::error::{Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// How much of the tail of the output a failure message carries (the Mac keeps 400 characters).
const ERROR_TAIL_CHARS: usize = 400;

/// Lines kept for the failure message.
const TAIL_LINES: usize = 32;

/// `CREATE_NO_WINDOW`, so a console tool never flashes a window from the GUI app.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Splits a byte stream into lines on `\n` and `\r`, keeping the unfinished tail.
#[derive(Default)]
struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    /// Feeds bytes and hands every finished, non-empty line to `emit`.
    fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(String)) {
        for &b in bytes {
            if b == b'\n' || b == b'\r' {
                self.flush(emit)
            } else {
                self.pending.push(b)
            }
        }
    }

    /// Emits whatever is left as a final line.
    fn flush(&mut self, emit: &mut impl FnMut(String)) {
        if self.pending.is_empty() {
            return;
        }
        let line = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        emit(line)
    }
}

/// One output pipe being drained.
struct Pipe<R> {
    reader: Option<R>,
    lines: LineSplitter,
}

impl<R: AsyncRead + Unpin> Pipe<R> {
    fn new(reader: Option<R>) -> Self {
        Pipe {
            reader,
            lines: LineSplitter::default(),
        }
    }

    /// Reads one chunk. Returns `Ok(None)` at end of stream, and pends forever once closed.
    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>> {
        match self.reader.as_mut() {
            Some(r) => match r.read(buf).await? {
                0 => {
                    self.reader = None;
                    Ok(None)
                }
                n => Ok(Some(n)),
            },
            None => std::future::pending().await,
        }
    }

    fn open(&self) -> bool {
        self.reader.is_some()
    }
}

/// The last lines of output, for the error message.
struct Tail {
    lines: VecDeque<String>,
}

impl Tail {
    fn push(&mut self, line: &str) {
        if self.lines.len() == TAIL_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line.to_string())
    }

    /// The last [`ERROR_TAIL_CHARS`] characters of the joined lines.
    fn text(&self) -> String {
        let joined = self.lines.iter().cloned().collect::<Vec<_>>().join("\n");
        let count = joined.chars().count();
        joined
            .chars()
            .skip(count.saturating_sub(ERROR_TAIL_CHARS))
            .collect()
    }
}

/// Runs `cmd args`, calling `on_line` for each stdout/stderr line (`\r` also splits).
/// A non-zero exit is an error carrying the last lines.
pub async fn run(
    cmd: &Path,
    args: &[OsString],
    mut on_line: impl FnMut(&str) + Send,
    cancel: CancellationToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let name = cmd
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cmd.display().to_string());

    let mut command = Command::new(cmd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command
        .spawn()
        .map_err(|e| Error::Tool(format!("{name}: {e}")))?;
    let mut out = Pipe::new(child.stdout.take());
    let mut err = Pipe::new(child.stderr.take());
    let mut tail = Tail {
        lines: VecDeque::with_capacity(TAIL_LINES),
    };
    let mut emit = |line: String| {
        tail.push(&line);
        on_line(&line)
    };
    let mut out_buf = vec![0u8; 16 * 1024];
    let mut err_buf = vec![0u8; 16 * 1024];

    // Drain both pipes until they close, then wait for the exit; cancel kills at any point.
    while out.open() || err.open() {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(Error::Cancelled);
            }
            r = out.read(&mut out_buf) => match r? {
                Some(n) => out.lines.feed(&out_buf[..n], &mut emit),
                None => out.lines.flush(&mut emit),
            },
            r = err.read(&mut err_buf) => match r? {
                Some(n) => err.lines.feed(&err_buf[..n], &mut emit),
                None => err.lines.flush(&mut emit),
            },
        }
    }

    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(Error::Cancelled);
        }
        s = child.wait() => s?,
    };
    if status.success() {
        return Ok(());
    }
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let text = tail.text();
    Err(Error::Tool(match status.code() {
        Some(code) => format!("{name} exit {code}: {text}"),
        None => format!("{name} was killed: {text}"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&[u8]]) -> Vec<String> {
        let mut lines = Vec::new();
        let mut s = LineSplitter::default();
        let mut emit = |l: String| lines.push(l);
        for c in chunks {
            s.feed(c, &mut emit);
        }
        s.flush(&mut emit);
        lines
    }

    #[test]
    fn splits_on_cr_and_lf() {
        assert_eq!(
            split(&[b"frame=1\rframe=2\r", b"fra", b"me=3\r\nwarn\n", b"last"]),
            vec!["frame=1", "frame=2", "frame=3", "warn", "last"]
        );
    }

    #[test]
    fn tail_keeps_the_end() {
        let mut t = Tail {
            lines: VecDeque::new(),
        };
        for i in 0..100 {
            t.push(&format!("line {i:03} {}", "x".repeat(20)));
        }
        let text = t.text();
        assert!(text.chars().count() <= ERROR_TAIL_CHARS);
        assert!(text.ends_with(&format!("line 099 {}", "x".repeat(20))));
    }
}
