//! TMDB v3 client with an in-memory LRU of response bodies keyed by URL.
//!
//! Mirrors the Android `Tmdb.kt` contract: `language=en-US` and `api_key` on
//! every request, gzip, a 10 s timeout, 32 cached responses, 1 h TTL for
//! trending and search and 24 h for details and seasons. Parsing is lenient
//! in the same way as Android's `optString` / `optInt`.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lru::LruCache;
use parking_lot::Mutex;
use serde_json::Value;
use url::Url;

use crate::error::{Error, Result};
use crate::model::{EpisodeInfo, MediaType, SeasonInfo, TitleDetails, TitleSummary, TmdbId};
use crate::settings::SettingsStore;

/// REST base URL.
pub const API_BASE: &str = "https://api.themoviedb.org/3";
/// Image base URL; a size segment and the path follow.
pub const IMAGE_BASE: &str = "https://image.tmdb.org/t/p/";
/// Search keeps the top results by popularity.
pub const SEARCH_LIMIT: usize = 36;
/// Response cache capacity (entries keyed by URL).
pub const CACHE_ENTRIES: usize = 32;
/// TTL for trending and search lists.
pub const LIST_TTL: Duration = Duration::from_secs(60 * 60);
/// TTL for details and seasons.
pub const DETAIL_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Per-request timeout.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The message carried by [`Error::Tmdb`] when no API key is configured.
pub const MISSING_KEY_MESSAGE: &str = "TMDB API key is not set. Add it in Settings.";

/// TMDB image sizes: `w342` posters, `w300` stills, `w500` the details poster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageSize {
    W300,
    W342,
    W500,
}

impl ImageSize {
    /// The URL segment, such as `"w342"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ImageSize::W300 => "w300",
            ImageSize::W342 => "w342",
            ImageSize::W500 => "w500",
        }
    }
}

/// The URL path segment for a media type.
fn media_segment(media: MediaType) -> &'static str {
    match media {
        MediaType::Movie => "movie",
        MediaType::Tv => "tv",
    }
}

struct Cached {
    at: Instant,
    body: Arc<Value>,
}

/// The TMDB client. The API key is read from the [`SettingsStore`] on every request.
pub struct Tmdb {
    http: reqwest::Client,
    settings: SettingsStore,
    base: String,
    cache: Mutex<LruCache<String, Cached>>,
    /// Added to `Instant::now()`; lets tests move the clock forward.
    clock_skew: Mutex<Duration>,
}

impl Tmdb {
    /// Builds the HTTP client (gzip, 10 s timeout) against [`API_BASE`].
    pub fn new(settings: SettingsStore) -> Result<Tmdb> {
        Self::with_base(settings, API_BASE)
    }

