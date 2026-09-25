//! Screen routing: the destinations and the back stack.

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

/// What a navigation did, so the shell knows which screens to set up or reset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    /// `to` was opened on top of the stack.
    Pushed { to: Route },
    /// `from` was closed and `to` is showing again.
    Popped { from: Route, to: Route },
    /// Nothing changed (BACK on Home, or the route is already showing).
    Stayed,
}

/// The back stack. Home is always at the bottom and cannot be popped.
///
/// Each screen appears at most once: opening a screen that is already on the stack
/// (Ctrl+F while on Details opened from Search, say) unwinds back to it, the way
/// Android's `FLAG_ACTIVITY_CLEAR_TOP` would, so BACK never revisits a stale copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Router {
    stack: Vec<Route>,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    pub fn new() -> Self {
        Self {
            stack: vec![Route::Home],
        }
    }

    /// The route on top.
    pub fn current(&self) -> &Route {
        // `stack` always holds Home at the bottom.
        &self.stack[self.stack.len() - 1]
    }

    /// Every route, bottom first.
    pub fn stack(&self) -> &[Route] {
        &self.stack
    }

    /// Opens `route`. When a route for the same screen is already on the stack, the
    /// stack first unwinds to just below it. Returns the closed routes, top first.
    pub fn push(&mut self, route: Route) -> (Vec<Route>, Transition) {
        if *self.current() == route {
            return (Vec::new(), Transition::Stayed);
        }
        let screen = route.screen();
        let mut closed = Vec::new();
        if let Some(pos) = self.stack.iter().position(|r| r.screen() == screen) {
            while self.stack.len() > pos.max(1) {
                closed.extend(self.stack.pop());
            }
        }
        if screen == Screen::Home {
            return match closed.first().cloned() {
                Some(from) => (
                    closed,
                    Transition::Popped {
                        from,
                        to: Route::Home,
                    },
                ),
                None => (closed, Transition::Stayed),
            };
        }
        self.stack.push(route.clone());
        (closed, Transition::Pushed { to: route })
    }

    /// BACK: closes the top route. Home stays.
    pub fn back(&mut self) -> Transition {
        if self.stack.len() < 2 {
            return Transition::Stayed;
        }
        let Some(from) = self.stack.pop() else {
            return Transition::Stayed;
        };
        Transition::Popped {
            from,
            to: self.current().clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(id: TmdbId) -> Route {
        Route::Details {
            id,
            media: MediaType::Movie,
            library_only: false,
        }
    }

    #[test]
    fn starts_on_home_and_back_stays() {
        let mut r = Router::new();
        assert_eq!(r.current(), &Route::Home);
        assert_eq!(r.back(), Transition::Stayed);
        assert_eq!(r.stack(), &[Route::Home]);
    }

    #[test]
    fn push_and_back() {
        let mut r = Router::new();
        let (closed, t) = r.push(Route::Search);
        assert!(closed.is_empty());
        assert_eq!(t, Transition::Pushed { to: Route::Search });
        r.push(details(7));
        assert_eq!(r.stack().len(), 3);
        assert_eq!(
            r.back(),
            Transition::Popped {
                from: details(7),
                to: Route::Search
            }
        );
        assert_eq!(
            r.back(),
            Transition::Popped {
                from: Route::Search,
                to: Route::Home
            }
        );
    }

    #[test]
    fn pushing_the_current_route_stays() {
        let mut r = Router::new();
        r.push(Route::Settings);
        assert_eq!(r.push(Route::Settings), (Vec::new(), Transition::Stayed));
        assert_eq!(r.stack().len(), 2);
    }

    #[test]
    fn reopening_a_screen_unwinds_to_it() {
        let mut r = Router::new();
        r.push(Route::Search);
        r.push(details(1));
        let (closed, t) = r.push(Route::Search);
        assert_eq!(closed, vec![details(1), Route::Search]);
        assert_eq!(t, Transition::Pushed { to: Route::Search });
        assert_eq!(r.stack(), &[Route::Home, Route::Search]);
    }

    #[test]
    fn details_replaces_details() {
        let mut r = Router::new();
        r.push(details(1));
        r.push(details(2));
        assert_eq!(r.stack(), &[Route::Home, details(2)]);
    }

    #[test]
    fn push_home_unwinds_everything() {
        let mut r = Router::new();
        r.push(Route::Search);
        r.push(details(3));
        let (closed, t) = r.push(Route::Home);
        assert_eq!(closed, vec![details(3), Route::Search]);
        assert_eq!(
            t,
            Transition::Popped {
                from: details(3),
                to: Route::Home
            }
        );
        assert_eq!(r.stack(), &[Route::Home]);
    }

    #[test]
    fn player_route_maps_to_player_screen() {
        let req = PlayRequest {
            key: EpisodeKey::episode(5, 2, 3),
            start_at: Some(90),
            library_only: true,
        };
        assert_eq!(Route::Player(req).screen(), Screen::Player);
        assert_eq!(details(1).screen(), Screen::Details);
    }
}
