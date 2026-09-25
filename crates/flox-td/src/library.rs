//! The channel index and library operations.
//!
//! Every uploaded part is a document message whose caption is a
//! [`crate::caption`] JSON. Parts are grouped by (key, quality, codec) into
//! prints; a print is listed once all its parts are present. A caption-less
//! `.srt`/`.vtt` document sent as a reply to a print's part-1 message is that
//! print's subtitle.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use flox_core::error::{Error, Result};
use flox_core::model::{EpisodeKey, MediaType, TmdbId};
use flox_core::settings::LibrarySort;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::caption;
use crate::chats;
use crate::transport::{json_i64, TdTransport};

/// Messages requested per `searchChatMessages` page.
pub const PAGE_LIMIT: i32 = 100;

/// Upper bound on pages per refresh, so a misbehaving server cannot loop forever.
const MAX_PAGES: usize = 10_000;

/// `downloadFile` priority for subtitles (the highest).
const SUBTITLE_PRIORITY: i32 = 32;

/// One uploaded document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub message_id: i64,
    pub file_id: i32,
    pub size: u64,
}

/// One complete print of an episode or movie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: EpisodeKey,
    pub quality: String,
    pub codec: String,
    pub parts: Vec<Part>,
    pub subtitle: Option<Part>,
    pub size: u64,
    pub newest_message_id: i64,
}

impl Entry {
    /// `"quality codec"`, such as `"2160p DV hevc"`.
    pub fn label(&self) -> String {
        format!("{} {}", self.quality, self.codec)
    }

    /// The leading number of the quality (`"2160p DV"` → 2160), 0 when absent.
    pub fn height(&self) -> u32 {
        let digits: String = self
            .quality
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().unwrap_or(0)
    }

    /// Every message of the print: the parts, then the subtitle.
    pub fn message_ids(&self) -> Vec<i64> {
        self.parts
            .iter()
            .chain(self.subtitle.iter())
            .map(|p| p.message_id)
            .collect()
    }
}

/// The document fields of a `messageDocument` message.
struct Doc<'a> {
    id: i64,
    file_name: &'a str,
    caption: &'a str,
    reply_to: Option<i64>,
    part: Part,
}

/// Reads a TDLib 1.8.x `message` holding a `messageDocument`.
fn read_doc(m: &Value) -> Option<Doc<'_>> {
    let content = m.get("content")?;
    if content.get("@type").and_then(Value::as_str) != Some("messageDocument") {
        return None;
    }
    let id = m.get("id").and_then(json_i64)?;
    let doc = content.get("document")?;
    let file = doc.get("document")?;
    let file_id = file
        .get("id")
        .and_then(json_i64)
        .and_then(|f| i32::try_from(f).ok())?;
    let size = ["size", "expected_size"]
        .iter()
        .filter_map(|k| file.get(*k).and_then(json_i64))
        .find(|s| *s > 0)
        .and_then(|s| u64::try_from(s).ok())
        .unwrap_or(0);
    // 1.8.67: `reply_to: messageReplyToMessage { chat_id, message_id }`;
    // older builds: `reply_to_message_id`.
    let reply_to = m
        .get("reply_to")
        .filter(|r| r.get("@type").and_then(Value::as_str) == Some("messageReplyToMessage"))
        .and_then(|r| r.get("message_id"))
        .or_else(|| m.get("reply_to_message_id"))
        .and_then(json_i64)
        .filter(|id| *id != 0);
    Some(Doc {
        id,
        file_name: doc
            .get("file_name")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        caption: content
            .get("caption")
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        reply_to,
        part: Part {
            message_id: id,
            file_id,
            size,
        },
    })
}

fn is_subtitle_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".srt") || lower.ends_with(".vtt")
}

/// Complete prints grouped by episode.
#[derive(Clone, Debug, Default)]
pub struct LibraryIndex {
    entries: HashMap<EpisodeKey, Vec<Entry>>,
}

