//! 4KHDHub scraping and HubCloud resolution. Filled in by piece P9.

mod fourk;
mod hubcloud;
mod unwrap;

use flox_core::error::{Error, Result};
use flox_core::model::{MediaType, TitleDetails};
use url::Url;

/// Site root.
pub const BASE_URL: &str = "https://4khdhub.one";

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

/// Finds the title page: poster match, else normalised title + year.
pub async fn find(_http: &reqwest::Client, _title: &TitleDetails) -> Result<Option<Url>> {
    Err(Error::NotImplemented("flox_rip::hub::find"))
}

/// Parses the variants on a title page, sorted as on the Mac.
pub async fn variants(
    _http: &reqwest::Client,
    _page: &Url,
    _media: MediaType,
) -> Result<Vec<Variant>> {
    Err(Error::NotImplemented("flox_rip::hub::variants"))
}

/// Follows a HubCloud link to a direct download URL.
pub async fn resolve(_http: &reqwest::Client, _link: &str) -> Result<String> {
    Err(Error::NotImplemented("flox_rip::hub::resolve"))
}

/// Decodes the redirect page chain (base64, base64, rot13, base64, JSON `o`, base64).
/// Filled in by P9.
#[allow(clippy::unimplemented)]
pub fn unwrap_redirect(_html: &str) -> Option<String> {
    unimplemented!("flox_rip::hub::unwrap_redirect (P9)")
}
