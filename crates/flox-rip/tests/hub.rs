//! 4KHDHub and HubCloud parsing against pages saved from the live site, the redirect
//! round trip, a mock-server walk of the whole resolve chain, and ignored live tests.
//!
//! Refresh the fixtures with
//! `FLOX_HUB_REFRESH=1 cargo test -p flox-rip --test hub -- --ignored`.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use flox_core::model::{MediaType, TitleDetails, TitleSummary};
use flox_rip::hub::{
    self, default_variant, parse_direct, parse_hop, parse_search, parse_variants, unwrap_redirect,
};
use url::Url;
use wiremock::matchers::{header, header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SEARCH_TV: &str = include_str!("fixtures/hub/search_tv.html");
const SEARCH_MOVIE: &str = include_str!("fixtures/hub/search_movie.html");
const TV: &str = include_str!("fixtures/hub/tv.html");
const MOVIE: &str = include_str!("fixtures/hub/movie.html");
const REDIRECT: &str = include_str!("fixtures/hub/redirect.html");
const DRIVE: &str = include_str!("fixtures/hub/drive.html");
const HUBCLOUD_PHP: &str = include_str!("fixtures/hub/hubcloud_php.html");

const BEAR_POSTER: &str = "/eKfVzzEazSIjJMrw9ADa2x8ksLz.jpg";
const OPPENHEIMER_POSTER: &str = "/8Gxv8gSFCU0XGDykEGv7zR1n2ua.jpg";

fn title(media: MediaType, name: &str, year: Option<u16>, poster: Option<&str>) -> TitleDetails {
    TitleDetails {
        summary: TitleSummary {
            id: 1,
            media,
            title: name.to_owned(),
            year,
            poster_path: poster.map(str::to_owned),
            overview: String::new(),
            popularity: 0.0,
        },
        runtime_min: None,
        seasons: Vec::new(),
    }
}

fn base() -> Url {
    match Url::parse(hub::BASE_URL) {
        Ok(u) => u,
        Err(e) => panic!("{e}"),
    }
}

fn b64(s: &str) -> String {
    STANDARD.encode(s)
}

fn rot13(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            _ => c,
        })
        .collect()
}

/// The inverse of the site's chain: JSON with base64 `o`, base64, rot13, base64, base64.
fn redirect_page(target: &str) -> String {
    let json = serde_json::json!({ "w": 10, "l": "https://example.test/next/", "o": b64(target) });
    let blob = b64(&b64(&rot13(&b64(&json.to_string()))));
    // The site's blobs keep their padding; strip it here to exercise re-padding too.
    let blob = blob.trim_end_matches('=');
    format!("<script>function s(e,t,i){{}}s('o','{blob}',180*1000);</script>")
}

#[test]
fn search_matches_poster() {
    let t = title(MediaType::Tv, "The Bear", None, Some(BEAR_POSTER));
    assert_eq!(
        parse_search(SEARCH_TV, &base(), &t).unwrap().as_str(),
        "https://4khdhub.one/the-bear-series-2038/"
    );
    let m = title(
        MediaType::Movie,
        "Oppenheimer",
        None,
        Some(OPPENHEIMER_POSTER),
    );
    assert_eq!(
        parse_search(SEARCH_MOVIE, &base(), &m).unwrap().as_str(),
        "https://4khdhub.one/oppenheimer-movie-683/"
    );
}

#[test]
fn search_falls_back_to_title_and_year() {
    let t = title(MediaType::Tv, "the bear", Some(2022), Some("/other.jpg"));
    assert_eq!(
        parse_search(SEARCH_TV, &base(), &t).unwrap().as_str(),
        "https://4khdhub.one/the-bear-series-2038/"
    );
    let m = title(
        MediaType::Movie,
        "Bongee Bear & the Kingdom of Rhythm",
        Some(2019),
        None,
    );
    // "&" normalises to "and", so this matches the card titled with "and".
    assert_eq!(
        parse_search(SEARCH_TV, &base(), &m).unwrap().as_str(),
        "https://4khdhub.one/bongee-bear-and-the-kingdom-of-rhythm-movie-7143/"
    );
}

#[test]
fn search_rejects_wrong_year_or_kind() {
    let t = title(MediaType::Tv, "The Bear", Some(2021), None);
    assert!(parse_search(SEARCH_TV, &base(), &t).is_none());
    // Right poster, wrong kind: series cards are skipped when looking for a movie.
    let m = title(MediaType::Movie, "The Bear", Some(2022), Some(BEAR_POSTER));
    assert!(parse_search(SEARCH_TV, &base(), &m).is_none());
}

