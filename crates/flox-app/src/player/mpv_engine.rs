//! The [`Engine`] over libmpv (flox-player).
//!
//! - Library prints play through the `flox://` protocol: the stream-cb opener resolves the URL
//!   to the entry's parts and opens a [`TdStream`] over them (through [`TdSource`]).
//! - Sniffed manifests play with the page's headers, the Chrome user agent and the captions
//!   added (not selected) once the file is loaded.
//! - Settings become options at creation (`base_options_with_ui_language`) and properties on
//!   every load (loudness filter, aspect, subtitle scale); speed and volume come from the
//!   controller.
//! - [`EventMapper`] turns mpv events into controller [`Input`]s: `Playback` on time, duration,
//!   pause and end changes, `Tracks` on track-list changes, `FirstFrame` on the first playback
//!   restart, and `EngineError` when a load or the stream fails.
//! - The engine never seeks by itself: the controller seeks on `FileLoaded`.
//!
//! The page-player hooks go to [`PageSurface`].

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use flox_core::lang;
use flox_core::model::MediaType;
use flox_core::settings::{AspectMode, Settings, SubtitleSize};
use flox_core::sniff::Caption;
use flox_player::ffi::MpvLib;
use flox_player::filters::loudness_af;
use flox_player::mpv::{Format, Mpv, MpvEvent};
use flox_player::options::base_options_with_ui_language;
use flox_player::stream_cb::{self, Canceller, Opener, StreamSource};
use flox_player::tracks::{audio_options, subtitle_tracks};
use flox_td::library::{Entry, Part};
use flox_td::stream::TdStream;
use flox_td::transport::TdTransport;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::controller::{Engine, Input, PageAction, Playback};
use super::web::PageSurface;

/// The custom protocol library prints play through.
pub const PROTOCOL: &str = "flox";

/// The user agent sent with sniffed streams (the page's own is dropped).
pub const USER_AGENT: &str = flox_web::host::USER_AGENT;

/// Properties the engine observes, with their formats.
pub const OBSERVED: &[(&str, Format)] = &[
    ("time-pos", Format::Double),
    ("duration", Format::Double),
    ("pause", Format::Flag),
    ("eof-reached", Format::Flag),
    ("track-list", Format::Node),
    ("demuxer-cache-state", Format::Node),
    ("hwdec-current", Format::String),
];

/// The part of mpv the engine drives; [`Mpv`] in the app, a recorder in tests.
pub trait MpvControl {
    fn command(&self, args: &[&str]) -> flox_core::error::Result<()>;
    fn set(&self, name: &str, value: Value) -> flox_core::error::Result<()>;
    fn get(&self, name: &str) -> flox_core::error::Result<Value>;
}

impl MpvControl for Mpv {
    fn command(&self, args: &[&str]) -> flox_core::error::Result<()> {
        Mpv::command(self, args)
    }

    fn set(&self, name: &str, value: Value) -> flox_core::error::Result<()> {
        self.set_property(name, value)
    }

    fn get(&self, name: &str) -> flox_core::error::Result<Value> {
        self.get_property(name)
    }
}

// ---- pure helpers ---------------------------------------------------------------------------

/// Percent-encodes everything but unreserved characters, for one URL path segment.
fn url_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `flox://<tmdb>/<movie|tv>/<season>/<episode>/<label>` for a library print.
pub fn flox_url(entry: &Entry) -> String {
    let k = entry.key;
    let media = match k.media {
        MediaType::Movie => "movie",
        MediaType::Tv => "tv",
    };
    format!(
        "{PROTOCOL}://{}/{media}/{}/{}/{}",
        k.tmdb,
        k.season,
        k.episode,
        url_segment(&entry.label())
    )
}

