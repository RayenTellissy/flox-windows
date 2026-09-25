//! The channel index and library operations. Filled in by piece P5.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use flox_core::error::{Error, Result};
use flox_core::model::{EpisodeKey, MediaType, TmdbId};

use crate::transport::TdTransport;

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
}

/// Complete prints grouped by episode.
#[derive(Clone, Debug, Default)]
pub struct LibraryIndex {
    entries: HashMap<EpisodeKey, Vec<Entry>>,
}

impl LibraryIndex {
    /// Builds the index from `searchChatMessages` message objects. Filled in by P5.
    #[allow(clippy::unimplemented)]
    pub fn build(_messages: &[serde_json::Value]) -> LibraryIndex {
        unimplemented!("flox_td::library::LibraryIndex::build (P5)")
    }

    /// The prints of one episode, tallest first.
    pub fn entries_for(&self, key: EpisodeKey) -> &[Entry] {
        self.entries.get(&key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every title with at least one print. Filled in by P5.
    #[allow(clippy::unimplemented)]
    pub fn titles(&self) -> Vec<(TmdbId, MediaType)> {
        unimplemented!("flox_td::library::LibraryIndex::titles (P5)")
    }

    /// The preferred quality if present, else the tallest. Filled in by P5.
    #[allow(clippy::unimplemented)]
    pub fn default_print(&self, _key: EpisodeKey, _preferred: Option<&str>) -> Option<&Entry> {
        unimplemented!("flox_td::library::LibraryIndex::default_print (P5)")
    }
}

/// Channel-level library operations.
pub struct Library {
    #[allow(dead_code)]
    transport: Arc<dyn TdTransport>,
}

impl Library {
    pub fn new(t: Arc<dyn TdTransport>) -> Self {
        Self { transport: t }
    }

    /// Finds the channel and rebuilds the index.
    pub async fn refresh(&self, _channel_title: &str) -> Result<Arc<LibraryIndex>> {
        Err(Error::NotImplemented("flox_td::library::Library::refresh"))
    }

    /// `deleteMessages revoke:true` for every part and the subtitle.
    pub async fn delete_entry(&self, _e: &Entry) -> Result<()> {
        Err(Error::NotImplemented(
            "flox_td::library::Library::delete_entry",
        ))
    }

    /// Downloads the subtitle fully and returns its local path.
    pub async fn download_subtitle(&self, _e: &Entry, _timeout: Duration) -> Result<PathBuf> {
        Err(Error::NotImplemented(
            "flox_td::library::Library::download_subtitle",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