    /// Like [`Tmdb::new`] with another REST base URL (a mock server in tests).
    pub fn with_base(settings: SettingsStore, base: &str) -> Result<Tmdb> {
        let http = reqwest::Client::builder()
            .gzip(true)
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| Error::Http(e.without_url()))?;
        let capacity = NonZeroUsize::new(CACHE_ENTRIES).unwrap_or(NonZeroUsize::MIN);
        Ok(Tmdb {
            http,
            settings,
            base: base.trim_end_matches('/').to_owned(),
            cache: Mutex::new(LruCache::new(capacity)),
            clock_skew: Mutex::new(Duration::ZERO),
        })
    }

    /// `/trending/{movie|tv}/week`. Person and poster-less results are dropped.
    pub async fn trending(&self, media: MediaType) -> Result<Vec<TitleSummary>> {
        let path = format!("/trending/{}/week", media_segment(media));
        let body = self.fetch(&path, &[], LIST_TTL).await?;
        Ok(parse_items(&body, media))
    }

    /// `/search/movie` + `/search/tv` in parallel, merged by popularity, top 36.
    /// A blank query returns an empty list without a request.
    pub async fn search(&self, query: &str) -> Result<Vec<TitleSummary>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let params = [("query", query), ("include_adult", "false")];
        let (movies, tv) = futures::future::try_join(
            self.fetch("/search/movie", &params, LIST_TTL),
            self.fetch("/search/tv", &params, LIST_TTL),
        )
        .await?;
        let mut merged = parse_items(&movies, MediaType::Movie);
        merged.extend(parse_items(&tv, MediaType::Tv));
        // Stable, so equal popularity keeps movies before TV as on Android.
        merged.sort_by(|a, b| b.popularity.total_cmp(&a.popularity));
        merged.truncate(SEARCH_LIMIT);
        Ok(merged)
    }

    /// `/{movie|tv}/{id}`. Season 0 (specials) is dropped.
    pub async fn details(&self, media: MediaType, id: TmdbId) -> Result<TitleDetails> {
        let path = format!("/{}/{id}", media_segment(media));
        let body = self.fetch(&path, &[], DETAIL_TTL).await?;
        Ok(parse_details(&body, media, id))
    }

    /// `/tv/{id}/season/{n}`.
    pub async fn season(&self, id: TmdbId, season: u32) -> Result<Vec<EpisodeInfo>> {
        let path = format!("/tv/{id}/season/{season}");
        let body = self.fetch(&path, &[], DETAIL_TTL).await?;
        Ok(parse_episodes(&body, season))
    }

    /// The full image URL for a TMDB path such as `/abc.jpg`.
    pub fn image_url(path: &str, size: ImageSize) -> String {
        format!("{IMAGE_BASE}{}{path}", size.as_str())
    }

    /// Drops every cached response.
    pub fn clear_cache(&self) {
        self.cache.lock().clear();
    }

    fn now(&self) -> Instant {
        Instant::now() + *self.clock_skew.lock()
    }

    #[cfg(test)]
    fn advance_clock(&self, by: Duration) {
        *self.clock_skew.lock() += by;
    }

    async fn fetch(
        &self,
        path: &str,
        params: &[(&str, &str)],
        ttl: Duration,
    ) -> Result<Arc<Value>> {
        let key = self
            .settings
            .get()
            .effective_tmdb_api_key()
            .ok_or_else(|| Error::Tmdb(MISSING_KEY_MESSAGE.to_owned()))?;
        let mut url = Url::parse(&format!("{}{path}", self.base))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("api_key", &key);
            q.append_pair("language", "en-US");
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        let url = url.to_string();

        let now = self.now();
        if let Some(hit) = self.cache.lock().get(&url) {
            if now.saturating_duration_since(hit.at) < ttl {
                return Ok(hit.body.clone());
            }
        }

        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| Error::Http(e.without_url()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| Error::Http(e.without_url()))?;
        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&text).ok().and_then(|v| {
                v.get("status_message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            return Err(Error::Tmdb(match detail {
                Some(msg) => format!("HTTP {} on {path}: {msg}", status.as_u16()),
                None => format!("HTTP {} on {path}", status.as_u16()),
            }));
        }
        let body = Arc::new(serde_json::from_str::<Value>(&text)?);
        self.cache.lock().put(
            url,
            Cached {
                at: now,
                body: body.clone(),
            },
        );
        Ok(body)
    }
}

/// `optString`: a string value, or empty.
fn opt_str<'a>(o: &'a Value, key: &str) -> &'a str {
    o.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A non-empty string value, or `None` (null, missing or empty).
