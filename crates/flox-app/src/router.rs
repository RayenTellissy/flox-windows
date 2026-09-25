//! Screen routing. Navigation logic is filled in by piece P16a.

use flox_core::model::{EpisodeKey, MediaType, TmdbId};

use crate::ui::Screen;

/// What the player should open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayRequest {
    pub key: EpisodeKey,
    pub start_at: Option<u32>,
    pub library_only: bool,
}

/// A destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    Search,
    Details {
        id: TmdbId,
        media: MediaType,
        library_only: bool,
    },
    Queue,
    Library,
    Settings,
    Login,
    Player(PlayRequest),
}

impl Route {
    /// The Slint screen that shows this route.
    pub fn screen(&self) -> Screen {
        match self {
            Route::Home => Screen::Home,
            Route::Search => Screen::Search,
            Route::Details { .. } => Screen::Details,
            Route::Queue => Screen::Queue,
            Route::Library => Screen::Library,
            Route::Settings => Screen::Settings,
            Route::Login => Screen::Login,
            Route::Player(_) => Screen::Player,
        }
    }
}
