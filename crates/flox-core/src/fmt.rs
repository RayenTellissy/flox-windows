//! Display formatting shared by the screens. Filled in by piece P2.

use crate::model::MediaType;

/// `h:mm:ss` or `m:ss`. Filled in by P2.
#[allow(clippy::unimplemented)]
pub fn clock(_secs: u32) -> String {
    unimplemented!("flox_core::fmt::clock (P2)")
}

/// Human-readable size such as `1.4 GB`. Filled in by P2.
#[allow(clippy::unimplemented)]
pub fn bytes(_n: u64) -> String {
    unimplemented!("flox_core::fmt::bytes (P2)")
}

/// The details meta line: `YEAR · TV|MOVIE · N MIN [· LIBRARY · 1080P, 2160P DV]`.
/// Missing parts are skipped; the whole line is uppercase. Filled in by P2.
#[allow(clippy::unimplemented)]
pub fn meta_line(
    _year: Option<u16>,
    _media: MediaType,
    _runtime_min: Option<u32>,
    _library_qualities: &[String],
) -> String {
    unimplemented!("flox_core::fmt::meta_line (P2)")
}