/// `keepaspect` and `panscan` for an aspect mode. Both are set on every change so a previous
/// mode never leaks into the next one.
pub fn aspect_properties(mode: AspectMode) -> [(&'static str, Value); 2] {
    let (keep, panscan) = match mode {
        AspectMode::Fit => (true, 0.0),
        AspectMode::Fill => (false, 0.0),
        AspectMode::Zoom => (true, 1.0),
    };
    [("keepaspect", json!(keep)), ("panscan", json!(panscan))]
}

/// `sub-scale`: the Android caption sizes 0.04 / 0.055 / 0.07 of the height.
pub fn sub_scale(size: SubtitleSize) -> f64 {
    match size {
        SubtitleSize::Small => 0.8,
        SubtitleSize::Normal => 1.0,
        SubtitleSize::Large => 1.25,
    }
}

/// The per-load properties from settings, in the order they are set.
pub fn load_properties(settings: &Settings) -> Vec<(&'static str, Value)> {
    let mut out: Vec<(&'static str, Value)> = aspect_properties(settings.aspect_mode).into();
    out.push(("sub-scale", json!(sub_scale(settings.subtitle_size))));
    out
}

/// `af set <filter>` when the loudness boost applies, else nothing.
pub fn loudness_command(settings: &Settings) -> Option<Vec<String>> {
    loudness_af(settings.loudness_boost, settings.loudness_gain_db)
        .map(|af| vec!["af".to_owned(), "set".to_owned(), af])
}

/// `http-header-fields` as a string list of `Name: value`.
pub fn header_fields(headers: &[(String, String)]) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|(k, v)| Value::String(format!("{k}: {v}")))
            .collect(),
    )
}

/// The title and ISO code of a caption language (an ISO code or an English name).
fn caption_language(language: &str) -> (String, String) {
    if let Some(name) = lang::display_name(language) {
        return (name.to_owned(), language.to_ascii_lowercase());
    }
    if let Some(code) = lang::iso_from_english_name(language) {
        return (language.to_owned(), code.to_owned());
    }
    (language.to_owned(), language.to_owned())
}

/// `sub-add <url> auto <title> <lang>`: added, never selected by the add itself.
pub fn caption_command(c: &Caption) -> Vec<String> {
    let (title, code) = caption_language(&c.language);
    vec![
        "sub-add".to_owned(),
        c.url.clone(),
        "auto".to_owned(),
        title,
        code,
    ]
}

/// `sub-add <path> auto English en` for a library print's subtitle.
pub fn library_subtitle_command(path: &Path) -> Vec<String> {
    vec![
        "sub-add".to_owned(),
        path.to_string_lossy().into_owned(),
        "auto".to_owned(),
        "English".to_owned(),
        "en".to_owned(),
    ]
}

fn selected_track(list: &Value, kind: &str) -> Option<i64> {
    list.as_array()?
        .iter()
        .find(|t| {
            t.get("type").and_then(Value::as_str) == Some(kind)
                && t.get("selected").and_then(Value::as_bool) == Some(true)
        })?
        .get("id")?
        .as_i64()
}

/// Turns mpv events into controller inputs and keeps what the overlay needs beyond them.
#[derive(Clone, Debug, Default)]
pub struct EventMapper {
    playback: Playback,
    first_frame: bool,
    buffered: f64,
    hwdec: Option<String>,
}

impl EventMapper {
    pub fn new() -> Self {
        Self::default()
    }

    /// The end of the demuxer cache in seconds (the seek bar's buffered band).
    pub fn buffered(&self) -> f64 {
        self.buffered
    }

    /// The last reported playback state.
    pub fn playback(&self) -> Playback {
        self.playback
    }

    /// The inputs one event produces.
    pub fn map(&mut self, event: MpvEvent) -> Vec<Input> {
        match event {
            MpvEvent::StartFile => {
                self.playback = Playback::default();
                self.first_frame = false;
                self.buffered = 0.0;
                Vec::new()
            }
            MpvEvent::FileLoaded => vec![Input::FileLoaded {
                duration: self.playback.duration,
            }],
            MpvEvent::PlaybackRestart => {
                if self.first_frame {
                    Vec::new()
                } else {
                    self.first_frame = true;
                    vec![Input::FirstFrame]
                }
            }
            MpvEvent::EndFile { reason, error } => {
                if reason == "error" {
                    vec![Input::EngineError(format!("mpv load failed ({error})"))]
                } else {
                    Vec::new()
                }
            }
            MpvEvent::PropertyChange { name, value } => self.property(&name, value),
            MpvEvent::Idle | MpvEvent::Shutdown => Vec::new(),
        }
    }