impl LibraryIndex {
    /// Builds the index from `searchChatMessages` message objects, in any order.
    pub fn build(messages: &[Value]) -> LibraryIndex {
        // (key, quality, codec) → part index → (part, parts expected).
        type Group = HashMap<u32, (Part, u32)>;
        let mut groups: HashMap<(EpisodeKey, String, String), Group> = HashMap::new();
        // Replied-to message id → newest subtitle.
        let mut subtitles: HashMap<i64, Part> = HashMap::new();

        for m in messages {
            let Some(d) = read_doc(m) else { continue };
            let Some(c) = caption::parse(d.caption) else {
                if let (Some(target), true) = (d.reply_to, is_subtitle_file(d.file_name)) {
                    let newer = subtitles
                        .get(&target)
                        .is_none_or(|old| old.message_id < d.id);
                    if newer {
                        subtitles.insert(target, d.part);
                    }
                }
                continue;
            };
            let slot = groups
                .entry((c.key(), c.quality.clone(), c.codec.clone()))
                .or_default();
            let newer = slot
                .get(&c.part)
                .is_none_or(|(old, _)| old.message_id < d.id);
            if newer {
                slot.insert(c.part, (d.part, c.parts));
            }
        }

        let mut entries: HashMap<EpisodeKey, Vec<Entry>> = HashMap::new();
        for ((key, quality, codec), group) in groups {
            let mut sorted: Vec<(u32, (Part, u32))> = group.into_iter().collect();
            sorted.sort_by_key(|(i, _)| *i);
            let Some((_, (_, expected))) = sorted.first() else {
                continue;
            };
            if sorted.len() < *expected as usize {
                continue;
            }
            let parts: Vec<Part> = sorted.into_iter().map(|(_, (p, _))| p).collect();
            let subtitle = parts
                .first()
                .and_then(|p| subtitles.get(&p.message_id))
                .cloned();
            let size = parts.iter().map(|p| p.size).sum();
            let newest_message_id = parts.iter().map(|p| p.message_id).max().unwrap_or(0);
            entries.entry(key).or_default().push(Entry {
                key,
                quality,
                codec,
                parts,
                subtitle,
                size,
                newest_message_id,
            });
        }
        for list in entries.values_mut() {
            list.sort_by(|a, b| {
                b.height()
                    .cmp(&a.height())
                    .then_with(|| a.label().cmp(&b.label()))
            });
        }
        LibraryIndex { entries }
    }

