//! Request and navigation verdicts, ported from Android `AdBlock.kt`.
//!
//! Player and CDN hosts always pass. Any other host (including the cheap TLDs
//! popunder networks use) is blocked when the request is a main-frame load or
//! loads code (a script, document, frame, worker...). Everything else from an
//! unknown host passes, because video segments and captions live on arbitrary
//! hosts: media wins over every host rule except the main frame.

use url::Url;

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

/// `Sec-Fetch-Dest` values that load code (`AdBlock.CODE_DEST`).
const CODE_DEST: &[&str] = &[
    "script", "document", "iframe", "frame", "object", "embed", "worker",
];

/// Path extensions that load code when no `Sec-Fetch-Dest` is present.
const CODE_EXT: &[&str] = &["js", "mjs", "html", "htm"];

/// `Sec-Fetch-Dest` values for media, which never count as code.
const MEDIA_DEST: &[&str] = &["video", "audio", "track"];

/// `host` is `domain` or one of its subdomains.
fn matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// A player host or a subdomain of one.
pub fn is_player_host(host: &str) -> bool {
    PLAYER_HOSTS.iter().any(|d| matches(host, d))
}

/// A CDN allowlist host or a subdomain of one.
pub fn is_cdn_host(host: &str) -> bool {
    CDN_HOSTS.iter().any(|d| matches(host, d))
}

