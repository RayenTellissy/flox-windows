//! Request and navigation verdicts, ported from Android `AdBlock.kt`.
//! Host lists are real data; the verdict logic is filled in by piece P12.

/// Embed player hosts (`Provider.HOSTS`). A host matches itself and its subdomains.
pub const PLAYER_HOSTS: &[&str] = &["vidlink.pro", "jwplayer.com", "jwpcdn.com"];

/// Third-party hosts the page may load from (`AdBlock.CDN_ALLOW`).
pub const CDN_HOSTS: &[&str] = &[
    "image.tmdb.org",
    "tmdb.org",
    "cdn.jsdelivr.net",
    "cdnjs.cloudflare.com",
    "unpkg.com",
    "fonts.googleapis.com",
    "fonts.gstatic.com",
    "gstatic.com",
    "googleapis.com",
    "cloudflare.com",
    "challenges.cloudflare.com",
    "static.cloudflareinsights.com",
    "jwpcdn.com",
    "wsrv.nl",
    "googlevideo.com",
];

/// Cheap TLDs used by popunder hosts (`AdBlock.BLOCKED_TLDS`).
pub const BLOCKED_TLDS: &[&str] = &[
    "cfd",
    "rest",
    "cyou",
    "sbs",
    "icu",
    "top",
    "click",
    "monster",
    "quest",
    "buzz",
    "bond",
    "lol",
    "mom",
    "autos",
    "boats",
    "motorcycles",
    "hair",
    "makeup",
    "skin",
    "beauty",
    "cam",
    "surf",
    "pics",
    "zip",
    "mov",
];

/// Whether a request may proceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    Allow,
    Block,
}

/// WebView2 resource context of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Document,
    Stylesheet,
    Image,
    Media,
    Font,
    Script,
    XmlHttpRequest,
    Fetch,
    TextTrack,
    EventSource,
    Websocket,
    Manifest,
    Other,
}

/// One intercepted request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestInfo<'a> {
    pub url: &'a str,
    pub is_main_frame: bool,
    /// The `Sec-Fetch-Dest` header, when present.
    pub fetch_dest: Option<&'a str>,
    pub resource_kind: ResourceKind,
}

/// The `AdBlock.kt` request verdict. Filled in by P12.
#[allow(clippy::unimplemented)]
pub fn verdict(_r: &RequestInfo) -> Verdict {
    unimplemented!("flox_web::policy::verdict (P12)")
}

/// Main frame: player hosts only. Subframes: player and CDN hosts. Filled in by P12.
#[allow(clippy::unimplemented)]
pub fn navigation_allowed(_url: &str, _main_frame: bool) -> bool {
    unimplemented!("flox_web::policy::navigation_allowed (P12)")
}