    fn property(&mut self, name: &str, value: Value) -> Vec<Input> {
        let before = self.playback;
        match name {
            "time-pos" => self.playback.time = value.as_f64().unwrap_or(0.0).max(0.0),
            "duration" => self.playback.duration = value.as_f64().unwrap_or(0.0).max(0.0),
            "pause" => self.playback.paused = value.as_bool().unwrap_or(false),
            "eof-reached" => self.playback.ended = value.as_bool().unwrap_or(false),
            "track-list" => {
                return vec![Input::Tracks {
                    audio: audio_options(&value),
                    subs: subtitle_tracks(&value),
                    aid: selected_track(&value, "audio"),
                    sid: selected_track(&value, "sub"),
                }];
            }
            "demuxer-cache-state" => {
                self.buffered = value
                    .get("cache-end")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .max(0.0);
                return Vec::new();
            }
            "hwdec-current" => {
                let current = value.as_str().map(str::to_owned);
                if current != self.hwdec {
                    tracing::info!("hwdec-current: {}", current.as_deref().unwrap_or("no"));
                    self.hwdec = current;
                }
                return Vec::new();
            }
            _ => return Vec::new(),
        }
        if self.playback == before {
            Vec::new()
        } else {
            vec![Input::Playback(self.playback)]
        }
    }
}

// ---- the flox:// stream -------------------------------------------------------------------

/// `flox://` URLs of the current load, mapped to their parts.
pub type Streams = Arc<Mutex<HashMap<String, Vec<Part>>>>;

/// A cancelled [`TdStream`] fails every later read with `Interrupted`, which the stream-cb
/// trampoline would retry forever; this makes it a plain failure.
pub fn fatal_interrupt(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::Interrupted {
        io::Error::other("stream cancelled")
    } else {
        e
    }
}

/// A [`TdStream`] as an mpv stream source. Closing (mpv `close_fn`) cancels the downloads and
/// deletes the parts' cached bytes.
pub struct TdSource {
    stream: Option<TdStream>,
    size: u64,
    cancel: CancellationToken,
}

impl TdSource {
    pub fn new(stream: TdStream) -> Self {
        Self {
            size: stream.size(),
            cancel: stream.cancel_handle(),
            stream: Some(stream),
        }
    }
}

impl Read for TdSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stream.as_mut() {
            Some(s) => s.read(buf).map_err(fatal_interrupt),
            None => Ok(0),
        }
    }
}

impl Seek for TdSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self.stream.as_mut() {
            Some(s) => s.seek(pos),
            None => Err(io::Error::other("stream closed")),
        }
    }
}

impl StreamSource for TdSource {
    fn size(&self) -> Option<u64> {
        Some(self.size)
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }

    fn canceller(&self) -> Option<Canceller> {
        let token = self.cancel.clone();
        Some(Arc::new(move || token.cancel()))
    }
}

impl Drop for TdSource {
    fn drop(&mut self) {
        let Some(stream) = self.stream.take() else {
            return;
        };
        // `close` blocks on the runtime: only from mpv's stream thread. On a runtime thread
        // the stream's own drop releases the files in the background.
        if tokio::runtime::Handle::try_current().is_ok() {
            drop(stream);
        } else if let Err(e) = stream.close() {
            tracing::debug!("closing a library stream failed: {e}");
        }
    }
}

/// The `flox://` opener: the parts registered for the URL, streamed from Telegram.
pub fn td_opener(
    streams: Streams,
    transport: Arc<dyn TdTransport>,
    runtime: tokio::runtime::Handle,
) -> Opener {
    Box::new(move |uri| {
        let parts = streams.lock().get(uri).cloned()?;
        match TdStream::open(transport.clone(), runtime.clone(), parts) {
            Ok(stream) => Some(Box::new(TdSource::new(stream)) as Box<dyn StreamSource>),
            Err(e) => {
                tracing::warn!("cannot open {uri}: {e}");
                None
            }
        }
    })
}

// ---- the engine ---------------------------------------------------------------------------

/// Telegram access for the `flox://` protocol.
#[derive(Clone)]
pub struct TdAccess {
    pub transport: Arc<dyn TdTransport>,
    pub runtime: tokio::runtime::Handle,
}

