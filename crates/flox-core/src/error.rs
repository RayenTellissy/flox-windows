//! The one error type shared by every Flox crate.

/// Errors raised anywhere in Flox.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// An operation this build does not provide. Carries `crate::module::fn`; nothing in
    /// the app raises it today, and it stays for callers that match on it.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("url: {0}")]
    Url(#[from] url::ParseError),
    #[error("tmdb: {0}")]
    Tmdb(String),
    /// A TDLib `{"@type":"error"}` response.
    #[error("telegram error {code}: {message}")]
    Td { code: i32, message: String },
    /// A negative libmpv status code.
    #[error("mpv error {code}: {message}")]
    Mpv { code: i32, message: String },
    /// A native library (tdjson, libmpv) could not be loaded.
    #[error("library load: {0}")]
    Load(String),
    /// A required external tool (ffmpeg, ffprobe, yt-dlp) is missing or failed.
    #[error("tool: {0}")]
    Tool(String),
    /// A download target is a web page rather than a media file.
    #[error("not a plain file: {0}")]
    NotAFile(String),
    #[error("image: {0}")]
    Image(String),
    /// A platform feature that does not exist on this OS (WebView2 on macOS).
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

/// Result alias over [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
