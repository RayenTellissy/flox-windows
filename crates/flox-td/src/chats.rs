//! Finding (or creating) the library channel by title.

use std::collections::HashMap;
use std::sync::Arc;

use flox_core::error::{Error, Result};
use serde_json::{json, Value};
use tokio::sync::broadcast::{self, error::TryRecvError};

use crate::transport::{json_i64, TdTransport};

/// Description given to a channel Flox creates.
pub const CHANNEL_DESCRIPTION: &str = "Flox library";

/// Chats requested per `loadChats` call.
const LOAD_CHATS_LIMIT: i32 = 100;

/// Upper bound on `loadChats` calls, so a misbehaving server cannot loop forever.
const MAX_LOAD_CHATS_CALLS: usize = 1000;

/// How many main-list chat ids `getChats` returns.
const GET_CHATS_LIMIT: i32 = 10_000;

/// Records titles from `updateNewChat` and `updateChatTitle`.
fn note_title(u: &Value, titles: &mut HashMap<i64, String>) {
    let (id, title) = match u.get("@type").and_then(Value::as_str) {
        Some("updateNewChat") => {
            let chat = u.get("chat");
            (
                chat.and_then(|c| c.get("id")).and_then(json_i64),
                chat.and_then(|c| c.get("title")),
            )
        }
        Some("updateChatTitle") => (u.get("chat_id").and_then(json_i64), u.get("title")),
        _ => return,
    };
    if let (Some(id), Some(title)) = (id, title.and_then(Value::as_str)) {
        titles.insert(id, title.to_owned());
    }
}

