//! Flox for Windows: the Slint shell, spatial focus, view models and the player.

pub mod app;
pub mod focus;
pub mod player;
pub mod router;
pub mod vm;

/// The compiled Slint UI (`ui/app.slint`). The allows cover generated code only.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::todo)]
pub mod ui {
    slint::include_modules!();
}

pub use ui::{AppWindow, Screen};

/// Opens the main window and runs the event loop until it closes.
/// The runtime, stores and services are wired in by piece P16a.
pub fn run() -> anyhow::Result<()> {
    use slint::ComponentHandle;

    let window = AppWindow::new()?;
    window.set_version(flox_core::VERSION.into());
    window.run()?;
    Ok(())
}
