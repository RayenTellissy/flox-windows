//! Finding (or creating) the library channel by title. Filled in by piece P4.

use flox_core::error::{Error, Result};

use crate::transport::TdTransport;

/// Description given to a channel Flox creates.
pub const CHANNEL_DESCRIPTION: &str = "Flox library";

/// The chat id of the channel whose title matches `title` case-insensitively.
pub async fn find_channel(_t: &dyn TdTransport, _title: &str) -> Result<Option<i64>> {
    Err(Error::NotImplemented("flox_td::chats::find_channel"))
}

/// [`find_channel`], else `createNewSupergroupChat { is_channel: true }`.
pub async fn find_or_create_channel(_t: &dyn TdTransport, _title: &str) -> Result<i64> {
    Err(Error::NotImplemented(
        "flox_td::chats::find_or_create_channel",
    ))
}
