//! Redirect-page decoding.
//!
//! The site's download buttons point at a redirector whose page stores the target in
//! `s('o','<blob>', ...)`. The blob is base64 of base64 of rot13 of base64 of a JSON
//! object whose `"o"` field is the base64 HubCloud URL.

use base64::alphabet;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use url::Url;

use super::first;

/// Standard alphabet, padding required (it is added first), stray trailing bits allowed.
const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
);

/// Decodes the redirect page chain (base64, base64, rot13, base64, JSON `o`, base64)
/// to the HubCloud URL it hides.
pub fn unwrap_redirect(html: &str) -> Option<String> {
    let blob = first(re!(r"s\('o',\s*'([^']+)'"), html)?;
    let once = base64(blob)?;
    let twice = base64(&once)?;
    let json = base64(&rot13(&twice))?;
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    let target = base64(value.get("o")?.as_str()?)?;
    Url::parse(&target).ok()?;
    Some(target)
}

/// Pads to a multiple of four and decodes, lossily, to text.
fn base64(s: &str) -> Option<String> {
    let pad = (4 - s.len() % 4) % 4;
    let padded = format!("{s}{}", "=".repeat(pad));
    let bytes = LENIENT.decode(padded).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Rotates ASCII letters by 13, leaving everything else alone.
fn rot13(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rot13_round_trips() {
        assert_eq!(rot13("Hello, World! 123"), "Uryyb, Jbeyq! 123");
        assert_eq!(rot13(&rot13("abcXYZ+/=")), "abcXYZ+/=");
    }

    #[test]
    fn pads_unpadded_base64() {
        assert_eq!(base64("aGk").as_deref(), Some("hi"));
        assert_eq!(base64("aGk=").as_deref(), Some("hi"));
        assert_eq!(base64("!!!!"), None);
    }

    #[test]
    fn missing_blob() {
        assert_eq!(unwrap_redirect("<html>nothing</html>"), None);
        assert_eq!(unwrap_redirect("s('o','bm90IGpzb24=',1)"), None);
    }
}
