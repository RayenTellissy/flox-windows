//! 4KHDHub scraping and HubCloud resolution, ported from the Mac `HdHub` and `HubCloud`.
//!
//! The parsers are plain functions over HTML so they can be tested against saved
//! pages; the async entry points only add the HTTP round trips.

/// A lazily compiled regex. `None` only if the literal pattern is invalid, which the
/// tests rule out, so callers treat it as "no match".
macro_rules! re {
    ($p:expr) => {{
        static R: std::sync::LazyLock<Option<regex::Regex>> =
            std::sync::LazyLock::new(|| regex::Regex::new($p).ok());
        R.as_ref()
    }};
}

mod fourk;
mod hubcloud;
mod unwrap;

use flox_core::error::{Error, Result};
use flox_core::model::{MediaType, TitleDetails};
use regex::Regex;
use reqwest::header::{ACCEPT, REFERER, USER_AGENT as USER_AGENT_HEADER};
use reqwest::StatusCode;
use url::Url;

use crate::job::Tag;

pub use fourk::{parse_search, parse_variants};
pub use hubcloud::{parse_direct, parse_hop};
pub use unwrap::unwrap_redirect;

/// Site root.
pub const BASE_URL: &str = "https://4khdhub.one";

/// The desktop Chrome user agent every request is sent with.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

/// One release on a title page, such as `S04 SDR 2160p WEB-DL H265`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub label: String,
    pub season: Option<u32>,
    pub height: u32,
    pub dv: bool,
    pub hdr: bool,
    pub remux: bool,
    pub size_bytes: u64,
    pub files: Vec<HubFile>,
}

/// One downloadable file of a variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubFile {
    pub episode: Option<u32>,
    pub name: String,
    pub size_bytes: u64,
    pub link: String,
}

impl Variant {
    /// Builds a variant, reading height, Dolby Vision, HDR and remux from the label.
    pub fn new(label: String, season: Option<u32>, files: Vec<HubFile>) -> Self {
        let height = first(re!(r"(\d{3,4})p"), &label)
            .and_then(|h| h.parse().ok())
            .unwrap_or(0);
        let dv = re!(r"\b(DoVi|DV)\b").is_some_and(|r| r.is_match(&label));
        let hdr = re!(r"\bHDR").is_some_and(|r| r.is_match(&label));
        let remux = re!(r"(?i)REMUX").is_some_and(|r| r.is_match(&label));
        let size_bytes = files.iter().map(|f| f.size_bytes).sum();
        Self {
            label,
            season,
            height,
            dv,
            hdr,
            remux,
            size_bytes,
            files,
        }
    }

    /// The dynamic range tag: DV wins over HDR, else SDR.
    pub fn tag(&self) -> Tag {
        if self.dv {
            Tag::Dv
        } else if self.hdr {
            Tag::Hdr
        } else {
            Tag::Sdr
        }
    }

    /// The library quality this variant would upload as, `"<h>p[ DV|HDR]"`.
    pub fn quality(&self) -> String {
        let height = (self.height > 0).then(|| format!("{}p", self.height));
        let tag = match self.tag() {
            Tag::Dv => Some("DV".to_owned()),
            Tag::Hdr => Some("HDR".to_owned()),
            Tag::Sdr => None,
        };
        [height, tag]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Highest resolution first, then plain HDR/SDR over Dolby Vision, then WEB-DL over remux.
    fn rank(&self) -> (u32, bool, bool) {
        (self.height, !self.dv, !self.remux)
    }
}

/// Sorts variants best first, as on the Mac (stable, so page order breaks ties).
pub fn sort_variants(variants: &mut [Variant]) {
    variants.sort_by_key(|v| std::cmp::Reverse(v.rank()));
}

/// The variant to preselect: the first holding every wanted episode, else the first.
/// Movies pass no wanted episodes, which selects the first.
pub fn default_variant(shown: &[Variant], wanted: &[u32]) -> Option<usize> {
    if shown.is_empty() {
        return None;
    }
    let complete = shown.iter().position(|v| {
        wanted
            .iter()
            .all(|e| v.files.iter().any(|f| f.episode == Some(*e)))
    });
    Some(complete.unwrap_or(0))
}

/// True when the channel already holds this item at the variant's quality.
/// `uploaded` is the list of qualities (such as `"2160p DV"`) already in the library.
pub fn skip_uploaded<S: AsRef<str>>(variant: &Variant, uploaded: &[S]) -> bool {
    let quality = variant.quality();
    uploaded.iter().any(|q| q.as_ref() == quality)
}

/// Finds the title page: poster match, else normalised title + year.
pub async fn find(http: &reqwest::Client, title: &TitleDetails) -> Result<Option<Url>> {
    find_at(http, &Url::parse(BASE_URL)?, title).await
}

/// [`find`] against another site root (tests point this at a mock server).
pub async fn find_at(
    http: &reqwest::Client,
    base: &Url,
    title: &TitleDetails,
) -> Result<Option<Url>> {
    let mut search = base.clone();
    search
        .query_pairs_mut()
        .append_pair("s", &title.summary.title);
    let html = fetch(http, &search, None).await?;
    Ok(parse_search(&html, base, title))
}

/// Parses the variants on a title page, sorted as on the Mac.
pub async fn variants(
    http: &reqwest::Client,
    page: &Url,
    media: MediaType,
) -> Result<Vec<Variant>> {
    let html = fetch(http, page, None).await?;
    Ok(parse_variants(&html, media))
}

/// Follows a HubCloud link (or the redirector wrapping it) to a direct download URL.
pub async fn resolve(http: &reqwest::Client, link: &str) -> Result<String> {
    hubcloud::resolve(http, link).await
}

/// GETs a page with the Chrome user agent and an HTML Accept header; anything but 200 fails.
pub async fn fetch(http: &reqwest::Client, url: &Url, referer: Option<&str>) -> Result<String> {
    let mut req = http
        .get(url.clone())
        .header(USER_AGENT_HEADER, USER_AGENT)
        .header(ACCEPT, "text/html,application/xhtml+xml");
    if let Some(referer) = referer {
        req = req.header(REFERER, referer);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if status != StatusCode::OK {
        return Err(Error::Other(format!(
            "http {} from {}",
            status.as_u16(),
            url.host_str().unwrap_or("")
        )));
    }
    let body = resp.bytes().await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Capture group 1 of the first match.
fn first<'a>(re: Option<&Regex>, s: &'a str) -> Option<&'a str> {
    re?.captures(s)?.get(1).map(|m| m.as_str())
}

/// Decodes the three entities the site uses in titles and file names.
fn decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&#039;", "'")
        .replace("&quot;", "\"")
}