#[test]
fn tv_page_variants() {
    let v = parse_variants(TV, MediaType::Tv);
    assert_eq!(v.len(), 15);
    // Best first: every 2160p group, then 1080p, each in page order.
    let heights: Vec<u32> = v.iter().map(|v| v.height).collect();
    assert_eq!(&heights[..5], &[2160; 5]);
    assert!(heights[5..].iter().all(|h| *h == 1080));
    assert_eq!(v[0].label, "S05 SDR 2160p WEB-DL H265");
    assert_eq!(v[0].season, Some(5));
    assert_eq!(v[4].label, "S01 SDR 2160p WEB-DL H265");
    assert_eq!(v[5].label, "S05 AVC 1080p WEB-DL H264");

    let s5 = &v[0];
    assert!(!s5.dv && !s5.hdr && !s5.remux);
    assert_eq!(s5.quality(), "2160p");
    assert_eq!(s5.files.len(), 8);
    let episodes: Vec<Option<u32>> = s5.files.iter().map(|f| f.episode).collect();
    assert_eq!(episodes, (1..=8).map(Some).collect::<Vec<_>>());
    let f = &s5.files[0];
    assert_eq!(
        f.name,
        "The.Bear.S05E01.Soda.2160p.DSNP.WEB-DL.DDP5.1.H.265-4kHdHub.Com.mkv"
    );
    assert_eq!(f.size_bytes, hub::parse_size("1.53 GB"));
    assert!(f.link.starts_with("https://greenmotors.club/?id="));
    assert_eq!(
        s5.size_bytes,
        s5.files.iter().map(|f| f.size_bytes).sum::<u64>()
    );

    let season4: Vec<_> = v.iter().filter(|v| v.season == Some(4)).cloned().collect();
    assert_eq!(season4.len(), 3);
    assert_eq!(default_variant(&season4, &[1, 2]), Some(0));
}

#[test]
fn movie_page_variants() {
    let v = parse_variants(MOVIE, MediaType::Movie);
    let labels: Vec<&str> = v.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Oppenheimer (2160p BluRay HEVC HDR)",
            "Oppenheimer (2160p BluRay HEVC HDR)",
            "Oppenheimer (1080p BluRay HEVC HDR)",
            "Oppenheimer (1080p BluRay X264)",
            "Oppenheimer (1080p BluRay DS4K HEVC)",
            "Oppenheimer (1080p BluRay HEVC)",
            "Oppenheimer (1080p BluRay REMUX x264)",
        ]
    );
    let top = &v[0];
    assert_eq!(top.season, None);
    assert_eq!(top.quality(), "2160p HDR");
    assert_eq!(top.files.len(), 1);
    assert_eq!(top.files[0].episode, None);
    assert!(top.files[0]
        .name
        .starts_with("Oppenheimer (2023) IMAX 2160p UHD BluRay HDR"));
    assert_eq!(top.size_bytes, hub::parse_size("35.64 GB"));
    let remux = &v[6];
    assert!(remux.remux);
    assert_eq!(remux.size_bytes, 43_121_471_652);
    assert_eq!(default_variant(&v, &[]), Some(0));
}

#[test]
fn movie_page_skips_zip() {
    let html = r#"
        <div class="download-item a"><div class="flex-1 text-left font-semibold">Pack (2160p)</div>
        <div class="file-title">Pack.2160p.ZIP</div>
        <a href="https://hubcloud.ist/drive/zip">Download HubCloud</a></div>
        <div class="download-item b"><div class="flex-1 text-left font-semibold"> Tom &amp; Jerry&#039;s (1080p) </div>
        <div class="file-title">Tom.and.Jerry.1080p.mkv</div>
        <span class="badge" style="background-color: #ea580c; color: white;">700 MB</span>
        <a href="https://hubcloud.ist/drive/mkv">Download HubCloud</a></div>"#;
    let v = parse_variants(html, MediaType::Movie);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].label, "Tom & Jerry's (1080p)");
    assert_eq!(v[0].size_bytes, 700 * 1024 * 1024);
    assert_eq!(v[0].files[0].link, "https://hubcloud.ist/drive/mkv");
}

#[test]
fn redirect_fixture_unwraps() {
    assert_eq!(
        unwrap_redirect(REDIRECT).as_deref(),
        Some("https://hubcloud.ist/drive/ztcziri5x4v25jn")
    );
}

#[test]
fn redirect_round_trip() {
    let url = "https://hubcloud.foo/drive/abc123?x=1&y=two";
    assert_eq!(unwrap_redirect(&redirect_page(url)).as_deref(), Some(url));
}

#[test]
fn drive_and_server_pages() {
    assert_eq!(
        parse_hop(DRIVE).as_deref(),
        Some("https://gamerxyt.com/hubcloud.php?host=hubcloud&id=ztcziri5x4v25jn&token=TG1kT0ZUSmJwNGxNZzhYVzRTTjhjV1ZwaVcyMmJMQmJTYVMrTlIvT2UyYz0=")
    );
    // No R2, FSL or pixeldrain server on this page, so the workers.dev mirror wins.
    let direct = parse_direct(HUBCLOUD_PHP).unwrap();
    assert!(direct.starts_with("https://hubcloud.mobay90813998.workers.dev/"));
    assert!(direct.ends_with("(FraMeSToR-4kHdHub).mkv"));
    assert!(!direct.contains(' '));
}

