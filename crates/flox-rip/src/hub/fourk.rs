//! 4KHDHub search and title-page parsing.

use flox_core::model::{MediaType, TitleDetails};
use url::Url;

use super::{decode, first, parse_size, sort_variants, HubFile, Variant};

/// Picks the title's page from a search results page: a card of the right kind whose
/// TMDB poster path matches wins at once, else the first whose normalised title and
/// year match. Relative card links are resolved against `base`.
pub fn parse_search(html: &str, base: &Url, title: &TitleDetails) -> Option<Url> {
    let kind = match title.summary.media {
        MediaType::Tv => "-series-",
        MediaType::Movie => "-movie-",
    };
    let cards = re!(r#"(?s)<a href="([^"]+)" class="movie-card".*?</a>"#)?;
    let want_title = normalize(&title.summary.title);
    let want_year = title
        .summary
        .year
        .map(|y| y.to_string())
        .unwrap_or_default();
    let mine = title.summary.poster_path.as_deref();
    let mut by_title = None;
    for card in cards.captures_iter(html) {
        let (Some(block), Some(href)) = (card.get(0), card.get(1)) else {
            continue;
        };
        let href = href.as_str();
        if !href.contains(kind) {
            continue;
        }
        let Ok(url) = base.join(href) else {
            continue;
        };
        let block = block.as_str();
        let poster = first(re!(r#"image\.tmdb\.org/t/p/\w+(/[^"']+)"#), block);
        if poster.is_some() && poster == mine {
            return Some(url);
        }
        let card_title = first(re!(r#"movie-card-title">([^<]*)<"#), block).unwrap_or("");
        let card_year = first(re!(r#"movie-card-meta">\s*(\d{4})"#), block).unwrap_or("");
        if by_title.is_none()
            && normalize(&decode(card_title)) == want_title
            && card_year == want_year
        {
            by_title = Some(url);
        }
    }
    by_title
}

/// Lists every print on a title page, best first. Series pages are grouped by season
/// (one variant per season and print); movie pages give one single-file variant per
/// download, skipping `.zip` packs.
pub fn parse_variants(html: &str, media: MediaType) -> Vec<Variant> {
    let mut out = match media {
        MediaType::Tv => tv_variants(html),
        MediaType::Movie => movie_variants(html),
    };
    sort_variants(&mut out);
    out
}

fn tv_variants(html: &str) -> Vec<Variant> {
    let mut out = Vec::new();
    for group in html
        .split(r#"<div class="season-item episode-item"#)
        .skip(1)
    {
        let season: u32 = first(re!(r#"class="episode-number">\s*S(\d+)"#), group)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let label = decode(first(re!(r#"class="episode-title">([^<]*)<"#), group).unwrap_or(""))
            .trim()
            .to_owned();
        let mut files = Vec::new();
        for item in group
            .split(r#"<div class="episode-download-item">"#)
            .skip(1)
        {
            let Some(episode) =
                first(re!(r"Episode-(\d+)"), item).and_then(|e| e.parse::<u32>().ok())
            else {
                continue;
            };
            let Some(link) = hub_link(item) else {
                continue;
            };
            let name = decode(
                first(re!(r#"class="episode-file-title">\s*([^<]*?)\s*<"#), item).unwrap_or(""),
            );
            let size = first(re!(r#"class="badge-size">([^<]*)<"#), item).unwrap_or("");
            files.push(HubFile {
                episode: Some(episode),
                name,
                size_bytes: parse_size(size),
                link,
            });
        }
        if season == 0 || files.is_empty() {
            continue;
        }
        files.sort_by_key(|f| f.episode);
        out.push(Variant::new(label, Some(season), files));
    }
    out
}

fn movie_variants(html: &str) -> Vec<Variant> {
    let mut out = Vec::new();
    for item in html.split(r#"<div class="download-item "#).skip(1) {
        let Some(link) = hub_link(item) else {
            continue;
        };
        let name = decode(first(re!(r#"class="file-title">\s*([^<]*?)\s*<"#), item).unwrap_or(""));
        if name.to_lowercase().ends_with(".zip") {
            continue;
        }
        let header = decode(first(re!(r#"font-semibold">\s*([^<]*?)\s*<"#), item).unwrap_or(""));
        let size = first(re!(r#"#ea580c; color: white;">([^<]*)<"#), item).unwrap_or("");
        let label = if header.is_empty() {
            name.clone()
        } else {
            header
        };
        let file = HubFile {
            episode: None,
            name,
            size_bytes: parse_size(size),
            link,
        };
        out.push(Variant::new(label, None, vec![file]));
    }
    out
}

/// The "Download HubCloud" button's target: a HubCloud drive URL, or the redirector the
/// site wraps it in. Falls back to any link mentioning hubcloud.
fn hub_link(s: &str) -> Option<String> {
    let link = first(
        re!(r#"href="(https?://[^"]+)"[^>]*>\s*(?:<span[^>]*>)?\s*Download HubCloud"#),
        s,
    )
    .or_else(|| first(re!(r#"href="(https?://[^"]*hubcloud[^"]*)""#), s))?
    .replace("&amp;", "&");
    Url::parse(&link).ok()?;
    Some(link)
}

/// Lowercase, `&` as `and`, letters and digits only.
fn normalize(s: &str) -> String {
    s.to_lowercase()
        .replace('&', "and")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises() {
        assert_eq!(normalize("Fast & Furious: Hobbs"), "fastandfurioushobbs");
        assert_eq!(normalize("Amélie"), "amélie");
    }

    #[test]
    fn hub_link_prefers_button() {
        let s = r#"<a href="https://x.test/hubcloud/other">x</a>
            <a target="_blank" href="https://r.test/?id=a&amp;b=1" class="btn">
            <span style="">Download HubCloud&nbsp;</span></a>"#;
        assert_eq!(hub_link(s).as_deref(), Some("https://r.test/?id=a&b=1"));
        let s = r#"<a href="https://hubcloud.ist/drive/abc">Mirror</a>"#;
        assert_eq!(
            hub_link(s).as_deref(),
            Some("https://hubcloud.ist/drive/abc")
        );
        assert_eq!(hub_link(r#"<a href="https://a.test/">HubDrive</a>"#), None);
    }
}
