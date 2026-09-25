//! HubCloud drive page resolution.
//!
//! A drive page links to a `hubcloud.php` hop; that page, fetched with the drive URL
//! as Referer, lists the servers. The first Cloudflare R2 or FSL link wins, then
//! pixeldrain (through its API), then a `workers.dev` mirror.

use flox_core::error::{Error, Result};
use url::Url;

use super::{fetch, first, unwrap_redirect};

/// Resolves a 4KHDHub download link: unwraps the redirector unless the host is already
/// HubCloud, then walks drive page → `hubcloud.php` → direct server.
pub(super) async fn resolve(http: &reqwest::Client, link: &str) -> Result<String> {
    let link = Url::parse(link)?;
    let drive = if link.host_str().is_some_and(|h| h.contains("hubcloud")) {
        link
    } else {
        let html = fetch(http, &link, None).await?;
        let target = unwrap_redirect(&html)
            .ok_or_else(|| Error::Other("Could not read the 4KHDHub redirect".to_owned()))?;
        Url::parse(&target)?
    };
    let page = fetch(http, &drive, None).await?;
    let hop = parse_hop(&page)
        .ok_or_else(|| Error::Other("HubCloud page has no download link".to_owned()))?;
    let servers = fetch(http, &Url::parse(&hop)?, Some(drive.as_str())).await?;
    parse_direct(&servers)
        .ok_or_else(|| Error::Other("HubCloud offered no direct server".to_owned()))
}

/// The `…/hubcloud.php?…` URL on a drive page.
pub fn parse_hop(html: &str) -> Option<String> {
    let found = re!(r#"https?://[^"'\s]+hubcloud\.php\?[^"'\s]+"#)?.find(html)?;
    Url::parse(found.as_str()).ok().map(String::from)
}

/// The direct file URL on a `hubcloud.php` page, normalised (spaces percent-encoded).
pub fn parse_direct(html: &str) -> Option<String> {
    let links: Vec<String> = re!(r#"href="(https?://[^"]+)""#)
        .map(|r| {
            r.captures_iter(html)
                .filter_map(|c| c.get(1))
                .map(|m| m.as_str().replace("&amp;", "&"))
                .collect()
        })
        .unwrap_or_default();
    let parsed = |s: &String| Url::parse(s).ok().map(String::from);
    if let Some(r2) = links
        .iter()
        .find(|l| l.contains("cloudflarestorage.com") || l.contains("fsl"))
        .and_then(parsed)
    {
        return Some(r2);
    }
    if let Some(id) = first(re!(r"pixeldrain\.\w+/u/(\w+)"), html) {
        return Some(format!("https://pixeldrain.dev/api/file/{id}"));
    }
    links
        .iter()
        .find(|l| l.contains("workers.dev"))
        .and_then(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_priority() {
        let all = r#"<a href="https://a.workers.dev/f.mkv">w</a>
            <a href="https://pixeldrain.dev/u/AbC123">p</a>
            <a href="https://fsl.example.net/f.mkv?x=1&amp;y=2">f</a>
            <a href="https://b.r2.cloudflarestorage.com/f.mkv">r</a>"#;
        assert_eq!(
            parse_direct(all).as_deref(),
            Some("https://fsl.example.net/f.mkv?x=1&y=2")
        );
        let pixel = r#"<a href="https://a.workers.dev/f.mkv">w</a>
            <a href="https://pixeldrain.com/u/AbC123">p</a>"#;
        assert_eq!(
            parse_direct(pixel).as_deref(),
            Some("https://pixeldrain.dev/api/file/AbC123")
        );
        let workers =
            r#"<a href="https://t.me/x">t</a><a href="https://a.workers.dev/My File.mkv">w</a>"#;
        assert_eq!(
            parse_direct(workers).as_deref(),
            Some("https://a.workers.dev/My%20File.mkv")
        );
        assert_eq!(parse_direct(r#"<a href="https://t.me/x">t</a>"#), None);
    }

    #[test]
    fn hop_link() {
        let html =
            r#"var url = 'https://gamerxyt.com/hubcloud.php?host=hubcloud&id=abc&token=x=';"#;
        assert_eq!(
            parse_hop(html).as_deref(),
            Some("https://gamerxyt.com/hubcloud.php?host=hubcloud&id=abc&token=x=")
        );
        assert_eq!(parse_hop("<a href=\"https://a.test/\">"), None);
    }
}
