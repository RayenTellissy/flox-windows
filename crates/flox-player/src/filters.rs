//! The loudness boost audio filter.
//!
//! Android's DynamicsProcessing input gain plus limiter (ratio 10, threshold
//! -1 dB, attack 1 ms, release 60 ms) maps to lavfi `volume` + `acompressor`
//! with the same parameters, followed by a hard safety limiter. Apply it with
//! `af set <value>`; clear it with `af set ""`.

use flox_core::settings::LOUDNESS_GAIN_RANGE;

/// `lavfi=[volume=<gain>dB,acompressor=threshold=0.891:ratio=10:attack=1:release=60,alimiter=limit=0.98]`
/// when `boost` is on and `gain_db > 0`, else `None`. The gain is capped at the
/// settings maximum (12 dB).
pub fn loudness_af(boost: bool, gain_db: f32) -> Option<String> {
    if !boost || !gain_db.is_finite() || gain_db <= 0.0 {
        return None;
    }
    let gain = gain_db.min(LOUDNESS_GAIN_RANGE.1);
    Some(format!(
        "lavfi=[volume={gain}dB,acompressor=threshold=0.891:ratio=10:attack=1:release=60,alimiter=limit=0.98]"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_with_gain() {
        assert_eq!(
            loudness_af(true, 8.0).as_deref(),
            Some("lavfi=[volume=8dB,acompressor=threshold=0.891:ratio=10:attack=1:release=60,alimiter=limit=0.98]")
        );
        assert_eq!(
            loudness_af(true, 2.5).as_deref(),
            Some("lavfi=[volume=2.5dB,acompressor=threshold=0.891:ratio=10:attack=1:release=60,alimiter=limit=0.98]")
        );
    }

    #[test]
    fn off_or_no_gain() {
        assert_eq!(loudness_af(false, 8.0), None);
        assert_eq!(loudness_af(true, 0.0), None);
        assert_eq!(loudness_af(true, -3.0), None);
        assert_eq!(loudness_af(true, f32::NAN), None);
    }

    #[test]
    fn gain_is_capped() {
        assert!(loudness_af(true, 40.0)
            .unwrap()
            .starts_with("lavfi=[volume=12dB,"));
    }
}