/// Creates and configures mpv: base options with the UI language, the observers, the
/// `flox://` protocol when Telegram is available, and the loudness filter.
pub fn create_mpv(
    lib: Arc<MpvLib>,
    settings: &Settings,
    ui_language: &str,
    td: Option<TdAccess>,
    streams: Streams,
) -> anyhow::Result<Arc<Mpv>> {
    let options = base_options_with_ui_language(settings, Some(ui_language));
    let pairs: Vec<(&str, &str)> = options.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mpv = Mpv::new(lib, &pairs).context("creating mpv")?;
    for (name, format) in OBSERVED {
        mpv.observe(name, *format)
            .with_context(|| format!("observing {name}"))?;
    }
    if let Some(td) = td {
        stream_cb::register(&mpv, PROTOCOL, td_opener(streams, td.transport, td.runtime))
            .context("registering flox://")?;
    }
    Ok(Arc::new(mpv))
}

/// The engine. Without mpv (libmpv missing or failed to start) every load fails, which the
/// controller treats as an engine error.
pub struct MpvEngine<M: MpvControl = Mpv> {
    mpv: Option<Arc<M>>,
    settings: Settings,
    streams: Streams,
    /// Commands that need a loaded file (`sub-add`), run on `file-loaded`.
    after_load: Vec<Vec<String>>,
    page: PageSurface,
}

impl<M: MpvControl> MpvEngine<M> {
    pub fn new(
        mpv: Option<Arc<M>>,
        settings: Settings,
        streams: Streams,
        page: PageSurface,
    ) -> Self {
        Self {
            mpv,
            settings,
            streams,
            after_load: Vec::new(),
            page,
        }
    }

    /// The mpv instance, when there is one.
    pub fn mpv(&self) -> Option<&Arc<M>> {
        self.mpv.as_ref()
    }

    pub fn available(&self) -> bool {
        self.mpv.is_some()
    }

    /// Inputs the engine produced outside mpv events (the page surface).
    pub fn take_inputs(&mut self) -> Vec<Input> {
        self.page.poll()
    }

    /// Runs the commands that waited for the file (call on mpv `file-loaded`).
    pub fn on_file_loaded(&mut self) {
        for args in std::mem::take(&mut self.after_load) {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            self.command(&refs);
        }
    }

    fn require(&self) -> anyhow::Result<&Arc<M>> {
        self.mpv
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("libmpv is not available"))
    }

    fn command(&self, args: &[&str]) {
        if let Some(m) = &self.mpv {
            if let Err(e) = m.command(args) {
                tracing::warn!("mpv {}: {e}", args.first().copied().unwrap_or(""));
            }
        }
    }

    fn set(&self, name: &str, value: Value) {
        if let Some(m) = &self.mpv {
            if let Err(e) = m.set(name, value) {
                tracing::warn!("mpv set {name}: {e}");
            }
        }
    }

    /// Settings that apply to every load.
    fn apply_settings(&self) {
        for (name, value) in load_properties(&self.settings) {
            self.set(name, value);
        }
        if let Some(args) = loudness_command(&self.settings) {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            self.command(&refs);
        }
    }

    fn loadfile(&self, url: &str) -> anyhow::Result<()> {
        let m = self.require()?;
        m.command(&["loadfile", url, "replace"])
            .with_context(|| format!("loadfile {url}"))
    }

    fn log_frame_drops(&self) {
        let Some(m) = &self.mpv else {
            return;
        };
        let dropped = m.get("frame-drop-count").ok().and_then(|v| v.as_i64());
        let frames = m
            .get("estimated-frame-number")
            .ok()
            .and_then(|v| v.as_i64());
        if let (Some(dropped), Some(frames)) = (dropped, frames) {
            tracing::info!("frame-drop-count {dropped} of {frames} frames");
        }
    }
}

impl<M: MpvControl> Engine for MpvEngine<M> {
    fn load_library(
        &mut self,
        entry: &Entry,
        subtitle: Option<&Path>,
        _start: u32,
    ) -> anyhow::Result<()> {
        self.require()?;
        let url = flox_url(entry);
        {
            let mut streams = self.streams.lock();
            streams.clear();
            streams.insert(url.clone(), entry.parts.clone());
        }
        self.apply_settings();
        self.after_load = subtitle.map(library_subtitle_command).into_iter().collect();
        self.loadfile(&url)
    }

