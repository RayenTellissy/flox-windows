//! MPD parsing and segment download, as on the Mac.
//!
//! The manifest is read by string matching, not as XML: each `<Representation>`
//! block gives its `mimeType`, `height`, `id` and `<SegmentTemplate>`, and the
//! segment count comes from the template duration or the `<SegmentTimeline>`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use flox_core::error::{Error, Result};
use futures::{StreamExt, TryStreamExt};
use reqwest::header::USER_AGENT as USER_AGENT_HEADER;
use reqwest::Client;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::file::{client, USER_AGENT};

/// Segments fetched at once. They are still written in order.
pub const PARALLEL_SEGMENTS: usize = 6;

/// Attempts per segment.
pub const ATTEMPTS: u32 = 4;

/// Backoff unit: the wait after attempt `n` (from 1) is `n` times this.
pub const BACKOFF: Duration = Duration::from_millis(500);

/// Share of the overall progress taken by the video track.
pub const VIDEO_SHARE: f32 = 0.9;

/// The video track's file name inside the download folder.
pub const VIDEO_FILE: &str = "v.m4s";

/// The audio track's file name inside the download folder.
pub const AUDIO_FILE: &str = "a.m4s";

/// One representation with its segment URLs already resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Representation {
    pub id: String,
    pub mime_type: String,
    /// 0 when the manifest gives none (audio).
    pub height: u32,
    /// The initialization segment.
    pub init: Url,
    /// Media segments, numbered from 1.
    pub segments: Vec<Url>,
}

/// A parsed manifest (tallest video and first audio).
#[derive(Clone, Debug, PartialEq)]
pub struct Mpd {
    /// `mediaPresentationDuration` in seconds, 0 when absent.
    pub duration: f64,
    pub video: Representation,
    pub audio: Option<Representation>,
}

/// The value of attribute `name` in `s`, matched at an attribute boundary.
fn attr<'a>(name: &str, s: &'a str) -> Option<&'a str> {
    let needle = format!("{name}=\"");
    let mut from = 0;
    while let Some(i) = s[from..].find(&needle) {
        let at = from + i;
        let bounded = s[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
        let start = at + needle.len();
        if bounded {
            let rest = &s[start..];
            return Some(&rest[..rest.find('"').unwrap_or(rest.len())]);
        }
        from = start;
    }
    None
}

/// `PT#H#M#S` in seconds; unknown designators are skipped, a bad spec gives 0.
fn parse_duration(xml: &str) -> f64 {
    let Some(spec) = attr("mediaPresentationDuration", xml) else {
        return 0.0;
    };
    let Some(spec) = spec.strip_prefix("PT") else {
        return 0.0;
    };
    let (mut h, mut m, mut s) = (0.0, 0.0, 0.0);
    let mut num = String::new();
    for ch in spec.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
        } else {
            let v = num.parse::<f64>().unwrap_or(0.0);
            num.clear();
            match ch {
                'H' => h = v,
                'M' => m = v,
                'S' => s = v,
                _ => {}
            }
        }
    }
    h * 3600.0 + m * 60.0 + s
}

/// Segment count from a template: the timeline when present, else duration / segment length.
fn segment_count(template: &str, duration: f64) -> usize {
    if let Some(tl) = template.find("<SegmentTimeline") {
        let timeline = &template[tl..];
        let timeline = &timeline[..timeline
            .find("</SegmentTimeline>")
            .unwrap_or(timeline.len())];
        let mut count: i64 = 0;
        let mut rest = timeline;
        while let Some(i) = rest.find("<S ") {
            let tag = &rest[i..];
            let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
            count += 1 + attr("r", tag)
                .and_then(|r| r.parse::<i64>().ok())
                .unwrap_or(0);
            rest = &rest[i + 3..];
        }
        return usize::try_from(count).unwrap_or(0);
    }
    let timescale = attr("timescale", template)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0);
    let seg = attr("duration", template)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    if seg > 0.0 && timescale > 0.0 {
        let n = (duration / (seg / timescale)).ceil();
        if n.is_finite() && n > 0.0 {
            return n as usize;
        }
    }
    0
}

/// Fills `$RepresentationID$`, `$Number%05d$` and `$Number$`.
fn fill(template: &str, id: &str, number: Option<usize>) -> String {
    let mut name = template.replace("$RepresentationID$", id);
    if let Some(n) = number {
        name = name
            .replacen("$Number%05d$", &format!("{n:05}"), 1)
            .replace("$Number$", &n.to_string());
    }
    name
}