#[tokio::test]
async fn find_and_variants_against_mock_site() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(query_param("s", "The Bear"))
        .and(header_regex("user-agent", r"^Mozilla/5\.0 .*Chrome/"))
        .and(header_regex("accept", r"^text/html"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SEARCH_TV))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/the-bear-series-2038/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(TV))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let base = Url::parse(&server.uri()).unwrap();
    let t = title(MediaType::Tv, "The Bear", Some(2022), Some(BEAR_POSTER));
    let page = hub::find_at(&http, &base, &t).await.unwrap().unwrap();
    assert_eq!(page.path(), "/the-bear-series-2038/");
    let v = hub::variants(&http, &page, MediaType::Tv).await.unwrap();
    assert_eq!(v.len(), 15);

    let missing = base.join("/gone/").unwrap();
    let err = hub::variants(&http, &missing, MediaType::Tv)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("http 404"), "{err}");
}

#[tokio::test]
async fn resolve_walks_redirect_drive_and_hop() {
    let server = MockServer::start().await;
    let uri = server.uri();
    let drive = format!("{uri}/drive/xyz");
    Mock::given(method("GET"))
        .and(path("/"))
        .and(query_param("id", "abc"))
        .respond_with(ResponseTemplate::new(200).set_body_string(redirect_page(&drive)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"<script>var url = '{uri}/hubcloud.php?host=hubcloud&id=xyz&token=t=';</script>"#
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/hubcloud.php"))
        .and(header("referer", drive.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<a href="https://t.me/x">t</a>
               <a href="https://pixeldrain.dev/u/Pix01">p</a>
               <a href="https://a.workers.dev/f.mkv">w</a>"#,
        ))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let direct = hub::resolve(&http, &format!("{uri}/?id=abc"))
        .await
        .unwrap();
    assert_eq!(direct, "https://pixeldrain.dev/api/file/Pix01");
}

#[tokio::test]
async fn resolve_reports_unreadable_redirect() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>no blob</html>"))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let err = hub::resolve(&http, &server.uri()).await.unwrap_err();
    assert!(err.to_string().contains("redirect"), "{err}");
}

// Live site. Run with `cargo test -p flox-rip --test hub -- --ignored`; set
// FLOX_HUB_REFRESH=1 to overwrite the fixtures with what the site serves today.

fn refresh(name: &str, html: &str) {
    if std::env::var_os("FLOX_HUB_REFRESH").is_some() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hub");
        if let Err(e) = std::fs::write(dir.join(name), html) {
            panic!("writing {name}: {e}");
        }
    }
}

#[tokio::test]
#[ignore = "hits 4khdhub.one"]
async fn live_tv() {
    let http = reqwest::Client::new();
    let t = title(MediaType::Tv, "The Bear", Some(2022), Some(BEAR_POSTER));
    let mut search = base();
    search.query_pairs_mut().append_pair("s", "The Bear");
    refresh(
        "search_tv.html",
        &hub::fetch(&http, &search, None).await.unwrap(),
    );
    let page = hub::find(&http, &t)
        .await
        .unwrap()
        .expect("The Bear on 4KHDHub");
    refresh("tv.html", &hub::fetch(&http, &page, None).await.unwrap());
    let v = hub::variants(&http, &page, MediaType::Tv).await.unwrap();
    assert!(!v.is_empty());
    assert!(v.iter().all(|v| v.season.is_some() && !v.files.is_empty()));
}

#[tokio::test]
#[ignore = "hits 4khdhub.one and HubCloud"]
async fn live_movie_and_resolve() {
    let http = reqwest::Client::new();
    let m = title(
        MediaType::Movie,
        "Oppenheimer",
        Some(2023),
        Some(OPPENHEIMER_POSTER),
    );
    let mut search = base();
    search.query_pairs_mut().append_pair("s", "Oppenheimer");
    refresh(
        "search_movie.html",
        &hub::fetch(&http, &search, None).await.unwrap(),
    );
    let page = hub::find(&http, &m)
        .await
        .unwrap()
        .expect("Oppenheimer on 4KHDHub");
    refresh("movie.html", &hub::fetch(&http, &page, None).await.unwrap());
    let v = hub::variants(&http, &page, MediaType::Movie).await.unwrap();
    assert!(!v.is_empty());

    // The last variant is the 1080p remux on today's page; any will do.
    let link = Url::parse(&v[v.len() - 1].files[0].link).unwrap();
    let redirect = hub::fetch(&http, &link, None).await.unwrap();
    let drive = Url::parse(&unwrap_redirect(&redirect).expect("redirect decodes")).unwrap();
    let drive_html = hub::fetch(&http, &drive, None).await.unwrap();
    let hop = Url::parse(&parse_hop(&drive_html).expect("hubcloud.php link")).unwrap();
    let servers = hub::fetch(&http, &hop, Some(drive.as_str())).await.unwrap();
    refresh("redirect.html", &redirect);
    refresh("drive.html", &drive_html);
    refresh("hubcloud_php.html", &servers);
    assert!(parse_direct(&servers).is_some());

    let direct = hub::resolve(&http, link.as_str()).await.unwrap();
    assert!(direct.starts_with("https://"), "{direct}");
}
