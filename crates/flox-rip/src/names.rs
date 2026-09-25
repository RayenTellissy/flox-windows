//! File-name heuristics for local files, with the Mac regexes (`LocalFilesView.swift`).

use std::cmp::Ordering;
use std::iter::Peekable;
use std::str::Chars;
use std::sync::LazyLock;

use regex::Regex;

use crate::job::Tag;

/// Episode patterns, tried in order: `S01E05`, `1x05`, `E05`/`Ep 5`/`Episode 5`.
const EPISODE_PATTERNS: [&str; 3] = [
    r"(?i)s\d{1,2}[ ._-]?e(\d{1,3})",
    r"(?i)(?:^|[^a-z0-9])\d{1,2}x(\d{2,3})(?:[^a-z0-9]|$)",
    r"(?i)(?:^|[^a-z0-9])e(?:p|pisode)?[ ._-]?(\d{1,3})(?:[^a-z0-9]|$)",
];

/// Dolby Vision marker.
const DV_PATTERN: &str = r"(?i)(?:^|[^a-z0-9])(dv|dovi|dolby[ ._-]?vision)(?:[^a-z0-9]|$)";

/// HDR marker (also matches `HDR10`, `HDR10+`).
const HDR_PATTERN: &str = r"(?i)(?:^|[^a-z0-9])hdr";

static EPISODE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    EPISODE_PATTERNS
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
});
static DV: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(DV_PATTERN).ok());
static HDR: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(HDR_PATTERN).ok());

/// `S01E05`, `1x05`, `E05`/`Ep 5`/`Episode 5`, in that order.
pub fn episode_from_name(name: &str) -> Option<u32> {
    EPISODE.iter().find_map(|re| {
        re.captures(name)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse().ok())
    })
}

/// DV (`dv`, `dovi`, `dolby vision`), else HDR, else SDR.
pub fn tag_from_name(name: &str) -> Tag {
    let hit = |re: &Option<Regex>| re.as_ref().is_some_and(|re| re.is_match(name));
    if hit(&DV) {
        Tag::Dv
    } else if hit(&HDR) {
        Tag::Hdr
    } else {
        Tag::Sdr
    }
}

/// Takes a run of ASCII digits.
fn digits(it: &mut Peekable<Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    s
}

/// Compares two digit runs by value, then fewer leading zeros first.
fn cmp_numbers(a: &str, b: &str) -> Ordering {
    let (ta, tb) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    ta.len()
        .cmp(&tb.len())
        .then_with(|| ta.cmp(tb))
        .then_with(|| a.len().cmp(&b.len()))
}

/// Finder-style natural order: digit runs compare as numbers, letters ignore case,
/// and exact ties fall back to plain string order so the order is total.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(cx), Some(cy)) if cx.is_ascii_digit() && cy.is_ascii_digit() => {
                let o = cmp_numbers(&digits(&mut x), &digits(&mut y));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(cx), Some(cy)) => {
                let o = cx.to_lowercase().cmp(cy.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_compile() {
        assert_eq!(EPISODE.len(), 3);
        assert!(DV.is_some());
        assert!(HDR.is_some());
    }

    #[test]
    fn episode_table() {
        let table: &[(&str, Option<u32>)] = &[
            ("Show.S01E05.1080p.WEB-DL.mkv", Some(5)),
            ("show.s1e7.mkv", Some(7)),
            ("Show S02 E10 720p.mp4", Some(10)),
            ("Show_S03_E011.mkv", Some(11)),
            ("Show-s04-e123.mkv", Some(123)),
            ("Show.S01.E02.mkv", Some(2)),
            ("The.Office.S09E23.Finale.mkv", Some(23)),
            ("Show 1x05.mkv", Some(5)),
            ("show.2x123.mp4", Some(123)),
            ("Show_10x01_Title.mkv", Some(1)),
            ("Show 1x5.mkv", None),
            ("1920x1080 sample.mkv", None),
            ("Show E05.mkv", Some(5)),
            ("Show - Ep 7.mp4", Some(7)),
            ("Show - Ep.08.mp4", Some(8)),
            ("Show Episode 12.mkv", Some(12)),
            ("show.episode_3.mkv", Some(3)),
            ("E01.mkv", Some(1)),
            ("[Group] Show - EP99 [1080p].mkv", Some(99)),
            ("Show.E1000.mkv", None),
            ("Movie.2019.1080p.BluRay.x264.mkv", None),
            ("Inception.mkv", None),
            ("Seven.mkv", None),
            ("Show.S01E05E06.mkv", Some(5)),
            ("show s01e05 1x09.mkv", Some(5)),
            ("Show 3x04 Ep 9.mkv", Some(4)),
            ("Some.Documentary.Part.2.mkv", None),
            ("Show E05v2.mkv", None),
            ("Season 1 Episode 6.mkv", Some(6)),
            ("", None),
        ];
        for (name, want) in table {
            assert_eq!(episode_from_name(name), *want, "{name}");
        }
    }

    #[test]
    fn tag_table() {
        let table: &[(&str, Tag)] = &[
            ("Show.S01E01.2160p.DV.HDR.mkv", Tag::Dv),
            ("Movie.2160p.DoVi.mkv", Tag::Dv),
            ("Movie 2160p Dolby Vision.mkv", Tag::Dv),
            ("Movie.2160p.dolby.vision.mkv", Tag::Dv),
            ("Movie.2160p.DolbyVision.mkv", Tag::Dv),
            ("Movie.2160p.dolby-vision.mkv", Tag::Dv),
            ("Movie-DV.mkv", Tag::Dv),
            ("DV.Movie.mkv", Tag::Dv),
            ("Movie.2160p.HDR.mkv", Tag::Hdr),
            ("Movie.2160p.HDR10.mkv", Tag::Hdr),
            ("Movie.2160p.HDR10+.mkv", Tag::Hdr),
            ("Movie hdr.mkv", Tag::Hdr),
            ("Movie.1080p.x264.mkv", Tag::Sdr),
            ("DVD.Rip.mkv", Tag::Sdr),
            ("Movie.DVDRip.mkv", Tag::Sdr),
            ("Advent.mkv", Tag::Sdr),
            ("Movie.PDHDR.mkv", Tag::Sdr),
            ("Movie.dvx.mkv", Tag::Sdr),
        ];
        for (name, want) in table {
            assert_eq!(tag_from_name(name), *want, "{name}");
        }
    }

    #[test]
    fn natural_order() {
        let mut names = vec![
            "Show E10.mkv",
            "show e2.mkv",
            "Show E1.mkv",
            "Show E02.mkv",
            "Show E100.mkv",
            "a.mkv",
            "B.mkv",
            "Show E9.mkv",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            vec![
                "a.mkv",
                "B.mkv",
                "Show E1.mkv",
                "show e2.mkv",
                "Show E02.mkv",
                "Show E9.mkv",
                "Show E10.mkv",
                "Show E100.mkv",
            ]
        );
        assert_eq!(natural_cmp("file", "file"), Ordering::Equal);
        assert_eq!(natural_cmp("file", "file2"), Ordering::Less);
        assert_eq!(natural_cmp("S01E09.mkv", "S01E10.mkv"), Ordering::Less);
        assert_eq!(
            natural_cmp("part 99999999999999999999", "part 100000000000000000000"),
            Ordering::Less
        );
    }
}