    fn load_url(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        captions: &[Caption],
        _start: u32,
    ) -> anyhow::Result<()> {
        self.require()?;
        self.streams.lock().clear();
        self.set("http-header-fields", header_fields(headers));
        self.set("user-agent", json!(USER_AGENT));
        self.apply_settings();
        self.after_load = captions.iter().map(caption_command).collect();
        self.loadfile(url)
    }

    fn seek_to(&mut self, secs: f64) {
        let target = format!("{:.3}", secs.max(0.0));
        self.command(&["seek", &target, "absolute"]);
    }

    fn seek_by(&mut self, secs: i64) {
        let by = secs.to_string();
        self.command(&["seek", &by, "relative"]);
    }

    fn set_paused(&mut self, paused: bool) {
        self.set("pause", json!(paused));
    }

    fn set_speed(&mut self, speed: f32) {
        self.set("speed", json!(f64::from(speed)));
    }

    fn set_audio(&mut self, ids: &[i64]) {
        if let Some(id) = ids.first() {
            self.set("aid", json!(id));
        }
    }

    fn set_subtitle(&mut self, id: Option<i64>) {
        match id {
            Some(id) => self.set("sid", json!(id)),
            None => self.set("sid", json!("no")),
        }
    }

    fn set_volume(&mut self, percent: u32) {
        self.set("volume", json!(f64::from(percent)));
    }

    fn stop(&mut self) {
        self.after_load.clear();
        if self.mpv.is_some() {
            self.log_frame_drops();
            // Closing the stream drops TDLib's cached bytes for the parts.
            self.command(&["stop"]);
        }
        self.streams.lock().clear();
    }

    fn page_load(&mut self, url: &str) {
        self.page.load(url);
    }

    fn page_action(&mut self, action: PageAction) {
        self.page.action(action);
    }

    fn page_close(&mut self) {
        self.page.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use flox_core::model::EpisodeKey;
    use flox_player::tracks::AudioOption;

    #[derive(Clone, Debug, PartialEq)]
    enum Call {
        Command(Vec<String>),
        Set(String, Value),
    }

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<Call>>,
        fail_commands: bool,
    }

    impl MpvControl for Recorder {
        fn command(&self, args: &[&str]) -> flox_core::error::Result<()> {
            self.calls.lock().push(Call::Command(
                args.iter().map(|a| (*a).to_owned()).collect(),
            ));
            if self.fail_commands {
                return Err(flox_core::error::Error::Other("refused".into()));
            }
            Ok(())
        }

        fn set(&self, name: &str, value: Value) -> flox_core::error::Result<()> {
            self.calls.lock().push(Call::Set(name.to_owned(), value));
            Ok(())
        }

        fn get(&self, _name: &str) -> flox_core::error::Result<Value> {
            Ok(json!(0))
        }
    }

    fn cmd(args: &[&str]) -> Call {
        Call::Command(args.iter().map(|a| (*a).to_owned()).collect())
    }

    fn set(name: &str, value: Value) -> Call {
        Call::Set(name.to_owned(), value)
    }

    fn entry() -> Entry {
        Entry {
            key: EpisodeKey::episode(1399, 1, 3),
            quality: "2160p DV".to_owned(),
            codec: "hevc".to_owned(),
            parts: vec![
                Part {
                    message_id: 10,
                    file_id: 1,
                    size: 100,
                },
                Part {
                    message_id: 11,
                    file_id: 2,
                    size: 50,
                },
            ],
            subtitle: None,
            size: 150,
            newest_message_id: 11,
        }
    }

    fn engine(settings: Settings) -> (MpvEngine<Recorder>, Arc<Recorder>, Streams) {
        let rec = Arc::new(Recorder::default());
        let streams: Streams = Arc::default();
        let e = MpvEngine::new(
            Some(rec.clone()),
            settings,
            streams.clone(),
            PageSurface::unavailable(),
        );
        (e, rec, streams)
    }

    fn calls(rec: &Recorder) -> Vec<Call> {
        rec.calls.lock().drain(..).collect()
    }