/// String-level MPD parse, resolving segment URLs against `base`.
pub fn parse(xml: &str, base: &Url) -> Result<Mpd> {
    let duration = parse_duration(xml);
    let mut reps: Vec<Representation> = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<Representation ") {
        let block = &rest[i..];
        let end = block
            .find("</Representation>")
            .map_or(block.len(), |e| e + "</Representation>".len());
        let rep = &block[..end];
        let head = &rep[..rep.find('>').unwrap_or(rep.len())];
        let template = rep.find("<SegmentTemplate").map_or(rep, |t| &rep[t..]);
        let template_head = &template[..template.find('>').unwrap_or(template.len())];
        let id = attr("id", head).unwrap_or_default().to_string();
        let init = attr("initialization", template_head).unwrap_or_default();
        let media = attr("media", template_head).unwrap_or_default();
        let count = segment_count(template, duration);
        let segments = (1..=count)
            .map(|n| base.join(&fill(media, &id, Some(n))))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        reps.push(Representation {
            mime_type: attr("mimeType", head).unwrap_or_default().to_string(),
            height: attr("height", head)
                .and_then(|h| h.parse().ok())
                .unwrap_or(0),
            init: base.join(&fill(init, &id, None))?,
            segments,
            id,
        });
        rest = &block[end..];
    }

    let mut video: Option<Representation> = None;
    let mut audio: Option<Representation> = None;
    for r in reps {
        if r.mime_type.starts_with("video") {
            if video.as_ref().is_none_or(|v| r.height > v.height) {
                video = Some(r);
            }
        } else if audio.is_none() {
            audio = Some(r);
        }
    }
    let video = video.ok_or_else(|| Error::Other("no video representation".into()))?;
    if video.segments.is_empty() {
        return Err(Error::Other("could not read the segment list".into()));
    }
    Ok(Mpd {
        duration,
        video,
        audio,
    })
}

/// Downloads into `dir`, returning (video, audio) track files. `progress` is 0.0..=1.0.
pub async fn download(
    mpd: &Mpd,
    headers: &[(String, String)],
    dir: &Path,
    progress: impl Fn(f32) + Send,
    cancel: CancellationToken,
) -> Result<(PathBuf, Option<PathBuf>)> {
    download_with(mpd, headers, dir, progress, cancel, BACKOFF).await
}

/// [`download`] with the backoff unit injected.
pub(crate) async fn download_with(
    mpd: &Mpd,
    headers: &[(String, String)],
    dir: &Path,
    progress: impl Fn(f32) + Send,
    cancel: CancellationToken,
    backoff: Duration,
) -> Result<(PathBuf, Option<PathBuf>)> {
    // Behind a lock so the future stays `Send` without asking `progress` to be `Sync`.
    let progress = parking_lot::Mutex::new(progress);
    let report = |p: f32| (progress.lock())(p);
    let work = async {
        let fetcher = Fetcher {
            client: client(None)?,
            headers,
            backoff,
        };
        let share = if mpd.audio.is_some() {
            VIDEO_SHARE
        } else {
            1.0
        };
        let video = dir.join(VIDEO_FILE);
        fetcher
            .track(&mpd.video, &video, |p| report(p * share))
            .await?;
        let audio = match &mpd.audio {
            Some(rep) => {
                let path = dir.join(AUDIO_FILE);
                fetcher
                    .track(rep, &path, |p| report(share + p * (1.0 - share)))
                    .await?;
                Some(path)
            }
            None => None,
        };
        Ok((video, audio))
    };
    tokio::select! {
        _ = cancel.cancelled() => Err(Error::Cancelled),
        r = work => r,
    }
}

struct Fetcher<'a> {
    client: Client,
    headers: &'a [(String, String)],
    backoff: Duration,
}

