//! The loudness boost audio filter. Filled in by piece P11.

/// `lavfi=[volume=<gain>dB,acompressor=threshold=0.891:ratio=10:attack=1:release=60,alimiter=limit=0.98]`
/// when `boost` is on and `gain_db > 0`, else `None`. Filled in by P11.
#[allow(clippy::unimplemented)]
pub fn loudness_af(_boost: bool, _gain_db: f32) -> Option<String> {
    unimplemented!("flox_player::filters::loudness_af (P11)")
}