    /// The prints of one episode, tallest first.
    pub fn entries_for(&self, key: EpisodeKey) -> &[Entry] {
        self.entries.get(&key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every title with at least one print, newest upload first.
    pub fn titles(&self) -> Vec<(TmdbId, MediaType)> {
        self.titles_by(LibrarySort::DateAdded)
    }

    /// Every title with at least one print in `sort` order: date added is the
    /// newest message id among the title's prints, size is its largest print.
    /// [`LibrarySort::Title`] needs TMDB names, so it returns the titles in id
    /// order for the caller to sort once the names are known.
    pub fn titles_by(&self, sort: LibrarySort) -> Vec<(TmdbId, MediaType)> {
        let mut stats: HashMap<(TmdbId, MediaType), (i64, u64)> = HashMap::new();
        for e in self.entries.values().flatten() {
            let s = stats.entry((e.key.tmdb, e.key.media)).or_default();
            s.0 = s.0.max(e.newest_message_id);
            s.1 = s.1.max(e.size);
        }
        let mut out: Vec<((TmdbId, MediaType), (i64, u64))> = stats.into_iter().collect();
        let media_rank = |m: MediaType| u8::from(m == MediaType::Tv);
        out.sort_by(|(a, sa), (b, sb)| {
            let by_id = a.0.cmp(&b.0).then(media_rank(a.1).cmp(&media_rank(b.1)));
            match sort {
                LibrarySort::DateAdded => sb.0.cmp(&sa.0).then(by_id),
                LibrarySort::Size => sb.1.cmp(&sa.1).then(by_id),
                LibrarySort::Title => by_id,
            }
        });
        out.into_iter().map(|(t, _)| t).collect()
    }

    /// The newest message id among a title's prints, 0 when it has none.
    pub fn date_added(&self, tmdb: TmdbId, media: MediaType) -> i64 {
        self.of_title(tmdb, media)
            .map(|e| e.newest_message_id)
            .max()
            .unwrap_or(0)
    }

    /// The size of a title's largest print, 0 when it has none.
    pub fn title_size(&self, tmdb: TmdbId, media: MediaType) -> u64 {
        self.of_title(tmdb, media)
            .map(|e| e.size)
            .max()
            .unwrap_or(0)
    }

    /// Every print of a title, in no particular order.
    pub fn of_title(&self, tmdb: TmdbId, media: MediaType) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(move |(k, _)| k.tmdb == tmdb && k.media == media)
            .flat_map(|(_, v)| v.iter())
    }

    /// Every print in the library, in no particular order.
    pub fn all(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values().flatten()
    }

    /// Season numbers of a show with at least one print, ascending.
    pub fn seasons(&self, tmdb: TmdbId) -> Vec<u32> {
        let mut s: Vec<u32> = self
            .entries
            .keys()
            .filter(|k| k.tmdb == tmdb && k.media == MediaType::Tv)
            .map(|k| k.season)
            .collect();
        s.sort_unstable();
        s.dedup();
        s
    }

    /// Whether `key` has at least one print.
    pub fn has(&self, key: EpisodeKey) -> bool {
        !self.entries_for(key).is_empty()
    }

    /// Whether the library has no prints.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The print of `key` with exactly this quality and codec.
    pub fn find(&self, key: EpisodeKey, quality: &str, codec: &str) -> Option<&Entry> {
        self.entries_for(key)
            .iter()
            .find(|e| e.quality == quality && e.codec == codec)
    }

    /// The preferred quality if present, else the tallest. `preferred` matches
    /// either the quality (`"1080p"`) or the full label (`"1080p hevc"`).
    pub fn default_print(&self, key: EpisodeKey, preferred: Option<&str>) -> Option<&Entry> {
        let all = self.entries_for(key);
        preferred
            .filter(|p| !p.is_empty())
            .and_then(|p| all.iter().find(|e| e.quality == p || e.label() == p))
            .or_else(|| all.first())
    }
}

/// `deleteMessages revoke:true`. Does nothing for an empty list.
pub async fn delete_messages(t: &dyn TdTransport, chat_id: i64, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    t.request(json!({
        "@type": "deleteMessages",
        "chat_id": chat_id,
        "message_ids": ids,
        "revoke": true,
    }))
    .await
    .map(|_| ())
}

/// Replace semantics before an upload: deletes the print of `key` with the
/// same quality and codec, if `index` has one, and returns the deleted
/// message ids (empty when there was nothing to replace).
pub async fn replace_existing(
    t: &dyn TdTransport,
    chat_id: i64,
    index: &LibraryIndex,
    key: EpisodeKey,
    quality: &str,
    codec: &str,
) -> Result<Vec<i64>> {
    let Some(e) = index.find(key, quality, codec) else {
        return Ok(Vec::new());
    };
    let ids = e.message_ids();
    delete_messages(t, chat_id, &ids).await?;
    Ok(ids)
}

/// Every document message in `chat_id`, newest first, paging
/// `searchChatMessages` on `next_from_message_id`.
pub async fn list_documents(t: &dyn TdTransport, chat_id: i64) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut from: i64 = 0;
    for _ in 0..MAX_PAGES {
        let page = t
            .request(json!({
                "@type": "searchChatMessages",
                "chat_id": chat_id,
                "query": "",
                "from_message_id": from,
                "offset": 0,
                "limit": PAGE_LIMIT,
                "filter": {"@type": "searchMessagesFilterDocument"},
            }))
            .await?;
        let messages = page
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next = page
            .get("next_from_message_id")
            .and_then(json_i64)
            .unwrap_or(0);
        let empty = messages.is_empty();
        out.extend(messages);
        if empty || next == 0 || next == from {
            return Ok(out);
        }
        from = next;
    }
    tracing::warn!("searchChatMessages: stopped after {MAX_PAGES} pages");
    Ok(out)
}

