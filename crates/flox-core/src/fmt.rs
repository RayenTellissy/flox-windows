//! Display formatting shared by the screens.

use crate::model::MediaType;

/// `h:mm:ss` when an hour or longer, else `m:ss`.
pub fn clock(secs: u32) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Human-readable size in decimal units, like the Mac's `ByteCountFormatter(.file)`:
/// `512 B`, `12 KB`, `1.4 MB`, `1.4 GB`, `2.1 TB`. Kilobytes are whole numbers; larger
/// units carry one decimal.
pub fn bytes(n: u64) -> String {
    const UNITS: &[&str] = &["KB", "MB", "GB", "TB", "PB", "EB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    let name = UNITS.get(unit).copied().unwrap_or("EB");
    if unit == 0 {
        let whole = value.round();
        if whole >= 1000.0 {
            return format!("1.0 {}", UNITS.get(1).copied().unwrap_or("MB"));
        }
        format!("{whole:.0} {name}")
    } else {
        format!("{value:.1} {name}")
    }
}

/// The details meta line: `YEAR · TV|MOVIE · N MIN [· LIBRARY · 1080P, 2160P DV]`.
/// Missing parts are skipped; the whole line is uppercase.
pub fn meta_line(
    year: Option<u16>,
    media: MediaType,
    runtime_min: Option<u32>,
    library_qualities: &[String],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(y) = year {
        parts.push(y.to_string());
    }
    parts.push(
        match media {
            MediaType::Tv => "TV",
            MediaType::Movie => "MOVIE",
        }
        .to_owned(),
    );
    if let Some(m) = runtime_min {
        parts.push(format!("{m} MIN"));
    }
    let qualities: Vec<&str> = library_qualities
        .iter()
        .map(|q| q.trim())
        .filter(|q| !q.is_empty())
        .collect();
    if !qualities.is_empty() {
        parts.push(format!("LIBRARY · {}", qualities.join(", ")));
    }
    parts.join(" · ").to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(5), "0:05");
        assert_eq!(clock(59 * 60 + 59), "59:59");
        assert_eq!(clock(3600), "1:00:00");
        assert_eq!(clock(2 * 3600 + 3 * 60 + 4), "2:03:04");
    }

    #[test]
    fn bytes_formats() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1000), "1 KB");
        assert_eq!(bytes(12_345), "12 KB");
        assert_eq!(bytes(999_600), "1.0 MB");
        assert_eq!(bytes(1_400_000), "1.4 MB");
        assert_eq!(bytes(1_440_000_000), "1.4 GB");
        assert_eq!(bytes(999_990_000), "1.0 GB");
        assert_eq!(bytes(2_100_000_000_000), "2.1 TB");
        assert_eq!(bytes(u64::MAX), "18.4 EB");
    }

    #[test]
    fn meta_line_formats() {
        assert_eq!(
            meta_line(
                Some(2021),
                MediaType::Movie,
                Some(148),
                &["1080p".to_owned(), "2160p DV".to_owned()]
            ),
            "2021 · MOVIE · 148 MIN · LIBRARY · 1080P, 2160P DV"
        );
        assert_eq!(meta_line(None, MediaType::Tv, None, &[]), "TV");
        assert_eq!(
            meta_line(Some(2008), MediaType::Tv, Some(47), &[" ".to_owned()]),
            "2008 · TV · 47 MIN"
        );
    }
}