impl Fetcher<'_> {
    async fn once(&self, url: &Url) -> Result<Bytes> {
        let mut req = self.client.get(url.clone());
        for (k, v) in self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.header(USER_AGENT_HEADER, USER_AGENT).send().await?;
        let code = resp.status().as_u16();
        if code != 200 {
            let name = url
                .path_segments()
                .and_then(|mut s| s.next_back())
                .unwrap_or_default();
            return Err(Error::Other(format!("http {code} for {name}")));
        }
        Ok(resp.bytes().await?)
    }

    /// Up to [`ATTEMPTS`] tries with a growing wait between them.
    async fn fetch(&self, url: &Url) -> Result<Bytes> {
        let mut attempt = 1;
        loop {
            match self.once(url).await {
                Ok(b) => return Ok(b),
                Err(e) if attempt >= ATTEMPTS => return Err(e),
                Err(e) => {
                    tracing::debug!("dash fetch attempt {attempt} failed: {e}");
                    tokio::time::sleep(self.backoff * attempt).await;
                    attempt += 1;
                }
            }
        }
    }

    /// A media segment; a 404 on either of the last two becomes empty.
    async fn segment(&self, url: &Url, number: usize, total: usize) -> Result<Bytes> {
        match self.fetch(url).await {
            Err(Error::Other(m)) if number + 1 >= total && m.starts_with("http 404") => {
                tracing::debug!("dash tolerating a missing tail segment: {m}");
                Ok(Bytes::new())
            }
            r => r,
        }
    }

    /// Writes the init and every segment of `rep` to `out`, in order.
    async fn track(&self, rep: &Representation, out: &Path, progress: impl Fn(f32)) -> Result<()> {
        let mut file = tokio::fs::File::create(out).await?;
        file.write_all(&self.fetch(&rep.init).await?).await?;
        let total = rep.segments.len();
        // Indices rather than borrowed URLs keep the stream `Send`-provable.
        let mut segments = futures::stream::iter(0..total)
            .map(|i| self.segment(&rep.segments[i], i + 1, total))
            .buffered(PARALLEL_SEGMENTS);
        let mut done = 0usize;
        while let Some(data) = segments.try_next().await? {
            file.write_all(&data).await?;
            done += 1;
            progress(done as f32 / total.max(1) as f32);
        }
        file.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const TEMPLATE_MPD: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT0H0M10.0S" minBufferTime="PT2S">
  <Period id="0">
    <AdaptationSet id="0" contentType="video">
      <Representation id="v480" mimeType="video/mp4" codecs="avc1" bandwidth="800000" width="854" height="480">
        <SegmentTemplate timescale="1000" duration="4000" initialization="v/$RepresentationID$/init.mp4" media="v/$RepresentationID$/seg-$Number%05d$.m4s" startNumber="1"/>
      </Representation>
      <Representation id="v1080" mimeType="video/mp4" codecs="hvc1" bandwidth="4000000" width="1920" height="1080">
        <SegmentTemplate timescale="1000" duration="4000" initialization="v/$RepresentationID$/init.mp4" media="v/$RepresentationID$/seg-$Number%05d$.m4s" startNumber="1"/>
      </Representation>
      <Representation id="v1080b" mimeType="video/mp4" codecs="avc1" bandwidth="3000000" width="1920" height="1080">
        <SegmentTemplate timescale="1000" duration="4000" initialization="x.mp4" media="x-$Number$.m4s"/>
      </Representation>
    </AdaptationSet>
    <AdaptationSet id="1" contentType="audio">
      <Representation id="aud_en" mimeType="audio/mp4" codecs="mp4a.40.2" bandwidth="128000">
        <SegmentTemplate timescale="48000" duration="192000" initialization="a/$RepresentationID$-init.mp4" media="a/$RepresentationID$-$Number$.m4s"/>
      </Representation>
      <Representation id="aud_fr" mimeType="audio/mp4" codecs="mp4a.40.2" bandwidth="128000">
        <SegmentTemplate timescale="48000" duration="192000" initialization="b-init.mp4" media="b-$Number$.m4s"/>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    const TIMELINE_MPD: &str = r#"<MPD type="static" mediaPresentationDuration="PT1H2M3.5S">
  <Period>
    <AdaptationSet>
      <Representation id="720" mimeType="video/mp4" height="720">
        <SegmentTemplate timescale="90000" initialization="$RepresentationID$/init.m4s" media="$RepresentationID$/$Number$.m4s">
          <SegmentTimeline>
            <S t="0" d="180000" r="2"/>
            <S d="90000"/>
          </SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    fn base(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn strs(urls: &[Url]) -> Vec<String> {
        urls.iter().map(Url::to_string).collect()
    }

    #[test]
    fn parses_a_template_manifest() {
        let mpd = parse(
            TEMPLATE_MPD,
            &base("https://cdn.example/show/ep1/manifest.mpd?tok=1"),
        )
        .unwrap();
        assert_eq!(mpd.duration, 10.0);
        assert_eq!(mpd.video.id, "v1080");
        assert_eq!(mpd.video.height, 1080);
        assert_eq!(
            mpd.video.init.as_str(),
            "https://cdn.example/show/ep1/v/v1080/init.mp4"
        );
        assert_eq!(
            strs(&mpd.video.segments),
            [
                "https://cdn.example/show/ep1/v/v1080/seg-00001.m4s",
                "https://cdn.example/show/ep1/v/v1080/seg-00002.m4s",
                "https://cdn.example/show/ep1/v/v1080/seg-00003.m4s",
            ]
        );
        let audio = mpd.audio.unwrap();
        assert_eq!(audio.id, "aud_en");
        assert_eq!(audio.height, 0);
        assert_eq!(
            audio.init.as_str(),
            "https://cdn.example/show/ep1/a/aud_en-init.mp4"
        );
        assert_eq!(
            strs(&audio.segments),
            [
                "https://cdn.example/show/ep1/a/aud_en-1.m4s",
                "https://cdn.example/show/ep1/a/aud_en-2.m4s",
                "https://cdn.example/show/ep1/a/aud_en-3.m4s",
            ]
        );
    }

    #[test]
    fn parses_a_timeline_manifest() {
        let mpd = parse(TIMELINE_MPD, &base("https://cdn.example/x/master.mpd")).unwrap();
        assert_eq!(mpd.duration, 3723.5);
        assert_eq!(mpd.video.id, "720");
        assert_eq!(
            mpd.video.init.as_str(),
            "https://cdn.example/x/720/init.m4s"
        );
        assert_eq!(
            strs(&mpd.video.segments),
            [
                "https://cdn.example/x/720/1.m4s",
                "https://cdn.example/x/720/2.m4s",
                "https://cdn.example/x/720/3.m4s",
                "https://cdn.example/x/720/4.m4s",
            ]
        );
        assert_eq!(mpd.audio, None);
    }

    #[test]
    fn rejects_manifests_without_video_or_segments() {
        let b = base("https://cdn.example/m.mpd");
        let audio_only = r#"<Representation id="a" mimeType="audio/mp4"><SegmentTemplate timescale="1" duration="2" media="$Number$"/></Representation>"#;
        assert_eq!(
            parse(audio_only, &b).unwrap_err().to_string(),
            "no video representation"
        );
        let empty = r#"<Representation id="v" mimeType="video/mp4" height="720"><SegmentTemplate media="$Number$"/></Representation>"#;
        assert_eq!(
            parse(empty, &b).unwrap_err().to_string(),
            "could not read the segment list"
        );
    }

    #[test]
    fn attributes_match_whole_names() {
        assert_eq!(attr("id", r#"<Representation xid="1" id="2">"#), Some("2"));
        assert_eq!(
            attr("height", r#"<R maxheight="9" height="720">"#),
            Some("720")
        );
        assert_eq!(attr("r", r#"<S t="0" d="1">"#), None);
    }

    fn manifest(server: &MockServer, video_segments: usize, with_audio: bool) -> Mpd {
        let duration = format!("PT{}S", video_segments * 2);
        let audio = if with_audio {
            r#"<Representation id="a1" mimeType="audio/mp4"><SegmentTemplate timescale="1" duration="10" initialization="a/init" media="a/$Number$"/></Representation>"#
        } else {
            ""
        };
        let xml = format!(
            r#"<MPD mediaPresentationDuration="{duration}">
<Representation id="v1" mimeType="video/mp4" height="1080"><SegmentTemplate timescale="1" duration="2" initialization="v/init" media="v/$Number$"/></Representation>
{audio}</MPD>"#
        );
        parse(&xml, &base(&format!("{}/dash/manifest.mpd", server.uri()))).unwrap()
    }

    async fn serve(server: &MockServer, at: &str, body: &str, delay_ms: u64) {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body)
                    .set_delay(Duration::from_millis(delay_ms)),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn writes_segments_in_order() {
        let server = MockServer::start().await;
        serve(&server, "/dash/v/init", "[vi]", 0).await;
        for n in 1..=8u64 {
            // Earlier segments answer later so completion order is reversed within a window.
            serve(
                &server,
                &format!("/dash/v/{n}"),
                &format!("[v{n}]"),
                (9 - n) * 25,
            )
            .await;
        }
        serve(&server, "/dash/a/init", "[ai]", 0).await;
        serve(&server, "/dash/a/1", "[a1]", 0).await;
        serve(&server, "/dash/a/2", "[a2]", 0).await;
        let mpd = manifest(&server, 8, true);
        assert_eq!(mpd.audio.as_ref().map(|a| a.segments.len()), Some(2));

        let dir = tempfile::tempdir().unwrap();
        let reports = Mutex::new(Vec::new());
        let headers = [("Referer".to_string(), "https://vidlink.pro/".to_string())];
        let (v, a) = download(
            &mpd,
            &headers,
            dir.path(),
            |p| reports.lock().push(p),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(v, dir.path().join("v.m4s"));
        assert_eq!(
            std::fs::read_to_string(&v).unwrap(),
            "[vi][v1][v2][v3][v4][v5][v6][v7][v8]"
        );
        let a = a.unwrap();
        assert_eq!(a, dir.path().join("a.m4s"));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "[ai][a1][a2]");

        let reports = reports.lock().clone();
        assert_eq!(reports.len(), 10);
        assert!((reports[7] - 0.9).abs() < 1e-6);
        assert!((reports[8] - 0.95).abs() < 1e-6);
        assert!((reports[9] - 1.0).abs() < 1e-6);
        assert!(reports.windows(2).all(|w| w[0] <= w[1]));

        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| {
            r.headers
                .get("referer")
                .is_some_and(|h| h == "https://vidlink.pro/")
                && r.headers
                    .get("user-agent")
                    .is_some_and(|h| h.as_bytes().starts_with(b"Mozilla/5.0"))
        }));
    }

    #[tokio::test]
    async fn retries_failed_segments() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dash/v/2"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(3)
            .with_priority(1)
            .mount(&server)
            .await;
        serve(&server, "/dash/v/init", "i", 0).await;
        for n in 1..=3 {
            serve(&server, &format!("/dash/v/{n}"), &n.to_string(), 0).await;
        }
        let mpd = manifest(&server, 3, false);
        let dir = tempfile::tempdir().unwrap();
        let reports = Mutex::new(Vec::new());
        let (v, a) = download_with(
            &mpd,
            &[],
            dir.path(),
            |p| reports.lock().push(p),
            CancellationToken::new(),
            Duration::from_millis(5),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(v).unwrap(), "i123");
        assert_eq!(a, None);
        assert_eq!(reports.lock().last().copied(), Some(1.0));
        let hits = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/dash/v/2")
            .count();
        assert_eq!(hits, 4);
    }

    #[tokio::test]
    async fn gives_up_after_four_attempts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dash/v/1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        serve(&server, "/dash/v/init", "i", 0).await;
        for n in 2..=4 {
            serve(&server, &format!("/dash/v/{n}"), "x", 0).await;
        }
        let mpd = manifest(&server, 4, false);
        let dir = tempfile::tempdir().unwrap();
        let err = download_with(
            &mpd,
            &[],
            dir.path(),
            |_| {},
            CancellationToken::new(),
            Duration::from_millis(5),
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "http 500 for 1");
        let hits = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/dash/v/1")
            .count();
        assert_eq!(hits, 4);
    }

    #[tokio::test]
    async fn tolerates_404_on_the_last_two_segments() {
        let server = MockServer::start().await;
        serve(&server, "/dash/v/init", "i", 0).await;
        for n in 1..=3 {
            serve(&server, &format!("/dash/v/{n}"), &n.to_string(), 0).await;
        }
        // 4 and 5 are missing: wiremock answers 404.
        let mpd = manifest(&server, 5, false);
        let dir = tempfile::tempdir().unwrap();
        let (v, _) = download_with(
            &mpd,
            &[],
            dir.path(),
            |_| {},
            CancellationToken::new(),
            Duration::from_millis(5),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(v).unwrap(), "i123");
    }

    #[tokio::test]
    async fn a_404_before_the_tail_fails() {
        let server = MockServer::start().await;
        serve(&server, "/dash/v/init", "i", 0).await;
        for n in [1, 2, 4, 5] {
            serve(&server, &format!("/dash/v/{n}"), &n.to_string(), 0).await;
        }
        let mpd = manifest(&server, 5, false);
        let dir = tempfile::tempdir().unwrap();
        let err = download_with(
            &mpd,
            &[],
            dir.path(),
            |_| {},
            CancellationToken::new(),
            Duration::from_millis(5),
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "http 404 for 3");
    }

    #[tokio::test]
    async fn cancel_stops_the_download() {
        let server = MockServer::start().await;
        serve(&server, "/dash/v/init", "i", 30_000).await;
        let mpd = manifest(&server, 2, false);
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.cancel();
        });
        let err = download(&mpd, &[], dir.path(), |_| {}, cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }

    #[test]
    fn download_future_is_send() {
        fn assert_send<T: Send>(_: T) {}
        let mpd = parse(TIMELINE_MPD, &base("https://cdn.example/x/master.mpd")).unwrap();
        assert_send(download(
            &mpd,
            &[],
            Path::new("x"),
            |_| {},
            CancellationToken::new(),
        ));
    }
}