/// Channel-level library operations.
pub struct Library {
    transport: Arc<dyn TdTransport>,
    chat_id: Mutex<Option<i64>>,
}

impl Library {
    pub fn new(t: Arc<dyn TdTransport>) -> Self {
        Self {
            transport: t,
            chat_id: Mutex::new(None),
        }
    }

    /// The channel found by the last [`Library::refresh`], if any.
    pub fn chat_id(&self) -> Option<i64> {
        *self.chat_id.lock()
    }

    fn require_chat(&self) -> Result<i64> {
        self.chat_id()
            .ok_or_else(|| Error::Unavailable("the library channel is not loaded".into()))
    }

    /// Finds the channel and rebuilds the index. A missing channel is an empty
    /// library.
    pub async fn refresh(&self, channel_title: &str) -> Result<Arc<LibraryIndex>> {
        let t = self.transport.as_ref();
        let Some(chat_id) = chats::find_channel(t, channel_title).await? else {
            *self.chat_id.lock() = None;
            return Ok(Arc::new(LibraryIndex::default()));
        };
        *self.chat_id.lock() = Some(chat_id);
        let messages = list_documents(t, chat_id).await?;
        Ok(Arc::new(LibraryIndex::build(&messages)))
    }

    /// `deleteMessages revoke:true` for every part and the subtitle.
    pub async fn delete_entry(&self, e: &Entry) -> Result<()> {
        let chat_id = self.require_chat()?;
        delete_messages(self.transport.as_ref(), chat_id, &e.message_ids()).await
    }

    /// [`replace_existing`] in the loaded channel.
    pub async fn replace_existing(
        &self,
        index: &LibraryIndex,
        key: EpisodeKey,
        quality: &str,
        codec: &str,
    ) -> Result<Vec<i64>> {
        let chat_id = self.require_chat()?;
        replace_existing(self.transport.as_ref(), chat_id, index, key, quality, codec).await
    }

