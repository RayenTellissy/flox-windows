//! Language names for the settings choices, caption languages and audio labels.

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

/// Every language Flox can name: ISO 639-1, ISO 639-2/B, ISO 639-2/T (or `""` when the
/// same as /B), and the English name. The 13 selectable languages come first, then other
/// languages that commonly appear in captions and audio tracks.
const LANGUAGES: &[(&str, &str, &str, &str)] = &[
    ("en", "eng", "", "English"),
    ("es", "spa", "", "Spanish"),
    ("fr", "fre", "fra", "French"),
    ("de", "ger", "deu", "German"),
    ("it", "ita", "", "Italian"),
    ("pt", "por", "", "Portuguese"),
    ("ru", "rus", "", "Russian"),
    ("ar", "ara", "", "Arabic"),
    ("hi", "hin", "", "Hindi"),
    ("ja", "jpn", "", "Japanese"),
    ("ko", "kor", "", "Korean"),
    ("zh", "chi", "zho", "Chinese"),
    ("tr", "tur", "", "Turkish"),
    ("bg", "bul", "", "Bulgarian"),
    ("bn", "ben", "", "Bengali"),
    ("ca", "cat", "", "Catalan"),
    ("cs", "cze", "ces", "Czech"),
    ("da", "dan", "", "Danish"),
    ("el", "gre", "ell", "Greek"),
    ("et", "est", "", "Estonian"),
    ("fa", "per", "fas", "Persian"),
    ("fi", "fin", "", "Finnish"),
    ("he", "heb", "", "Hebrew"),
    ("hr", "hrv", "", "Croatian"),
    ("hu", "hun", "", "Hungarian"),
    ("id", "ind", "", "Indonesian"),
    ("is", "ice", "isl", "Icelandic"),
    ("lt", "lit", "", "Lithuanian"),
    ("lv", "lav", "", "Latvian"),
    ("mk", "mac", "mkd", "Macedonian"),
    ("ml", "mal", "", "Malayalam"),
    ("ms", "may", "msa", "Malay"),
    ("nl", "dut", "nld", "Dutch"),
    ("no", "nor", "", "Norwegian"),
    ("nb", "nob", "", "Norwegian Bokmål"),
    ("pl", "pol", "", "Polish"),
    ("ro", "rum", "ron", "Romanian"),
    ("sk", "slo", "slk", "Slovak"),
    ("sl", "slv", "", "Slovenian"),
    ("sr", "srp", "", "Serbian"),
    ("sv", "swe", "", "Swedish"),
    ("ta", "tam", "", "Tamil"),
    ("te", "tel", "", "Telugu"),
    ("th", "tha", "", "Thai"),
    ("tl", "tgl", "", "Tagalog"),
    ("uk", "ukr", "", "Ukrainian"),
    ("ur", "urd", "", "Urdu"),
    ("vi", "vie", "", "Vietnamese"),
    ("eu", "baq", "eus", "Basque"),
    ("gl", "glg", "", "Galician"),
];

/// Other English names seen in caption lists, mapped to their ISO 639-1 code.
const ALIASES: &[(&str, &str)] = &[
    ("mandarin", "zh"),
    ("cantonese", "zh"),
    ("castilian", "es"),
    ("farsi", "fa"),
    ("filipino", "tl"),
    ("flemish", "nl"),
    ("brazilian", "pt"),
    ("brazilian portuguese", "pt"),
    ("latin american spanish", "es"),
    ("simplified chinese", "zh"),
    ("traditional chinese", "zh"),
    ("bokmal", "nb"),
    ("norwegian bokmal", "nb"),
    ("slovene", "sl"),
];

