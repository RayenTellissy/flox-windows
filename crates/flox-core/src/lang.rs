//! Language names for the settings choices, caption languages and audio labels.
//! Lookups are filled in by piece P2.

/// The 13 selectable languages: ISO 639-1 code and English name (Android order).
pub const LANGUAGE_NAMES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("ru", "Russian"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("tr", "Turkish"),
];

/// `"English"` → `"en"` (case-insensitive; tolerant of qualifiers like `"English (US)"`).
/// Filled in by P2.
#[allow(clippy::unimplemented)]
pub fn iso_from_english_name(_name: &str) -> Option<&'static str> {
    unimplemented!("flox_core::lang::iso_from_english_name (P2)")
}

/// `"en"` (or `"eng"`) → `"English"`. Filled in by P2.
#[allow(clippy::unimplemented)]
pub fn display_name(_iso: &str) -> Option<&'static str> {
    unimplemented!("flox_core::lang::display_name (P2)")
}

#[cfg(test)]
mod tests {
    #[test]
    fn table_matches_settings_languages() {
        let codes: Vec<&str> = super::LANGUAGE_NAMES.iter().map(|(c, _)| *c).collect();
        assert_eq!(codes, crate::settings::LANGUAGES);
    }
}