    #[test]
    fn flox_urls_encode_the_label() {
        assert_eq!(flox_url(&entry()), "flox://1399/tv/1/3/2160p%20DV%20hevc");
        let mut movie = entry();
        movie.key = EpisodeKey::movie(27205);
        movie.quality = "1080p".to_owned();
        movie.codec = "h264".to_owned();
        assert_eq!(flox_url(&movie), "flox://27205/movie/0/0/1080p%20h264");
    }

    #[test]
    fn aspect_sets_both_properties_every_time() {
        assert_eq!(
            aspect_properties(AspectMode::Fit),
            [("keepaspect", json!(true)), ("panscan", json!(0.0))]
        );
        assert_eq!(
            aspect_properties(AspectMode::Fill),
            [("keepaspect", json!(false)), ("panscan", json!(0.0))]
        );
        assert_eq!(
            aspect_properties(AspectMode::Zoom),
            [("keepaspect", json!(true)), ("panscan", json!(1.0))]
        );
    }

    #[test]
    fn subtitle_sizes_map_to_sub_scale() {
        assert_eq!(sub_scale(SubtitleSize::Small), 0.8);
        assert_eq!(sub_scale(SubtitleSize::Normal), 1.0);
        assert_eq!(sub_scale(SubtitleSize::Large), 1.25);
    }

    #[test]
    fn loudness_only_when_boost_and_gain() {
        let off = Settings {
            loudness_boost: false,
            ..Settings::default()
        };
        assert_eq!(loudness_command(&off), None);
        let on = Settings {
            loudness_boost: true,
            loudness_gain_db: 6.0,
            ..Settings::default()
        };
        let args = loudness_command(&on).unwrap();
        assert_eq!(args[..2], ["af".to_owned(), "set".to_owned()]);
        assert!(args[2].starts_with("lavfi=[volume=6dB,acompressor="));
    }

    #[test]
    fn captions_and_subtitles_are_added_unselected() {
        let c = Caption {
            url: "https://x/en.vtt".to_owned(),
            language: "English".to_owned(),
            kind: "vtt".to_owned(),
        };
        assert_eq!(
            caption_command(&c),
            ["sub-add", "https://x/en.vtt", "auto", "English", "en"]
        );
        let c = Caption {
            url: "u".to_owned(),
            language: "fr".to_owned(),
            kind: "srt".to_owned(),
        };
        assert_eq!(
            caption_command(&c)[3..],
            ["French".to_owned(), "fr".to_owned()]
        );
        let c = Caption {
            url: "u".to_owned(),
            language: "Klingon".to_owned(),
            kind: "srt".to_owned(),
        };
        assert_eq!(
            caption_command(&c)[3..],
            ["Klingon".to_owned(), "Klingon".to_owned()]
        );
        assert_eq!(
            library_subtitle_command(Path::new("/tmp/s.srt")),
            ["sub-add", "/tmp/s.srt", "auto", "English", "en"]
        );
    }

    #[test]
    fn header_fields_are_a_string_list() {
        let h = vec![
            ("Referer".to_owned(), "https://vidlink.pro/".to_owned()),
            ("X-A".to_owned(), "1, 2".to_owned()),
        ];
        assert_eq!(
            header_fields(&h),
            json!(["Referer: https://vidlink.pro/", "X-A: 1, 2"])
        );
    }

    #[test]
    fn library_load_registers_parts_and_adds_the_subtitle_after_loading() {
        let settings = Settings {
            aspect_mode: AspectMode::Zoom,
            subtitle_size: SubtitleSize::Large,
            loudness_boost: false,
            ..Settings::default()
        };
        let (mut e, rec, streams) = engine(settings);
        e.load_library(&entry(), Some(Path::new("/s.srt")), 120)
            .unwrap();
        let url = "flox://1399/tv/1/3/2160p%20DV%20hevc";
        assert_eq!(streams.lock().get(url).map(Vec::len), Some(2));
        assert_eq!(
            calls(&rec),
            vec![
                set("keepaspect", json!(true)),
                set("panscan", json!(1.0)),
                set("sub-scale", json!(1.25)),
                cmd(&["loadfile", url, "replace"]),
            ],
            "no seek: the controller seeks on file-loaded"
        );
        e.on_file_loaded();
        assert_eq!(
            calls(&rec),
            vec![cmd(&["sub-add", "/s.srt", "auto", "English", "en"])]
        );
        e.on_file_loaded();
        assert!(calls(&rec).is_empty(), "added once");
        e.stop();
        assert!(streams.lock().is_empty());
        assert_eq!(calls(&rec), vec![cmd(&["stop"])]);
    }