fn opt_path(o: &Value, key: &str) -> Option<String> {
    Some(opt_str(o, key))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `optInt`: an integer value (floats truncate), or `None`.
fn opt_u64(o: &Value, key: &str) -> Option<u64> {
    let v = o.get(key)?;
    v.as_u64()
        .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
}

fn opt_u32(o: &Value, key: &str) -> Option<u32> {
    opt_u64(o, key).and_then(|n| u32::try_from(n).ok())
}

/// A positive u32, as Android's `optInt(..).takeIf { it > 0 }`.
fn positive_u32(v: Option<u32>) -> Option<u32> {
    v.filter(|n| *n > 0)
}

fn title_of(o: &Value) -> String {
    let t = opt_str(o, "title");
    if t.is_empty() {
        opt_str(o, "name").to_owned()
    } else {
        t.to_owned()
    }
}

/// The year from `release_date`, else `first_air_date`.
fn year_of(o: &Value) -> Option<u16> {
    let d = opt_str(o, "release_date");
    let d = if d.is_empty() {
        opt_str(o, "first_air_date")
    } else {
        d
    };
    d.get(..4).and_then(|y| y.parse().ok())
}

fn summary_of(o: &Value, media: MediaType, fallback_id: TmdbId) -> TitleSummary {
    TitleSummary {
        id: opt_u64(o, "id").unwrap_or(fallback_id),
        media,
        title: title_of(o),
        year: year_of(o),
        poster_path: opt_path(o, "poster_path"),
        overview: opt_str(o, "overview").to_owned(),
        popularity: o.get("popularity").and_then(Value::as_f64).unwrap_or(0.0),
    }
}

/// The `results` of a list response, minus people and poster-less items.
fn parse_items(body: &Value, media: MediaType) -> Vec<TitleSummary> {
    body.get("results")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|o| o.is_object())
                .filter(|o| opt_str(o, "media_type") != "person")
                .filter(|o| opt_path(o, "poster_path").is_some())
                .map(|o| summary_of(o, media, 0))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_details(o: &Value, media: MediaType, id: TmdbId) -> TitleDetails {
    let runtime_min = positive_u32(opt_u32(o, "runtime")).or_else(|| {
        o.get("episode_run_time")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
    });
    let seasons = o
        .get("seasons")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|s| s.is_object())
                .filter_map(|s| {
                    let number = opt_u32(s, "season_number").filter(|n| *n > 0)?;
                    let name = opt_str(s, "name");
                    Some(SeasonInfo {
                        number,
                        name: if name.is_empty() {
                            format!("Season {number}")
                        } else {
                            name.to_owned()
                        },
                        episode_count: opt_u32(s, "episode_count").unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    TitleDetails {
        summary: summary_of(o, media, id),
        runtime_min,
        seasons,
    }
}

fn parse_episodes(o: &Value, season: u32) -> Vec<EpisodeInfo> {
    o.get("episodes")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .enumerate()
                .filter(|(_, e)| e.is_object())
                .map(|(i, e)| EpisodeInfo {
                    season: opt_u32(e, "season_number").unwrap_or(season),
                    episode: opt_u32(e, "episode_number")
                        .unwrap_or_else(|| u32::try_from(i + 1).unwrap_or(u32::MAX)),
                    name: opt_str(e, "name").to_owned(),
                    overview: opt_str(e, "overview").to_owned(),
                    still_path: opt_path(e, "still_path"),
                    runtime_min: positive_u32(opt_u32(e, "runtime")),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{build_defaults, Settings};
    use std::io::Write;
    use std::path::PathBuf;
    use wiremock::matchers::{header_regex, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TRENDING_MOVIE: &str = include_str!("../testdata/tmdb/trending_movie.json");
    const TRENDING_TV: &str = include_str!("../testdata/tmdb/trending_tv.json");
    const SEARCH_MOVIE: &str = include_str!("../testdata/tmdb/search_movie.json");
    const SEARCH_TV: &str = include_str!("../testdata/tmdb/search_tv.json");
    const MOVIE_DETAILS: &str = include_str!("../testdata/tmdb/movie_details.json");
    const TV_DETAILS: &str = include_str!("../testdata/tmdb/tv_details.json");
    const SEASON: &str = include_str!("../testdata/tmdb/tv_season.json");

    fn store(key: Option<&str>) -> SettingsStore {
        let settings = Settings {
            tmdb_api_key: key.map(str::to_owned),
            ..Settings::default()
        };
        SettingsStore::new(PathBuf::from("unused-settings.json"), settings)
    }

    fn json(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
    }

    fn gzip_json(body: &str) -> ResponseTemplate {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(body.as_bytes()).unwrap();
        let bytes = enc.finish().unwrap();
        ResponseTemplate::new(200)
            .insert_header("Content-Encoding", "gzip")
            .set_body_raw(bytes, "application/json")
    }

    async fn client(server: &MockServer) -> Tmdb {
        Tmdb::with_base(store(Some("k123")), &server.uri()).unwrap()
    }

    fn common(m: wiremock::MockBuilder) -> wiremock::MockBuilder {
        m.and(query_param("api_key", "k123"))
            .and(query_param("language", "en-US"))
            .and(header_regex("accept-encoding", "gzip"))
            .and(header_regex("accept", "application/json"))
    }

    #[test]
    fn futures_are_send_and_the_client_is_sync() {
        fn send<T: Send>(_: &T) {}
        fn sync<T: Send + Sync>() {}
        sync::<Tmdb>();
        let tmdb = Tmdb::new(store(Some("k"))).unwrap();
        send(&tmdb.trending(MediaType::Movie));
        send(&tmdb.search("x"));
        send(&tmdb.details(MediaType::Tv, 1));
        send(&tmdb.season(1, 1));
    }

    #[test]
    fn image_url_joins_size_and_path() {
        assert_eq!(
            Tmdb::image_url("/p.jpg", ImageSize::W342),
            "https://image.tmdb.org/t/p/w342/p.jpg"
        );
        assert_eq!(
            Tmdb::image_url("/s.jpg", ImageSize::W300),
            "https://image.tmdb.org/t/p/w300/s.jpg"
        );
        assert_eq!(
            Tmdb::image_url("/d.jpg", ImageSize::W500),
            "https://image.tmdb.org/t/p/w500/d.jpg"
        );
    }

    #[tokio::test]
    async fn trending_movie_parses_and_drops_people_and_posterless() {
        let server = MockServer::start().await;
        common(Mock::given(method("GET")).and(path("/trending/movie/week")))
            .respond_with(gzip_json(TRENDING_MOVIE))
            .expect(1)
            .mount(&server)
            .await;
        let tmdb = client(&server).await;
        let items = tmdb.trending(MediaType::Movie).await.unwrap();
        assert_eq!(
            items,
            vec![
                TitleSummary {
                    id: 27205,
                    media: MediaType::Movie,
                    title: "Inception".into(),
                    year: Some(2010),
                    poster_path: Some("/inception.jpg".into()),
                    overview: "Dreams within dreams.".into(),
                    popularity: 88.5,
                },
                TitleSummary {
                    id: 603,
                    media: MediaType::Movie,
                    title: "The Matrix".into(),
                    year: None,
                    poster_path: Some("/matrix.jpg".into()),
                    overview: String::new(),
                    popularity: 0.0,
                },
            ]
        );
    }

    #[tokio::test]
    async fn trending_tv_uses_name_and_first_air_date() {
        let server = MockServer::start().await;
        common(Mock::given(method("GET")).and(path("/trending/tv/week")))
            .respond_with(json(TRENDING_TV))
            .expect(1)
            .mount(&server)
            .await;
        let items = client(&server).await.trending(MediaType::Tv).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, 1399);
        assert_eq!(items[0].media, MediaType::Tv);
        assert_eq!(items[0].title, "Game of Thrones");
        assert_eq!(items[0].year, Some(2011));
    }

    #[tokio::test]
    async fn search_merges_by_popularity_and_caps_at_36() {
        let server = MockServer::start().await;
        common(Mock::given(method("GET")).and(path("/search/movie")))
            .and(query_param("query", "the office & co"))
            .and(query_param("include_adult", "false"))
            .respond_with(json(SEARCH_MOVIE))
            .expect(1)
            .mount(&server)
            .await;
        common(Mock::given(method("GET")).and(path("/search/tv")))
            .and(query_param("query", "the office & co"))
            .and(query_param("include_adult", "false"))
            .respond_with(json(SEARCH_TV))
            .expect(1)
            .mount(&server)
            .await;
        let tmdb = client(&server).await;
        let items = tmdb.search("the office & co").await.unwrap();
        let got: Vec<(TmdbId, MediaType)> = items.iter().map(|i| (i.id, i.media)).collect();
        assert_eq!(
            got,
            vec![
                (2316, MediaType::Tv),
                (1, MediaType::Movie),
                (2, MediaType::Movie),
                (2996, MediaType::Tv),
                (3, MediaType::Movie),
            ]
        );
        assert!(items.windows(2).all(|w| w[0].popularity >= w[1].popularity));
        // Cached: a repeat search makes no further requests (expect(1) above).
        assert_eq!(tmdb.search("the office & co").await.unwrap(), items);
    }

    #[tokio::test]
    async fn search_keeps_the_top_36() {
        let server = MockServer::start().await;
        let many = |offset: u64| {
            let results: Vec<Value> = (0..30)
                .map(|i| {
                    serde_json::json!({
                        "id": offset + i,
                        "title": format!("t{i}"),
                        "poster_path": "/p.jpg",
                        "popularity": (offset + i) as f64,
                    })
                })
                .collect();
            serde_json::json!({ "results": results }).to_string()
        };
        Mock::given(path("/search/movie"))
            .respond_with(json(&many(0)))
            .mount(&server)
            .await;
        Mock::given(path("/search/tv"))
            .respond_with(json(&many(100)))
            .mount(&server)
            .await;
        let items = client(&server).await.search("x").await.unwrap();
        assert_eq!(items.len(), SEARCH_LIMIT);
        assert_eq!(items[0].id, 129);
        assert_eq!(items[35].id, 24);
    }

    #[tokio::test]
    async fn blank_search_makes_no_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json("{}"))
            .expect(0)
            .mount(&server)
            .await;
        assert!(client(&server).await.search("  ").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn movie_details_parse() {
        let server = MockServer::start().await;
        common(Mock::given(method("GET")).and(path("/movie/27205")))
            .respond_with(json(MOVIE_DETAILS))
            .expect(1)
            .mount(&server)
            .await;
        let d = client(&server)
            .await
            .details(MediaType::Movie, 27205)
            .await
            .unwrap();
        assert_eq!(d.summary.title, "Inception");
        assert_eq!(d.summary.year, Some(2010));
        assert_eq!(d.summary.poster_path.as_deref(), Some("/inception.jpg"));
        assert_eq!(d.runtime_min, Some(148));
        assert!(d.seasons.is_empty());
    }

    #[tokio::test]
    async fn tv_details_parse_runtime_and_seasons() {
        let server = MockServer::start().await;
        common(Mock::given(method("GET")).and(path("/tv/1399")))
            .respond_with(json(TV_DETAILS))
            .expect(1)
            .mount(&server)
            .await;
        let d = client(&server)
            .await
            .details(MediaType::Tv, 1399)
            .await
            .unwrap();
        assert_eq!(d.summary.id, 1399);
        assert_eq!(d.summary.title, "Game of Thrones");
        assert_eq!(d.summary.media, MediaType::Tv);
        assert_eq!(d.runtime_min, Some(57));
        assert_eq!(
            d.seasons,
            vec![
                SeasonInfo {
                    number: 1,
                    name: "Season 1".into(),
                    episode_count: 10,
                },
                SeasonInfo {
                    number: 2,
                    name: "Season 2".into(),
                    episode_count: 10,
                },
            ]
        );
    }

    #[tokio::test]
    async fn season_parses_episodes() {
        let server = MockServer::start().await;
        common(Mock::given(method("GET")).and(path("/tv/1399/season/1")))
            .respond_with(json(SEASON))
            .expect(1)
            .mount(&server)
            .await;
        let eps = client(&server).await.season(1399, 1).await.unwrap();
        assert_eq!(
            eps,
            vec![
                EpisodeInfo {
                    season: 1,
                    episode: 1,
                    name: "Winter Is Coming".into(),
                    overview: "Lord Stark is troubled.".into(),
                    still_path: Some("/still1.jpg".into()),
                    runtime_min: Some(62),
                },
                EpisodeInfo {
                    season: 1,
                    episode: 2,
                    name: "The Kingsroad".into(),
                    overview: String::new(),
                    still_path: None,
                    runtime_min: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn list_ttl_is_one_hour() {
        let server = MockServer::start().await;
        Mock::given(path("/trending/movie/week"))
            .respond_with(json(TRENDING_MOVIE))
            .expect(2)
            .mount(&server)
            .await;
        let tmdb = client(&server).await;
        tmdb.trending(MediaType::Movie).await.unwrap();
        tmdb.trending(MediaType::Movie).await.unwrap();
        tmdb.advance_clock(LIST_TTL - Duration::from_secs(1));
        tmdb.trending(MediaType::Movie).await.unwrap();
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            1
        );
        tmdb.advance_clock(Duration::from_secs(2));
        tmdb.trending(MediaType::Movie).await.unwrap();
        tmdb.trending(MediaType::Movie).await.unwrap();
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            2
        );
    }

    #[tokio::test]
    async fn detail_and_season_ttl_is_24_hours() {
        let server = MockServer::start().await;
        Mock::given(path("/tv/1399"))
            .respond_with(json(TV_DETAILS))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(path("/tv/1399/season/1"))
            .respond_with(json(SEASON))
            .expect(2)
            .mount(&server)
            .await;
        let tmdb = client(&server).await;
        tmdb.details(MediaType::Tv, 1399).await.unwrap();
        tmdb.season(1399, 1).await.unwrap();
        tmdb.advance_clock(Duration::from_secs(2 * 60 * 60));
        tmdb.details(MediaType::Tv, 1399).await.unwrap();
        tmdb.season(1399, 1).await.unwrap();
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            2
        );
        tmdb.advance_clock(DETAIL_TTL);
        tmdb.details(MediaType::Tv, 1399).await.unwrap();
        tmdb.season(1399, 1).await.unwrap();
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            4
        );
    }

    #[tokio::test]
    async fn lru_holds_32_urls() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(SEASON))
            .mount(&server)
            .await;
        let tmdb = client(&server).await;
        for n in 1..=33 {
            tmdb.season(1, n).await.unwrap();
        }
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            33
        );
        // Season 1 was the least recently used and has been evicted; 33 is still cached.
        tmdb.season(1, 33).await.unwrap();
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            33
        );
        tmdb.season(1, 1).await.unwrap();
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            34
        );
    }

    #[tokio::test]
    async fn changing_the_key_misses_the_cache() {
        let server = MockServer::start().await;
        Mock::given(path("/tv/1399/season/1"))
            .respond_with(json(SEASON))
            .expect(2)
            .mount(&server)
            .await;
        let a = Tmdb::with_base(store(Some("a")), &server.uri()).unwrap();
        a.season(1399, 1).await.unwrap();
        let b = Tmdb::with_base(store(Some("b")), &server.uri()).unwrap();
        b.season(1399, 1).await.unwrap();
    }

    #[tokio::test]
    async fn empty_key_is_a_clear_error() {
        if build_defaults().tmdb_api_key.is_some() {
            return;
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json("{}"))
            .expect(0)
            .mount(&server)
            .await;
        for key in [None, Some(""), Some("   ")] {
            let tmdb = Tmdb::with_base(store(key), &server.uri()).unwrap();
            match tmdb.trending(MediaType::Movie).await {
                Err(Error::Tmdb(msg)) => assert_eq!(msg, MISSING_KEY_MESSAGE),
                other => panic!("expected a missing key error, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn http_errors_carry_the_status_message_but_not_the_key() {
        let server = MockServer::start().await;
        Mock::given(path("/movie/1"))
            .respond_with(ResponseTemplate::new(401).set_body_raw(
                br#"{"status_code":7,"status_message":"Invalid API key: You must be granted a valid key."}"#
                    .to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;
        let err = client(&server)
            .await
            .details(MediaType::Movie, 1)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("HTTP 401"), "{msg}");
        assert!(msg.contains("Invalid API key"), "{msg}");
        assert!(!msg.contains("k123"), "{msg}");
    }

    #[tokio::test]
    async fn errors_are_not_cached() {
        let server = MockServer::start().await;
        Mock::given(path("/tv/5/season/1"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(path("/tv/5/season/1"))
            .respond_with(json(SEASON))
            .mount(&server)
            .await;
        let tmdb = client(&server).await;
        assert!(tmdb.season(5, 1).await.is_err());
        assert_eq!(tmdb.season(5, 1).await.unwrap().len(), 2);
    }

    /// Hits the real API. Run with `TMDB_API_KEY=... cargo test -p flox-core -- --ignored live_tmdb`.
    #[tokio::test]
    #[ignore = "needs network and TMDB_API_KEY"]
    async fn live_tmdb() {
        let key = std::env::var("TMDB_API_KEY").unwrap_or_default();
        assert!(!key.is_empty(), "set TMDB_API_KEY");
        let tmdb = Tmdb::new(store(Some(&key))).unwrap();
        let movies = tmdb.trending(MediaType::Movie).await.unwrap();
        assert!(!movies.is_empty());
        assert!(movies.iter().all(|m| m.poster_path.is_some()));
        let tv = tmdb.trending(MediaType::Tv).await.unwrap();
        assert!(!tv.is_empty());
        let found = tmdb.search("breaking bad").await.unwrap();
        assert!(found.len() <= SEARCH_LIMIT);
        assert!(found
            .iter()
            .any(|t| t.id == 1396 && t.media == MediaType::Tv));
        let d = tmdb.details(MediaType::Tv, 1396).await.unwrap();
        assert_eq!(d.summary.title, "Breaking Bad");
        assert_eq!(d.seasons.len(), 5);
        let eps = tmdb.season(1396, 1).await.unwrap();
        assert_eq!(eps.len(), 7);
        let m = tmdb.details(MediaType::Movie, 27205).await.unwrap();
        assert_eq!(m.summary.year, Some(2010));
        assert!(m.runtime_min.is_some());
    }
}