    /// Downloads the subtitle fully and returns its local path.
    pub async fn download_subtitle(&self, e: &Entry, timeout: Duration) -> Result<PathBuf> {
        let sub = e
            .subtitle
            .as_ref()
            .ok_or_else(|| Error::Unavailable(format!("{} has no subtitle", e.label())))?;
        let req = self.transport.request(json!({
            "@type": "downloadFile",
            "file_id": sub.file_id,
            "priority": SUBTITLE_PRIORITY,
            "offset": 0,
            "limit": 0,
            "synchronous": true,
        }));
        let file = tokio::time::timeout(timeout, req)
            .await
            .map_err(|_| Error::Timeout("subtitle download".into()))??;
        let local = file.get("local");
        let done = local
            .and_then(|l| l.get("is_downloading_completed"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let path = local
            .and_then(|l| l.get("path"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !done || path.is_empty() {
            return Err(Error::Unavailable(
                "the subtitle download did not complete".into(),
            ));
        }
        Ok(PathBuf::from(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::tests::Fake;

    /// A TDLib 1.8.67 `message` holding a `messageDocument`.
    fn doc_msg(id: i64, file_id: i32, size: i64, name: &str, caption: &str) -> Value {
        json!({
            "@type": "message",
            "id": id,
            "sender_id": {"@type": "messageSenderChat", "chat_id": -1001},
            "chat_id": -1001,
            "is_outgoing": true,
            "is_pinned": false,
            "can_be_edited": true,
            "date": 1_700_000_000,
            "edit_date": 0,
            "reply_to": null,
            "message_thread_id": 0,
            "content": {
                "@type": "messageDocument",
                "document": {
                    "@type": "document",
                    "file_name": name,
                    "mime_type": "video/x-matroska",
                    "document": {
                        "@type": "file",
                        "id": file_id,
                        "size": size,
                        "expected_size": size,
                        "local": {"@type": "localFile", "path": "", "is_downloading_completed": false},
                        "remote": {"@type": "remoteFile", "id": format!("r{file_id}"), "is_uploading_completed": true, "uploaded_size": size}
                    }
                },
                "caption": {"@type": "formattedText", "text": caption, "entities": []}
            }
        })
    }

    fn reply(mut m: Value, to: i64) -> Value {
        m["reply_to"] =
            json!({"@type": "messageReplyToMessage", "chat_id": -1001, "message_id": to});
        m
    }

    fn cap(key: EpisodeKey, q: &str, codec: &str, part: u32, parts: u32) -> String {
        caption::encode(&caption::Caption::new(key, q, codec, part, parts))
    }

    const GOT: TmdbId = 1399;

    fn ep() -> EpisodeKey {
        EpisodeKey::episode(GOT, 1, 3)
    }

    /// Hand-authored `searchChatMessages` pages, newest first.
    fn fixture() -> Vec<Value> {
        let e = ep();
        vec![
            // Subtitle reply to the 1080p part 1 (message 110).
            reply(doc_msg(140, 40, 30_000, "S01E03.en.SRT", ""), 110),
            // A re-upload of 1080p part 2 wins over message 111.
            doc_msg(130, 31, 2_100, "b.mkv", &cap(e, "1080p", "hevc", 2, 2)),
            // Incomplete 720p print: 1 of 2.
            doc_msg(120, 20, 700, "c.mkv", &cap(e, "720p", "h264", 1, 2)),
            doc_msg(111, 11, 2_000, "b.mkv", &cap(e, "1080p", "hevc", 2, 2)),
            doc_msg(110, 10, 1_000, "a.mkv", &cap(e, "1080p", "hevc", 1, 2)),
            // Same key, taller and DV.
            doc_msg(
                105,
                5,
                9_000,
                "d.mkv",
                &format!("S01E03 {}", cap(e, "2160p DV", "hevc", 1, 1)),
            ),
            // Same height and codec family, sorted by label.
            doc_msg(104, 4, 3_000, "e.mkv", &cap(e, "1080p", "av1", 1, 1)),
            // Caption-less non-subtitle reply is ignored.
            reply(doc_msg(103, 3, 10, "notes.txt", ""), 110),
            // A movie.
            doc_msg(
                100,
                1,
                50_000,
                "m.mkv",
                &cap(EpisodeKey::movie(603), "1080p", "hevc", 1, 1),
            ),
            // Not a document.
            json!({"@type": "message", "id": 99, "content": {"@type": "messageText", "text": {"text": "hi"}}}),
        ]
    }

    #[test]
    fn label_and_height() {
        let e = Entry {
            key: EpisodeKey::movie(1),
            quality: "2160p DV".into(),
            codec: "hevc".into(),
            parts: vec![],
            subtitle: None,
            size: 0,
            newest_message_id: 0,
        };
        assert_eq!(e.label(), "2160p DV hevc");
        assert_eq!(e.height(), 2160);
    }

    #[test]
    fn groups_complete_prints_sorted_tallest_first() {
        let idx = LibraryIndex::build(&fixture());
        let labels: Vec<String> = idx.entries_for(ep()).iter().map(Entry::label).collect();
        assert_eq!(labels, ["2160p DV hevc", "1080p av1", "1080p hevc"]);
        assert!(idx.find(ep(), "720p", "h264").is_none());
    }

    #[test]
    fn newest_message_wins_per_part() {
        let idx = LibraryIndex::build(&fixture());
        let e = idx.find(ep(), "1080p", "hevc").unwrap();
        assert_eq!(
            e.parts,
            vec![
                Part {
                    message_id: 110,
                    file_id: 10,
                    size: 1_000
                },
                Part {
                    message_id: 130,
                    file_id: 31,
                    size: 2_100
                },
            ]
        );
        assert_eq!(e.size, 3_100);
        assert_eq!(e.newest_message_id, 130);
    }

    #[test]
    fn subtitle_replies_attach_to_part_one() {
        let idx = LibraryIndex::build(&fixture());
        let e = idx.find(ep(), "1080p", "hevc").unwrap();
        assert_eq!(
            e.subtitle,
            Some(Part {
                message_id: 140,
                file_id: 40,
                size: 30_000
            })
        );
        assert_eq!(e.message_ids(), vec![110, 130, 140]);
        assert!(idx
            .find(ep(), "2160p DV", "hevc")
            .unwrap()
            .subtitle
            .is_none());
    }

    #[test]
    fn newer_subtitle_wins_and_legacy_reply_field_is_read() {
        let e = EpisodeKey::movie(7);
        let mut old = doc_msg(3, 3, 1, "a.vtt", "");
        old["reply_to_message_id"] = json!(1);
        let msgs = vec![
            doc_msg(1, 1, 10, "m.mkv", &cap(e, "720p", "h264", 1, 1)),
            reply(doc_msg(5, 5, 1, "b.srt", ""), 1),
            old,
        ];
        let idx = LibraryIndex::build(&msgs);
        assert_eq!(
            idx.entries_for(e)[0]
                .subtitle
                .as_ref()
                .map(|p| p.message_id),
            Some(5)
        );
    }

    #[test]
    fn order_of_messages_does_not_matter() {
        let mut rev = fixture();
        rev.reverse();
        let a = LibraryIndex::build(&fixture());
        let b = LibraryIndex::build(&rev);
        assert_eq!(a.entries_for(ep()), b.entries_for(ep()));
    }

    #[test]
    fn default_print_prefers_the_preferred_quality() {
        let idx = LibraryIndex::build(&fixture());
        let q = |p| idx.default_print(ep(), p).map(Entry::label);
        assert_eq!(q(None).as_deref(), Some("2160p DV hevc"));
        assert_eq!(q(Some("720p")).as_deref(), Some("2160p DV hevc"));
        assert_eq!(q(Some("1080p")).as_deref(), Some("1080p av1"));
        assert_eq!(q(Some("1080p hevc")).as_deref(), Some("1080p hevc"));
        assert_eq!(q(Some("")).as_deref(), Some("2160p DV hevc"));
        assert!(idx.default_print(EpisodeKey::movie(1), None).is_none());
    }

    #[test]
    fn titles_by_date_added_and_size() {
        let idx = LibraryIndex::build(&fixture());
        let got = (GOT, MediaType::Tv);
        let matrix = (603, MediaType::Movie);
        assert_eq!(idx.titles(), vec![got, matrix]);
        assert_eq!(idx.titles_by(LibrarySort::DateAdded), vec![got, matrix]);
        assert_eq!(idx.titles_by(LibrarySort::Size), vec![matrix, got]);
        assert_eq!(idx.titles_by(LibrarySort::Title), vec![matrix, got]);
        assert_eq!(idx.date_added(GOT, MediaType::Tv), 130);
        assert_eq!(idx.title_size(603, MediaType::Movie), 50_000);
        assert_eq!(idx.seasons(GOT), vec![1]);
        assert_eq!(idx.all().count(), 4);
    }

    fn found(ids: &[i64], next: i64, all: &[Value]) -> Value {
        let msgs: Vec<Value> = all
            .iter()
            .filter(|m| ids.contains(&m["id"].as_i64().unwrap()))
            .cloned()
            .collect();
        json!({"@type": "foundChatMessages", "total_count": all.len(), "messages": msgs, "next_from_message_id": next})
    }

    #[tokio::test]
    async fn refresh_pages_on_next_from_message_id() {
        let fake = Fake::new();
        fake.respond_after(
            "loadChats",
            vec![json!({"@type": "updateNewChat", "chat": {"@type": "chat", "id": -1001, "title": "Flox Library"}})],
            Err(Error::Td { code: 404, message: "Not Found".into() }),
        );
        fake.respond(
            "getChats",
            Ok(json!({"@type": "chats", "chat_ids": [-1001]})),
        );
        let all = fixture();
        fake.respond(
            "searchChatMessages",
            Ok(found(&[140, 130, 120, 111, 110], 110, &all)),
        );
        fake.respond(
            "searchChatMessages",
            Ok(found(&[105, 104, 103, 100, 99], 99, &all)),
        );
        fake.respond("searchChatMessages", Ok(found(&[], 0, &all)));

        let lib = Library::new(fake.clone());
        let idx = lib.refresh("flox library").await.unwrap();
        assert_eq!(lib.chat_id(), Some(-1001));
        assert_eq!(idx.entries_for(ep()).len(), 3);

        let reqs = fake.sent("searchChatMessages");
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0]["from_message_id"], 0);
        assert_eq!(reqs[1]["from_message_id"], 110);
        assert_eq!(reqs[2]["from_message_id"], 99);
        assert_eq!(reqs[0]["chat_id"], -1001);
        assert_eq!(reqs[0]["query"], "");
        assert_eq!(reqs[0]["limit"], 100);
        assert_eq!(reqs[0]["filter"]["@type"], "searchMessagesFilterDocument");
    }

    #[tokio::test]
    async fn missing_channel_is_an_empty_library() {
        let fake = Fake::new();
        fake.respond(
            "loadChats",
            Err(Error::Td {
                code: 404,
                message: "Not Found".into(),
            }),
        );
        fake.respond("getChats", Ok(json!({"@type": "chats", "chat_ids": []})));
        let lib = Library::new(fake.clone());
        let idx = lib.refresh("Flox Library").await.unwrap();
        assert!(idx.is_empty());
        assert_eq!(lib.chat_id(), None);
        assert!(fake.sent("searchChatMessages").is_empty());
        let e = LibraryIndex::build(&fixture())
            .find(ep(), "1080p", "hevc")
            .cloned()
            .unwrap();
        assert!(matches!(
            lib.delete_entry(&e).await,
            Err(Error::Unavailable(_))
        ));
    }

    async fn loaded() -> (Arc<Fake>, Library, LibraryIndex) {
        let fake = Fake::new();
        fake.respond_after(
            "loadChats",
            vec![json!({"@type": "updateNewChat", "chat": {"@type": "chat", "id": -1001, "title": "Flox Library"}})],
            Err(Error::Td { code: 404, message: "Not Found".into() }),
        );
        fake.respond(
            "getChats",
            Ok(json!({"@type": "chats", "chat_ids": [-1001]})),
        );
        fake.respond("searchChatMessages", Ok(found(&[], 0, &[])));
        let lib = Library::new(fake.clone());
        lib.refresh("Flox Library").await.unwrap();
        (fake, lib, LibraryIndex::build(&fixture()))
    }

    #[tokio::test]
    async fn delete_revokes_parts_and_subtitle() {
        let (fake, lib, idx) = loaded().await;
        let e = idx.find(ep(), "1080p", "hevc").unwrap();
        lib.delete_entry(e).await.unwrap();
        let reqs = fake.sent("deleteMessages");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0]["chat_id"], -1001);
        assert_eq!(reqs[0]["message_ids"], json!([110, 130, 140]));
        assert_eq!(reqs[0]["revoke"], true);
    }

    #[tokio::test]
    async fn replace_deletes_only_the_same_quality_and_codec() {
        let (fake, lib, idx) = loaded().await;
        let gone = lib
            .replace_existing(&idx, ep(), "1080p", "av1")
            .await
            .unwrap();
        assert_eq!(gone, vec![104]);
        let none = lib
            .replace_existing(&idx, ep(), "720p", "h264")
            .await
            .unwrap();
        assert!(none.is_empty());
        let reqs = fake.sent("deleteMessages");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0]["message_ids"], json!([104]));
    }

    #[tokio::test]
    async fn subtitle_downloads_synchronously() {
        let (fake, lib, idx) = loaded().await;
        fake.respond(
            "downloadFile",
            Ok(json!({"@type": "file", "id": 40, "local": {"@type": "localFile", "path": "/tmp/td/sub.srt", "is_downloading_completed": true}})),
        );
        let e = idx.find(ep(), "1080p", "hevc").unwrap();
        let p = lib
            .download_subtitle(e, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(p, PathBuf::from("/tmp/td/sub.srt"));
        let req = &fake.sent("downloadFile")[0];
        assert_eq!(req["file_id"], 40);
        assert_eq!(req["synchronous"], true);

        let no_sub = idx.find(ep(), "1080p", "av1").unwrap();
        assert!(lib
            .download_subtitle(no_sub, Duration::from_secs(5))
            .await
            .is_err());
    }
}