fn is_blocked_tld(host: &str) -> bool {
    let tld = host.rsplit('.').next().unwrap_or(host);
    BLOCKED_TLDS.contains(&tld)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostClass {
    Player,
    Cdn,
    BlockedTld,
    Other,
}

fn classify(host: &str) -> HostClass {
    if is_player_host(host) {
        HostClass::Player
    } else if is_cdn_host(host) {
        HostClass::Cdn
    } else if is_blocked_tld(host) {
        HostClass::BlockedTld
    } else {
        HostClass::Other
    }
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// The lowercased extension of the last path segment, or `""`.
fn extension(url: &Url) -> String {
    let path = url.path().to_ascii_lowercase();
    let last = path.rsplit('/').next().unwrap_or("");
    match last.rsplit_once('.') {
        Some((_, ext)) => ext.to_owned(),
        None => String::new(),
    }
}

/// Whether a request loads code. `Sec-Fetch-Dest` decides when present; without it
/// the path extension and the resource kind stand in for `AdBlock.looksLikeCode`
/// (the resource kind replaces its `Accept: text/html` check). Media never counts.
fn is_code(r: &RequestInfo, url: &Url) -> bool {
    if r.resource_kind == ResourceKind::Media {
        return false;
    }
    if let Some(dest) = r.fetch_dest {
        let dest = dest.trim().to_ascii_lowercase();
        if MEDIA_DEST.contains(&dest.as_str()) {
            return false;
        }
        return CODE_DEST.contains(&dest.as_str());
    }
    CODE_EXT.contains(&extension(url).as_str())
        || matches!(
            r.resource_kind,
            ResourceKind::Document | ResourceKind::Script
        )
}

/// The `AdBlock.kt` request verdict.
pub fn verdict(r: &RequestInfo) -> Verdict {
    let Ok(url) = Url::parse(r.url) else {
        return Verdict::Block;
    };
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return Verdict::Block;
    };
    // Android never sees WebSocket requests; WebView2 may, so they follow the host rules
    if !is_http(&url) && !matches!(url.scheme(), "ws" | "wss") {
        return Verdict::Block;
    }
    // Cloudflare challenge and insight endpoints on any origin
    if url.path().starts_with("/cdn-cgi/") {
        return Verdict::Allow;
    }
    match classify(&host) {
        HostClass::Player | HostClass::Cdn => Verdict::Allow,
        HostClass::BlockedTld | HostClass::Other => {
            if r.is_main_frame || is_code(r, &url) {
                Verdict::Block
            } else {
                Verdict::Allow
            }
        }
    }
}

/// Main frame: player hosts only. Subframes: player and CDN hosts.
/// `about:blank` and `about:srcdoc` pass so the host can park the view between sniffs
/// (WebView2 raises `NavigationStarting` for host-initiated loads, Android does not).
pub fn navigation_allowed(url: &str, main_frame: bool) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    if url.scheme() == "about" {
        return matches!(url.path(), "blank" | "srcdoc");
    }
    if !is_http(&url) {
        return false;
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    if main_frame {
        is_player_host(&host)
    } else {
        is_player_host(&host) || is_cdn_host(&host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ResourceKind::*;
    use Verdict::{Allow, Block};

    const MAIN: bool = true;
    const SUB: bool = false;

    /// (url, main frame, Sec-Fetch-Dest, resource kind, expected)
    type Case = (
        &'static str,
        bool,
        Option<&'static str>,
        ResourceKind,
        Verdict,
    );

    #[rustfmt::skip]
    const CASES: &[Case] = &[
        // player hosts and their subdomains always pass
        ("https://vidlink.pro/tv/1399/1/1", MAIN, Some("document"), Document, Allow),
        ("https://vidlink.pro/_next/static/app.js", SUB, Some("script"), Script, Allow),
        ("https://api.vidlink.pro/api/b/movie/1", SUB, Some("empty"), Fetch, Allow),
        ("https://ssl.p.jwpcdn.com/player/v/8/jwplayer.js", SUB, Some("script"), Script, Allow),
        ("https://cdn.jwplayer.com/libraries/x.js", SUB, None, Script, Allow),
        ("http://VIDLINK.PRO/movie/550", MAIN, Some("document"), Document, Allow),
        // CDN allowlist
        ("https://image.tmdb.org/t/p/w500/a.jpg", SUB, Some("image"), Image, Allow),
        ("https://cdn.jsdelivr.net/npm/hls.js", SUB, Some("script"), Script, Allow),
        ("https://fonts.gstatic.com/s/inter.woff2", SUB, Some("font"), Font, Allow),
        ("https://challenges.cloudflare.com/turnstile/v0/api.js", SUB, Some("script"), Script, Allow),
        ("https://www.googleapis.com/x", SUB, Some("iframe"), Document, Allow),
        ("https://rr3---sn-abc.googlevideo.com/videoplayback", SUB, Some("video"), Media, Allow),
        ("https://wsrv.nl/?url=x", SUB, Some("image"), Image, Allow),
        // CDN hosts pass as requests even in the main frame
        ("https://unpkg.com/react", MAIN, Some("document"), Document, Allow),
        // lookalikes are not subdomains
        ("https://evilvidlink.pro/ad.js", SUB, Some("script"), Script, Block),
        ("https://vidlink.pro.evil.com/ad.js", SUB, Some("script"), Script, Block),
        ("https://notgstatic.com/x.js", SUB, Some("script"), Script, Block),
        // unknown hosts: code is blocked
        ("https://ads.example.com/pop.js", SUB, Some("script"), Script, Block),
        ("https://ads.example.com/frame", SUB, Some("iframe"), Document, Block),
        ("https://ads.example.com/w", SUB, Some("worker"), Other, Block),
        ("https://ads.example.com/o", SUB, Some("object"), Other, Block),
        ("https://ads.example.com/e", SUB, Some("EMBED"), Other, Block),
        ("https://ads.example.com/f", SUB, Some("frame"), Document, Block),
        // unknown hosts: the main frame is blocked whatever it is
        ("https://ads.example.com/landing", MAIN, Some("document"), Document, Block),
        ("https://cdn.example.com/seg.ts", MAIN, None, Media, Block),
        // unknown hosts: non-code passes (video lives on arbitrary hosts)
        ("https://cdn.example.com/hls/master.m3u8", SUB, Some("empty"), XmlHttpRequest, Allow),
        ("https://cdn.example.com/v/seg-001.ts", SUB, Some("empty"), Fetch, Allow),
        ("https://cdn.example.com/playlist", SUB, Some("empty"), Fetch, Allow),
        ("https://cdn.example.com/poster.jpg", SUB, Some("image"), Image, Allow),
        ("https://cdn.example.com/en.vtt", SUB, Some("track"), TextTrack, Allow),
        ("https://cdn.example.com/style.css", SUB, Some("style"), Stylesheet, Allow),
        // Sec-Fetch-Dest decides over the extension
        ("https://cdn.example.com/lib.js", SUB, Some("empty"), Fetch, Allow),
        ("https://cdn.example.com/stream.bin", SUB, Some("script"), Script, Block),
        // media always passes, even from a cheap TLD or with a code-looking path
        ("https://abc.cfd/seg1.ts", SUB, Some("video"), Media, Allow),
        ("https://abc.top/a.html", SUB, None, Media, Allow),
        ("https://abc.top/a.html", SUB, Some("audio"), Media, Allow),
        // no Sec-Fetch-Dest: the extension or the resource kind decide
        ("https://ads.example.com/tag.js", SUB, None, Other, Block),
        ("https://ads.example.com/tag.MJS?v=1", SUB, None, Other, Block),
        ("https://ads.example.com/page.htm", SUB, None, XmlHttpRequest, Block),
        ("https://ads.example.com/x", SUB, None, Script, Block),
        ("https://ads.example.com/x", SUB, None, Document, Block),
        ("https://cdn.example.com/key", SUB, None, XmlHttpRequest, Allow),
        ("https://cdn.example.com/dir.js/seg", SUB, None, Fetch, Allow),
        // cheap TLDs follow the unknown-host rule
        ("https://pop.icu/p.js", SUB, Some("script"), Script, Block),
        ("https://pop.icu/beacon", SUB, Some("empty"), Fetch, Allow),
        ("https://x.monster/", MAIN, Some("document"), Document, Block),
        // Cloudflare's own paths pass on any host
        ("https://ads.example.com/cdn-cgi/challenge-platform/h/b/orchestrate/x.js", SUB, Some("script"), Script, Allow),
        ("https://vidlink.pro/cdn-cgi/rum", SUB, Some("empty"), Fetch, Allow),
        // WebSockets follow the host rules
        ("wss://ws.vidlink.pro/socket", SUB, Some("websocket"), Websocket, Allow),
        ("wss://live.example.com/socket", SUB, Some("websocket"), Websocket, Allow),
        // other non-http and unparsable URLs are blocked
        ("ftp://vidlink.pro/x", SUB, None, Other, Block),
        ("data:text/html,hi", SUB, Some("iframe"), Document, Block),
        ("not a url", SUB, None, Other, Block),
    ];

    #[test]
    fn verdict_table() {
        let mut failures = Vec::new();
        for &(url, is_main_frame, fetch_dest, resource_kind, want) in CASES {
            let got = verdict(&RequestInfo {
                url,
                is_main_frame,
                fetch_dest,
                resource_kind,
            });
            if got != want {
                failures.push(format!(
                    "{url} main={is_main_frame} dest={fetch_dest:?} {resource_kind:?}: got {got:?}, want {want:?}"
                ));
            }
        }
        assert!(CASES.len() >= 40);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn navigation() {
        let cases: &[(&str, bool, bool)] = &[
            ("https://vidlink.pro/movie/550?autoplay=true", MAIN, true),
            ("https://www.vidlink.pro/tv/1/1/1", MAIN, true),
            ("https://cdn.jwplayer.com/x", MAIN, true),
            ("https://challenges.cloudflare.com/cdn-cgi/x", MAIN, false),
            ("https://challenges.cloudflare.com/cdn-cgi/x", SUB, true),
            ("https://image.tmdb.org/x", SUB, true),
            ("https://ads.example.com/", MAIN, false),
            ("https://ads.example.com/", SUB, false),
            ("https://pop.icu/", SUB, false),
            ("intent://scan/#Intent;end", MAIN, false),
            ("javascript:alert(1)", SUB, false),
            ("about:blank", MAIN, true),
            ("about:srcdoc", SUB, true),
            ("about:config", MAIN, false),
            ("not a url", MAIN, false),
        ];
        for &(url, main, want) in cases {
            assert_eq!(navigation_allowed(url, main), want, "{url} main={main}");
        }
    }
}