/// Takes every update already queued on `rx`.
fn drain(rx: &mut broadcast::Receiver<Arc<Value>>, titles: &mut HashMap<i64, String>) {
    loop {
        match rx.try_recv() {
            Ok(u) => note_title(&u, titles),
            Err(TryRecvError::Lagged(n)) => {
                tracing::debug!("find_channel missed {n} updates; getChat fills the gaps");
            }
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
}

fn same_title(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// The chat id of the channel whose title matches `title` case-insensitively.
///
/// Loads the main chat list with `loadChats` until TDLib answers 404, noting
/// titles from `updateNewChat`/`updateChatTitle`. TDLib announces each chat
/// only once per session, so the list is then read with `getChats` and any
/// title not seen in updates is fetched with `getChat`.
pub async fn find_channel(t: &dyn TdTransport, title: &str) -> Result<Option<i64>> {
    let want = title.trim();
    let mut rx = t.updates();
    let mut titles = HashMap::new();

    for _ in 0..MAX_LOAD_CHATS_CALLS {
        let r = t
            .request(json!({
                "@type": "loadChats",
                "chat_list": {"@type": "chatListMain"},
                "limit": LOAD_CHATS_LIMIT,
            }))
            .await;
        drain(&mut rx, &mut titles);
        match r {
            Ok(_) => {}
            Err(Error::Td { code: 404, .. }) => break,
            Err(e) => return Err(e),
        }
    }

    let list = t
        .request(json!({
            "@type": "getChats",
            "chat_list": {"@type": "chatListMain"},
            "limit": GET_CHATS_LIMIT,
        }))
        .await?;
    drain(&mut rx, &mut titles);
    let ids: Vec<i64> = list
        .get("chat_ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(json_i64).collect())
        .unwrap_or_default();

    for id in &ids {
        let known = titles.get(id).cloned();
        let name = match known {
            Some(n) => n,
            None => {
                let chat = t
                    .request(json!({"@type": "getChat", "chat_id": id}))
                    .await?;
                let n = chat
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                titles.insert(*id, n.clone());
                n
            }
        };
        if same_title(&name, want) {
            return Ok(Some(*id));
        }
    }

    // Chats announced in updates but missing from the list snapshot.
    let mut rest: Vec<(&i64, &String)> = titles
        .iter()
        .filter(|(id, name)| !ids.contains(id) && same_title(name, want))
        .collect();
    rest.sort();
    Ok(rest.first().map(|(id, _)| **id))
}

/// [`find_channel`], else `createNewSupergroupChat { is_channel: true }`.
pub async fn find_or_create_channel(t: &dyn TdTransport, title: &str) -> Result<i64> {
    if let Some(id) = find_channel(t, title).await? {
        return Ok(id);
    }
    let chat = t
        .request(json!({
            "@type": "createNewSupergroupChat",
            "title": title.trim(),
            "is_forum": false,
            "is_channel": true,
            "description": CHANNEL_DESCRIPTION,
            "message_auto_delete_time": 0,
            "for_import": false,
        }))
        .await?;
    chat.get("id")
        .and_then(json_i64)
        .ok_or_else(|| Error::Other("createNewSupergroupChat returned no chat id".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::tests::Fake;

    fn not_found() -> Result<Value> {
        Err(Error::Td {
            code: 404,
            message: "Not Found".into(),
        })
    }

    fn new_chat(id: i64, title: &str) -> Value {
        json!({"@type": "updateNewChat", "chat": {"@type": "chat", "id": id, "title": title}})
    }

    #[tokio::test]
    async fn finds_by_title_from_updates_ignoring_case() {
        let fake = Fake::new();
        fake.respond_after(
            "loadChats",
            vec![new_chat(1, "Saved"), new_chat(-1007, "flox library")],
            Ok(json!({"@type": "ok"})),
        );
        fake.respond_after(
            "loadChats",
            vec![json!({"@type": "updateChatTitle", "chat_id": 1, "title": "Notes"})],
            not_found(),
        );
        fake.respond(
            "getChats",
            Ok(json!({"@type": "chats", "chat_ids": [1, -1007]})),
        );
        let id = find_channel(fake.as_ref(), "Flox Library").await.unwrap();
        assert_eq!(id, Some(-1007));
        assert_eq!(fake.sent("loadChats").len(), 2);
        assert!(fake.sent("getChat").is_empty());
    }

    #[tokio::test]
    async fn fetches_titles_it_has_not_seen() {
        let fake = Fake::new();
        fake.respond("loadChats", not_found());
        fake.respond(
            "getChats",
            Ok(json!({"@type": "chats", "chat_ids": [5, "-1009"]})),
        );
        fake.respond(
            "getChat",
            Ok(json!({"@type": "chat", "id": 5, "title": "Other"})),
        );
        fake.respond(
            "getChat",
            Ok(json!({"@type": "chat", "id": -1009, "title": "FLOX LIBRARY"})),
        );
        let id = find_channel(fake.as_ref(), "Flox Library").await.unwrap();
        assert_eq!(id, Some(-1009));
        assert_eq!(fake.sent("getChat").len(), 2);
    }

    #[tokio::test]
    async fn other_load_errors_propagate() {
        let fake = Fake::new();
        fake.respond(
            "loadChats",
            Err(Error::Td {
                code: 401,
                message: "Unauthorized".into(),
            }),
        );
        let r = find_channel(fake.as_ref(), "Flox Library").await;
        assert!(matches!(r, Err(Error::Td { code: 401, .. })));
    }

    #[tokio::test]
    async fn creates_the_channel_when_missing() {
        let fake = Fake::new();
        fake.respond("loadChats", not_found());
        fake.respond("getChats", Ok(json!({"@type": "chats", "chat_ids": []})));
        fake.respond(
            "createNewSupergroupChat",
            Ok(json!({"@type": "chat", "id": -10042, "title": "Flox Library"})),
        );
        let id = find_or_create_channel(fake.as_ref(), "Flox Library")
            .await
            .unwrap();
        assert_eq!(id, -10042);
        let req = &fake.sent("createNewSupergroupChat")[0];
        assert_eq!(req["title"], "Flox Library");
        assert_eq!(req["is_channel"], true);
        assert_eq!(req["description"], "Flox library");
    }

    #[tokio::test]
    async fn existing_channel_is_not_recreated() {
        let fake = Fake::new();
        fake.respond_after("loadChats", vec![new_chat(9, "Flox Library")], not_found());
        fake.respond("getChats", Ok(json!({"@type": "chats", "chat_ids": [9]})));
        assert_eq!(
            find_or_create_channel(fake.as_ref(), "Flox Library")
                .await
                .unwrap(),
            9
        );
        assert!(fake.sent("createNewSupergroupChat").is_empty());
    }
}
