//! Flox for Windows: the Slint shell, spatial focus, view models and the player.

pub mod app;
pub mod fixtures;
pub mod focus;
pub mod player;
pub mod router;
pub mod vm;

/// The compiled Slint UI (`ui/app.slint`). The allows cover generated code only.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::todo)]
pub mod ui {
    slint::include_modules!();
}

pub use app::{run, AppContext};
pub use ui::{AppWindow, Screen};