/// `"English"` → `"en"` (case-insensitive; tolerant of qualifiers like `"English (US)"`,
/// `"Spanish - Latin America"`, `"English [CC]"` or `"Portuguese, Brazil"`).
pub fn iso_from_english_name(name: &str) -> Option<&'static str> {
    let lower = name.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let lookup = |n: &str| -> Option<&'static str> {
        LANGUAGES
            .iter()
            .find(|(_, _, _, english)| english.to_lowercase() == n)
            .map(|(iso, _, _, _)| *iso)
            .or_else(|| {
                ALIASES
                    .iter()
                    .find(|(alias, _)| *alias == n)
                    .map(|(_, iso)| *iso)
            })
    };
    if let Some(iso) = lookup(&lower) {
        return Some(iso);
    }
    let head = lower
        .split(['(', '[', ',', ';', '/', '-', '|'])
        .next()
        .unwrap_or("")
        .trim();
    if let Some(iso) = lookup(head) {
        return Some(iso);
    }
    head.split_whitespace().find_map(lookup)
}

/// `"en"` (or `"eng"`, `"EN"`, `"en-US"`, `"pt_BR"`) → `"English"`.
pub fn display_name(iso: &str) -> Option<&'static str> {
    let code = iso
        .trim()
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if code.is_empty() {
        return None;
    }
    LANGUAGES
        .iter()
        .find(|(one, b, t, _)| *one == code || *b == code || (!t.is_empty() && *t == code))
        .map(|(_, _, _, english)| *english)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_settings_languages() {
        let codes: Vec<&str> = super::LANGUAGE_NAMES.iter().map(|(c, _)| *c).collect();
        assert_eq!(codes, crate::settings::LANGUAGES);
    }

    #[test]
    fn full_table_starts_with_the_selectable_ones() {
        for ((code, name), (one, _, _, english)) in LANGUAGE_NAMES.iter().zip(LANGUAGES) {
            assert_eq!((code, name), (one, english));
        }
    }

    #[test]
    fn every_selectable_language_round_trips() {
        for (code, name) in LANGUAGE_NAMES {
            assert_eq!(display_name(code), Some(*name));
            assert_eq!(iso_from_english_name(name), Some(*code));
        }
    }

    #[test]
    fn english_names_with_qualifiers() {
        assert_eq!(iso_from_english_name("english"), Some("en"));
        assert_eq!(iso_from_english_name("  SPANISH "), Some("es"));
        assert_eq!(iso_from_english_name("English (US)"), Some("en"));
        assert_eq!(iso_from_english_name("Spanish - Latin America"), Some("es"));
        assert_eq!(iso_from_english_name("Portuguese, Brazil"), Some("pt"));
        assert_eq!(iso_from_english_name("English [CC]"), Some("en"));
        assert_eq!(iso_from_english_name("Brazilian Portuguese"), Some("pt"));
        assert_eq!(iso_from_english_name("Chinese (Simplified)"), Some("zh"));
        assert_eq!(iso_from_english_name("Mandarin"), Some("zh"));
        assert_eq!(iso_from_english_name("Dutch"), Some("nl"));
        assert_eq!(iso_from_english_name("Basque"), Some("eu"));
        assert_eq!(iso_from_english_name("Galician"), Some("gl"));
        assert_eq!(iso_from_english_name("Chinese - Simplified"), Some("zh"));
        assert_eq!(iso_from_english_name("Norwegian Bokmal"), Some("nb"));
        assert_eq!(iso_from_english_name("English SDH"), Some("en"));
        assert_eq!(iso_from_english_name("Klingon"), None);
        assert_eq!(iso_from_english_name(""), None);
    }

    #[test]
    fn display_names_from_codes() {
        assert_eq!(display_name("eng"), Some("English"));
        assert_eq!(display_name("EN"), Some("English"));
        assert_eq!(display_name("en-US"), Some("English"));
        assert_eq!(display_name("pt_BR"), Some("Portuguese"));
        assert_eq!(display_name("fre"), Some("French"));
        assert_eq!(display_name("fra"), Some("French"));
        assert_eq!(display_name("zho"), Some("Chinese"));
        assert_eq!(display_name("nld"), Some("Dutch"));
        assert_eq!(display_name("eus"), Some("Basque"));
        assert_eq!(display_name("und"), None);
        assert_eq!(display_name(""), None);
    }
}
