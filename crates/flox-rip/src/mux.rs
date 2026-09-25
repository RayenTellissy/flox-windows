//! ffmpeg argument lists and `-stats` parsing, exactly as on the Mac. Filled in by piece P7.

use std::ffi::OsString;
use std::path::Path;

/// HLS remux: `-user_agent`, `-headers` (with the VidLink Referer), `-c copy -bsf:a aac_adtstoasc
/// -movflags +faststart`. Filled in by P7.
#[allow(clippy::unimplemented)]
pub fn hls_args(_input: &str, _headers: &[(String, String)], _out: &Path) -> Vec<OsString> {
    unimplemented!("flox_rip::mux::hls_args (P7)")
}

/// DASH mux of the downloaded tracks: `-c copy -tag:v hvc1 -movflags +faststart`. Filled in by P7.
#[allow(clippy::unimplemented)]
pub fn dash_args(_video: &Path, _audio: Option<&Path>, _out: &Path) -> Vec<OsString> {
    unimplemented!("flox_rip::mux::dash_args (P7)")
}

/// The job detail string for a `frame=… time=… speed=…` line. Filled in by P7.
#[allow(clippy::unimplemented)]
pub fn parse_stats(_line: &str) -> Option<String> {
    unimplemented!("flox_rip::mux::parse_stats (P7)")
}