    #[test]
    fn url_load_sets_headers_agent_filter_and_captions() {
        let settings = Settings {
            loudness_boost: true,
            loudness_gain_db: 4.0,
            ..Settings::default()
        };
        let (mut e, rec, _) = engine(settings);
        let headers = vec![("Referer".to_owned(), "https://vidlink.pro/".to_owned())];
        let captions = vec![Caption {
            url: "https://c/en.vtt".to_owned(),
            language: "en".to_owned(),
            kind: "vtt".to_owned(),
        }];
        e.load_url("https://m/master.m3u8", &headers, &captions, 0)
            .unwrap();
        let got = calls(&rec);
        assert_eq!(
            got[0],
            set(
                "http-header-fields",
                json!(["Referer: https://vidlink.pro/"])
            )
        );
        assert_eq!(got[1], set("user-agent", json!(USER_AGENT)));
        assert!(matches!(&got[5], Call::Command(a) if a[0] == "af" && a[1] == "set"));
        assert_eq!(
            got.last(),
            Some(&cmd(&["loadfile", "https://m/master.m3u8", "replace"]))
        );
        e.on_file_loaded();
        assert_eq!(
            calls(&rec),
            vec![cmd(&[
                "sub-add",
                "https://c/en.vtt",
                "auto",
                "English",
                "en"
            ])]
        );
    }

    #[test]
    fn controls_map_to_properties_and_commands() {
        let (mut e, rec, _) = engine(Settings::default());
        e.seek_to(75.5);
        e.seek_by(-30);
        e.set_paused(true);
        e.set_speed(1.25);
        e.set_audio(&[3, 4]);
        e.set_audio(&[]);
        e.set_subtitle(Some(2));
        e.set_subtitle(None);
        e.set_volume(115);
        assert_eq!(
            calls(&rec),
            vec![
                cmd(&["seek", "75.500", "absolute"]),
                cmd(&["seek", "-30", "relative"]),
                set("pause", json!(true)),
                set("speed", json!(1.25)),
                set("aid", json!(3)),
                set("sid", json!(2)),
                set("sid", json!("no")),
                set("volume", json!(115.0)),
            ]
        );
    }

    #[test]
    fn a_refused_loadfile_is_a_load_error() {
        let rec = Arc::new(Recorder {
            fail_commands: true,
            ..Recorder::default()
        });
        let mut e = MpvEngine::new(
            Some(rec),
            Settings::default(),
            Arc::default(),
            PageSurface::unavailable(),
        );
        assert!(e.load_url("x", &[], &[], 0).is_err());
    }

    #[test]
    fn without_mpv_loads_fail_and_controls_are_quiet() {
        let mut e: MpvEngine<Recorder> = MpvEngine::new(
            None,
            Settings::default(),
            Arc::default(),
            PageSurface::unavailable(),
        );
        assert!(!e.available());
        assert!(e.load_library(&entry(), None, 0).is_err());
        assert!(e.load_url("x", &[], &[], 0).is_err());
        e.seek_by(10);
        e.stop();
    }

    #[test]
    fn page_hooks_fail_fast_without_a_page_player() {
        let (mut e, _, _) = engine(Settings::default());
        e.page_load("https://vidlink.pro/movie/1");
        e.page_action(PageAction::Space);
        e.page_close();
        assert_eq!(e.take_inputs(), vec![Input::PageFailed]);
        assert!(e.take_inputs().is_empty());
    }

    fn prop(name: &str, value: Value) -> MpvEvent {
        MpvEvent::PropertyChange {
            name: name.to_owned(),
            value,
        }
    }

