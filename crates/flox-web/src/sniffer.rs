//! [`Sniffer`](flox_core::sniff::Sniffer) over a hidden, muted WebView2 host.
//!
//! One host is started on the first sniff and reused; sniffs run one at a time. Each sniff
//! registers the document-start script for its mode, loads the page and feeds the page
//! messages into a [`SniffState`]:
//!
//! - Playback resolves on the first `FLOX_MANIFEST` (45 s timeout).
//! - Rip resolves 0.5 s after the first `FLOX_PLAYLIST` (a `stream.playlist` or the best
//!   `stream.qualities` file) so a caption list posted right after it is still merged
//!   (60 s timeout).
//!
//! Captions from `FLOX_STREAM` are merged in either mode and headers pass through
//! [`filter_headers`]. The view is parked on `about:blank` after every sniff.

use std::time::{Duration, Instant};

use flox_core::sniff::{Caption, SniffMode, SniffResult, StreamKind};

use crate::bridge::{filter_headers, BridgeMessage};

/// Playback gives up after this long without a manifest.
pub const PLAYBACK_TIMEOUT: Duration = Duration::from_secs(45);
/// Rip gives up after this long without a playlist.
pub const RIP_TIMEOUT: Duration = Duration::from_secs(60);
/// Rip waits this long after the playlist before resolving.
pub const RIP_SETTLE: Duration = Duration::from_millis(500);

/// The overall timeout for a mode.
pub fn timeout(mode: SniffMode) -> Duration {
    match mode {
        SniffMode::Playback => PLAYBACK_TIMEOUT,
        SniffMode::Rip => RIP_TIMEOUT,
    }
}

/// The stream kind of a `FLOX_PLAYLIST`: the page's `stream.type`, else guessed from the URL.
pub fn playlist_kind(kind: &str, url: &str) -> StreamKind {
    match kind.trim().to_ascii_lowercase().as_str() {
        "hls" => StreamKind::Hls,
        "dash" => StreamKind::Dash,
        "file" | "mp4" => StreamKind::File,
        _ => {
            let path = url
                .split(['?', '#'])
                .next()
                .unwrap_or(url)
                .to_ascii_lowercase();
            if path.ends_with(".mpd") {
                StreamKind::Dash
            } else if path.ends_with(".mp4") || path.ends_with(".mkv") {
                StreamKind::File
            } else {
                StreamKind::Hls
            }
        }
    }
}

/// What [`SniffState::poll`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    /// Keep waiting for messages, at most until this instant.
    Pending(Instant),
    Ready(SniffResult),
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Found {
    url: String,
    kind: StreamKind,
    headers: Vec<(String, String)>,
}

/// The pure part of a sniff: page messages in, a result or a timeout out.
#[derive(Clone, Debug)]
pub struct SniffState {
    mode: SniffMode,
    deadline: Instant,
    settle_at: Option<Instant>,
    found: Option<Found>,
    captions: Vec<Caption>,
}

impl SniffState {
    pub fn new(mode: SniffMode, started: Instant) -> Self {
        Self {
            mode,
            deadline: started + timeout(mode),
            settle_at: None,
            found: None,
            captions: Vec::new(),
        }
    }

