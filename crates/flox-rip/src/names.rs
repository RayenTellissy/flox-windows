//! File-name heuristics for local files, with the Mac regexes. Filled in by piece P7.

use std::cmp::Ordering;

use crate::job::Tag;

/// `S01E05`, `1x05`, `E05`/`Ep 5`/`Episode 5`, in that order. Filled in by P7.
#[allow(clippy::unimplemented)]
pub fn episode_from_name(_name: &str) -> Option<u32> {
    unimplemented!("flox_rip::names::episode_from_name (P7)")
}

/// DV (`dv`, `dovi`, `dolby vision`), else HDR, else SDR. Filled in by P7.
#[allow(clippy::unimplemented)]
pub fn tag_from_name(_name: &str) -> Tag {
    unimplemented!("flox_rip::names::tag_from_name (P7)")
}

/// Finder-style natural order. Filled in by P7.
#[allow(clippy::unimplemented)]
pub fn natural_cmp(_a: &str, _b: &str) -> Ordering {
    unimplemented!("flox_rip::names::natural_cmp (P7)")
}
