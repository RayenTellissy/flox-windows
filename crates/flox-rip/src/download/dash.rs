//! MPD parsing and segment download, as on the Mac. Filled in by piece P8.

use std::path::{Path, PathBuf};

use flox_core::error::{Error, Result};
use tokio_util::sync::CancellationToken;
use url::Url;

/// A parsed manifest (tallest video and first audio). Fields are defined by P8.
#[derive(Clone, Debug, PartialEq)]
pub struct Mpd {
    _private: (),
}

/// String-level MPD parse, resolving segment URLs against `base`.
pub fn parse(_xml: &str, _base: &Url) -> Result<Mpd> {
    Err(Error::NotImplemented("flox_rip::download::dash::parse"))
}

/// Downloads into `dir`, returning (video, audio) track files. `progress` is 0.0..=1.0.
pub async fn download(
    _mpd: &Mpd,
    _headers: &[(String, String)],
    _dir: &Path,
    _progress: impl Fn(f32) + Send,
    _cancel: CancellationToken,
) -> Result<(PathBuf, Option<PathBuf>)> {
    Err(Error::NotImplemented("flox_rip::download::dash::download"))
}