    #[test]
    fn events_become_playback_inputs() {
        let mut m = EventMapper::new();
        assert!(m.map(MpvEvent::StartFile).is_empty());
        let p = |time, duration, paused, ended| {
            vec![Input::Playback(Playback {
                time,
                duration,
                paused,
                ended,
            })]
        };
        assert_eq!(
            m.map(prop("duration", json!(600.0))),
            p(0.0, 600.0, false, false)
        );
        assert_eq!(
            m.map(MpvEvent::FileLoaded),
            vec![Input::FileLoaded { duration: 600.0 }]
        );
        assert_eq!(m.map(MpvEvent::PlaybackRestart), vec![Input::FirstFrame]);
        assert!(m.map(MpvEvent::PlaybackRestart).is_empty(), "after a seek");
        assert_eq!(
            m.map(prop("time-pos", json!(12.5))),
            p(12.5, 600.0, false, false)
        );
        assert!(m.map(prop("time-pos", json!(12.5))).is_empty(), "unchanged");
        assert_eq!(
            m.map(prop("pause", json!(true))),
            p(12.5, 600.0, true, false)
        );
        assert_eq!(
            m.map(prop("eof-reached", json!(true))),
            p(12.5, 600.0, true, true)
        );
        assert_eq!(
            m.map(prop("time-pos", Value::Null)),
            p(0.0, 600.0, true, true)
        );
        // a new file resets everything, including the first-frame latch
        assert!(m.map(MpvEvent::StartFile).is_empty());
        assert_eq!(m.playback(), Playback::default());
        assert_eq!(m.map(MpvEvent::PlaybackRestart), vec![Input::FirstFrame]);
    }

    #[test]
    fn load_errors_and_stops() {
        let mut m = EventMapper::new();
        assert_eq!(
            m.map(MpvEvent::EndFile {
                reason: "error".to_owned(),
                error: -13
            }),
            vec![Input::EngineError("mpv load failed (-13)".to_owned())]
        );
        for reason in ["stop", "eof", "quit", "redirect"] {
            assert!(m
                .map(MpvEvent::EndFile {
                    reason: reason.to_owned(),
                    error: 0
                })
                .is_empty());
        }
        assert!(m.map(MpvEvent::Idle).is_empty());
        assert!(m.map(MpvEvent::Shutdown).is_empty());
    }

    #[test]
    fn track_list_becomes_tracks_with_the_selection() {
        let mut m = EventMapper::new();
        let list = json!([
            {"id": 1, "type": "video", "selected": true},
            {"id": 1, "type": "audio", "lang": "eng", "codec": "eac3", "demux-channel-count": 6, "selected": false},
            {"id": 2, "type": "audio", "lang": "jpn", "codec": "aac", "demux-channel-count": 2, "selected": true},
            {"id": 1, "type": "sub", "lang": "eng", "codec": "subrip", "selected": false},
        ]);
        let got = m.map(prop("track-list", list));
        let [Input::Tracks {
            audio,
            subs,
            aid,
            sid,
        }] = got.as_slice()
        else {
            panic!("{got:?}");
        };
        assert_eq!(
            audio,
            &vec![
                AudioOption {
                    ids: vec![1],
                    lang: Some("eng".to_owned()),
                    codec: "eac3".to_owned(),
                    channels: 6
                },
                AudioOption {
                    ids: vec![2],
                    lang: Some("jpn".to_owned()),
                    codec: "aac".to_owned(),
                    channels: 2
                },
            ]
        );
        assert_eq!(subs.len(), 1);
        assert_eq!((*aid, *sid), (Some(2), None));
    }

    #[test]
    fn cache_state_feeds_the_buffered_band() {
        let mut m = EventMapper::new();
        assert!(m
            .map(prop(
                "demuxer-cache-state",
                json!({"cache-end": 95.25, "seekable-ranges": []})
            ))
            .is_empty());
        assert_eq!(m.buffered(), 95.25);
        m.map(prop("demuxer-cache-state", Value::Null));
        assert_eq!(m.buffered(), 0.0);
        assert!(m.map(prop("hwdec-current", json!("d3d11va"))).is_empty());
    }

    #[test]
    fn cancelled_reads_are_fatal() {
        let e = fatal_interrupt(io::Error::from(io::ErrorKind::Interrupted));
        assert_ne!(e.kind(), io::ErrorKind::Interrupted);
        let e = fatal_interrupt(io::Error::from(io::ErrorKind::TimedOut));
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }
}