    /// Takes one page message. The first stream for the mode wins; later ones are ignored.
    pub fn on_message(&mut self, msg: BridgeMessage, now: Instant) {
        match (self.mode, msg) {
            (SniffMode::Playback, BridgeMessage::Manifest { url, kind, headers })
                if self.found.is_none() =>
            {
                self.found = Some(Found { url, kind, headers });
                self.settle_at = Some(now);
            }
            (
                SniffMode::Rip,
                BridgeMessage::Playlist {
                    url, kind, headers, ..
                },
            ) if self.found.is_none() => {
                self.found = Some(Found {
                    kind: playlist_kind(&kind, &url),
                    url,
                    headers,
                });
                self.settle_at = Some(now + RIP_SETTLE);
            }
            (_, BridgeMessage::Stream { captions }) => {
                for c in captions {
                    if !self.captions.iter().any(|have| have.url == c.url) {
                        self.captions.push(c);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn poll(&self, now: Instant) -> Poll {
        // once a stream is found only the settle time counts, even past the deadline
        if let (Some(at), Some(found)) = (self.settle_at, &self.found) {
            if now < at {
                return Poll::Pending(at);
            }
            return Poll::Ready(SniffResult {
                url: found.url.clone(),
                kind: found.kind,
                headers: filter_headers(&found.headers),
                captions: self.captions.clone(),
            });
        }
        if now >= self.deadline {
            return Poll::TimedOut;
        }
        Poll::Pending(self.deadline)
    }
}

#[cfg(windows)]
pub use imp::WebView2Sniffer;

#[cfg(windows)]
mod imp {
    use std::time::Instant;

    use async_trait::async_trait;
    use flox_core::error::{Error, Result};
    use flox_core::sniff::{SniffMode, SniffResult, Sniffer};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;
    use tracing::debug;

    use super::{timeout, Poll, SniffState};
    use crate::assets::{document_start_script, ScriptOptions};
    use crate::host::{default_user_data, HostConfig, HostEvent, Surface, WebHost};

    /// The WebView2-backed sniffer.
    pub struct WebView2Sniffer {
        opts: ScriptOptions,
        session: Mutex<Option<Session>>,
    }

    struct Session {
        host: WebHost,
        events: UnboundedReceiver<HostEvent>,
        dead: bool,
    }

    impl WebView2Sniffer {
        /// Cheap: the host starts on the first sniff.
        pub fn new(opts: ScriptOptions) -> Self {
            Self {
                opts,
                session: Mutex::new(None),
            }
        }

        async fn open_session() -> Result<Session> {
            let (tx, events) = unbounded_channel();
            let config = HostConfig {
                surface: Surface::Hidden,
                muted: true,
                user_data: default_user_data("sniffer"),
                after_load_script: None,
            };
            let host = WebHost::start(
                config,
                Box::new(move |event| {
                    let _ = tx.send(event);
                }),
            )
            .await?;
            Ok(Session {
                host,
                events,
                dead: false,
            })
        }
    }

    #[async_trait]
    impl Sniffer for WebView2Sniffer {
        async fn sniff(
            &self,
            page_url: &str,
            mode: SniffMode,
            cancel: CancellationToken,
        ) -> Result<SniffResult> {
            let mut guard = tokio::select! {
                guard = self.session.lock() => guard,
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            };
            if guard.as_ref().is_none_or(|s| s.dead) {
                *guard = None;
                let session = tokio::select! {
                    session = Self::open_session() => session?,
                    _ = cancel.cancelled() => return Err(Error::Cancelled),
                };
                *guard = Some(session);
            }
            let Some(session) = guard.as_mut() else {
                return Err(Error::Unavailable("WebView2 host missing".to_owned()));
            };
            // anything still queued belongs to the previous page
            while session.events.try_recv().is_ok() {}
            let script = document_start_script(mode, &self.opts);
            let result = run(session, page_url, mode, script, &cancel).await;
            if session.host.navigate("about:blank").is_err() {
                session.dead = true;
            }
            if session.dead {
                *guard = None;
            }
            result
        }
    }

    async fn run(
        session: &mut Session,
        page_url: &str,
        mode: SniffMode,
        script: String,
        cancel: &CancellationToken,
    ) -> Result<SniffResult> {
        let sent = session
            .host
            .set_document_script(script)
            .and_then(|()| session.host.navigate(page_url));
        if let Err(e) = sent {
            session.dead = true;
            return Err(e);
        }
        let mut state = SniffState::new(mode, Instant::now());
        loop {
            let wake = match state.poll(Instant::now()) {
                Poll::Ready(result) => return Ok(result),
                Poll::TimedOut => {
                    return Err(Error::Timeout(format!(
                        "no stream from {page_url} within {} s",
                        timeout(mode).as_secs()
                    )))
                }
                Poll::Pending(at) => at,
            };
            tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Cancelled),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(wake)) => {}
                event = session.events.recv() => match event {
                    Some(HostEvent::Message(msg)) => state.on_message(msg, Instant::now()),
                    Some(HostEvent::Loaded { url, success, .. }) => {
                        // a cancelled ad redirect also ends with success = false; keep waiting
                        debug!("sniffer page loaded: {url} ({success})");
                    }
                    Some(HostEvent::ProcessFailed { browser }) => {
                        if browser {
                            session.dead = true;
                            return Err(Error::Unavailable(
                                "the WebView2 browser process exited".to_owned(),
                            ));
                        }
                    }
                    None => {
                        session.dead = true;
                        return Err(Error::Unavailable("the WebView2 host closed".to_owned()));
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn manifest(url: &str) -> BridgeMessage {
        BridgeMessage::Manifest {
            url: url.to_owned(),
            kind: StreamKind::Hls,
            headers: pairs(&[("Referer", "https://vidlink.pro/"), ("Cookie", "a=b")]),
        }
    }

    fn playlist(url: &str, kind: &str) -> BridgeMessage {
        BridgeMessage::Playlist {
            url: url.to_owned(),
            kind: kind.to_owned(),
            headers: pairs(&[("origin", "https://videostr.net"), ("host", "x")]),
            meta: json!({}),
        }
    }

    fn caption(url: &str, lang: &str) -> Caption {
        Caption {
            url: url.to_owned(),
            language: lang.to_owned(),
            kind: "vtt".to_owned(),
        }
    }

    #[test]
    fn timeouts() {
        assert_eq!(timeout(SniffMode::Playback), Duration::from_secs(45));
        assert_eq!(timeout(SniffMode::Rip), Duration::from_secs(60));
        let t0 = Instant::now();
        let s = SniffState::new(SniffMode::Playback, t0);
        assert_eq!(s.poll(t0), Poll::Pending(t0 + PLAYBACK_TIMEOUT));
        assert_eq!(
            s.poll(t0 + PLAYBACK_TIMEOUT - Duration::from_millis(1)),
            Poll::Pending(t0 + PLAYBACK_TIMEOUT)
        );
        assert_eq!(s.poll(t0 + PLAYBACK_TIMEOUT), Poll::TimedOut);
        let r = SniffState::new(SniffMode::Rip, t0);
        assert_eq!(
            r.poll(t0 + PLAYBACK_TIMEOUT),
            Poll::Pending(t0 + RIP_TIMEOUT)
        );
        assert_eq!(r.poll(t0 + RIP_TIMEOUT), Poll::TimedOut);
    }

    #[test]
    fn playback_resolves_on_first_manifest_with_captions() {
        let t0 = Instant::now();
        let mut s = SniffState::new(SniffMode::Playback, t0);
        let t1 = t0 + Duration::from_secs(3);
        s.on_message(
            BridgeMessage::Stream {
                captions: vec![
                    caption("https://s/en.vtt", "en"),
                    caption("https://s/es.vtt", "es"),
                ],
            },
            t1,
        );
        s.on_message(
            BridgeMessage::Stream {
                captions: vec![
                    caption("https://s/en.vtt", "en"),
                    caption("https://s/fr.vtt", "fr"),
                ],
            },
            t1,
        );
        // a rip-only message does not resolve playback
        s.on_message(playlist("https://p/master.m3u8", "hls"), t1);
        assert_eq!(s.poll(t1), Poll::Pending(t0 + PLAYBACK_TIMEOUT));
        s.on_message(manifest("https://cdn/a.m3u8"), t1);
        s.on_message(manifest("https://cdn/b.m3u8"), t1);
        let Poll::Ready(r) = s.poll(t1) else {
            panic!("not ready")
        };
        assert_eq!(r.url, "https://cdn/a.m3u8");
        assert_eq!(r.kind, StreamKind::Hls);
        assert_eq!(r.headers, pairs(&[("Referer", "https://vidlink.pro/")]));
        let langs: Vec<&str> = r.captions.iter().map(|c| c.language.as_str()).collect();
        assert_eq!(langs, ["en", "es", "fr"]);
    }

    #[test]
    fn rip_settles_half_a_second_after_the_playlist() {
        let t0 = Instant::now();
        let mut s = SniffState::new(SniffMode::Rip, t0);
        let t1 = t0 + Duration::from_secs(10);
        // playback manifests do not resolve a rip
        s.on_message(manifest("https://cdn/a.m3u8"), t1);
        assert_eq!(s.poll(t1), Poll::Pending(t0 + RIP_TIMEOUT));
        s.on_message(playlist("https://p/master.m3u8", "hls"), t1);
        assert_eq!(s.poll(t1), Poll::Pending(t1 + RIP_SETTLE));
        // captions posted during the settle window are merged; a second playlist is ignored
        let t2 = t1 + Duration::from_millis(200);
        s.on_message(
            BridgeMessage::Stream {
                captions: vec![caption("https://s/en.vtt", "en")],
            },
            t2,
        );
        s.on_message(playlist("https://other/x.mp4", "file"), t2);
        assert_eq!(s.poll(t2), Poll::Pending(t1 + RIP_SETTLE));
        let Poll::Ready(r) = s.poll(t1 + RIP_SETTLE) else {
            panic!("not ready")
        };
        assert_eq!(r.url, "https://p/master.m3u8");
        assert_eq!(r.kind, StreamKind::Hls);
        assert_eq!(r.headers, pairs(&[("origin", "https://videostr.net")]));
        assert_eq!(r.captions.len(), 1);
    }

    #[test]
    fn rip_file_playlist() {
        let t0 = Instant::now();
        let mut s = SniffState::new(SniffMode::Rip, t0);
        s.on_message(playlist("https://files/1080.mp4", "file"), t0);
        let Poll::Ready(r) = s.poll(t0 + RIP_SETTLE) else {
            panic!("not ready")
        };
        assert_eq!(r.kind, StreamKind::File);
    }

    #[test]
    fn a_found_stream_outlives_the_deadline() {
        let t0 = Instant::now();
        let mut s = SniffState::new(SniffMode::Rip, t0);
        let late = t0 + RIP_TIMEOUT - Duration::from_millis(100);
        s.on_message(playlist("https://p/master.m3u8", "hls"), late);
        assert_eq!(s.poll(late), Poll::Pending(late + RIP_SETTLE));
        assert_eq!(s.poll(t0 + RIP_TIMEOUT), Poll::Pending(late + RIP_SETTLE));
        assert!(matches!(s.poll(late + RIP_SETTLE), Poll::Ready(_)));
    }

    #[test]
    fn playlist_kinds() {
        assert_eq!(playlist_kind("hls", "https://x/a"), StreamKind::Hls);
        assert_eq!(playlist_kind("DASH", "https://x/a"), StreamKind::Dash);
        assert_eq!(playlist_kind("file", "https://x/a.m3u8"), StreamKind::File);
        assert_eq!(playlist_kind("", "https://x/a.mpd?t=1"), StreamKind::Dash);
        assert_eq!(playlist_kind("", "https://x/a.MP4"), StreamKind::File);
        assert_eq!(playlist_kind("", "https://x/a"), StreamKind::Hls);
    }
}