/// Reads a size badge such as `1.53 GB` or `700 MB` into bytes (binary units), 0 if absent.
pub fn parse_size(s: &str) -> u64 {
    let Some(c) = re!(r"(?i)([\d.]+)\s*(GB|MB)").and_then(|r| r.captures(s)) else {
        return 0;
    };
    let (Some(value), Some(unit)) = (c.get(1), c.get(2)) else {
        return 0;
    };
    let Ok(value) = value.as_str().parse::<f64>() else {
        return 0;
    };
    let scale = if unit.as_str().eq_ignore_ascii_case("MB") {
        1024.0 * 1024.0
    } else {
        1024.0 * 1024.0 * 1024.0
    };
    (value * scale).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "https://hubcloud.ist/drive/x";

    fn file(episode: u32) -> HubFile {
        HubFile {
            episode: Some(episode),
            name: format!("E{episode}.mkv"),
            size_bytes: 1,
            link: HOST.to_owned(),
        }
    }

    fn variant(label: &str, episodes: &[u32]) -> Variant {
        Variant::new(
            label.to_owned(),
            Some(1),
            episodes.iter().map(|e| file(*e)).collect(),
        )
    }

    #[test]
    fn label_flags() {
        let v = variant("S04 DV HDR 2160p WEB-DL H265", &[1]);
        assert_eq!((v.height, v.dv, v.hdr, v.remux), (2160, true, true, false));
        assert_eq!(v.tag(), Tag::Dv);
        assert_eq!(v.quality(), "2160p DV");

        let v = variant("Movie (2160p UHD BluRay REMUX HDR10 HEVC)", &[]);
        assert_eq!((v.height, v.dv, v.hdr, v.remux), (2160, false, true, true));
        assert_eq!(v.quality(), "2160p HDR");

        let v = variant("S05 SDR 2160p WEB-DL H265", &[]);
        assert_eq!(v.tag(), Tag::Sdr);
        assert_eq!(v.quality(), "2160p");

        let v = variant("Something DoVi", &[]);
        assert_eq!(v.quality(), "DV");
        assert_eq!(variant("No quality", &[]).quality(), "");
        assert!(!variant("DVDRip 480p", &[]).dv);
    }

    #[test]
    fn sort_order() {
        let mut v = vec![
            variant("1080p WEB-DL", &[]),
            variant("2160p DV REMUX", &[]),
            variant("2160p REMUX", &[]),
            variant("2160p DV WEB-DL", &[]),
            variant("2160p WEB-DL", &[]),
            variant("720p", &[]),
        ];
        sort_variants(&mut v);
        let labels: Vec<_> = v.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "2160p WEB-DL",
                "2160p REMUX",
                "2160p DV WEB-DL",
                "2160p DV REMUX",
                "1080p WEB-DL",
                "720p"
            ]
        );
    }

    #[test]
    fn default_pick() {
        let v = vec![
            variant("A", &[1, 2]),
            variant("B", &[1, 2, 3]),
            variant("C", &[1, 2, 3, 4]),
        ];
        assert_eq!(default_variant(&v, &[1, 3]), Some(1));
        assert_eq!(default_variant(&v, &[4]), Some(2));
        assert_eq!(default_variant(&v, &[9]), Some(0));
        assert_eq!(default_variant(&v, &[]), Some(0));
        assert_eq!(default_variant(&[], &[1]), None);
    }

    #[test]
    fn skip_compares_quality() {
        let v = variant("S01 HDR 2160p WEB-DL", &[1]);
        assert!(skip_uploaded(&v, &["1080p", "2160p HDR"]));
        assert!(!skip_uploaded(&v, &["2160p", "2160p DV"]));
        assert!(!skip_uploaded::<&str>(&v, &[]));
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1.5 GB"), 1_610_612_736);
        assert_eq!(parse_size("512 mb"), 536_870_912);
        assert_eq!(parse_size("40.16GB"), 43_121_471_652);
        assert_eq!(parse_size("Episodes 8"), 0);
        assert_eq!(parse_size(""), 0);
    }

    #[test]
    fn entities() {
        assert_eq!(
            decode("Tom &amp; Jerry&#039;s &quot;Show&quot;"),
            "Tom & Jerry's \"Show\""
        );
    }
}
