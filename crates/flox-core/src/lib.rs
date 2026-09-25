//! Platform-neutral core of Flox for Windows: models, settings, watch progress,
//! TMDB, the image cache, path and tool resolution, and the sniffing contract
//! shared by `flox-web`, `flox-rip` and `flox-app`.

pub mod error;
pub mod fmt;
pub mod images;
pub mod lang;
pub mod model;
pub mod paths;
pub mod progress;
pub mod settings;
pub mod sniff;
pub mod tmdb;
pub mod tools;

pub use error::{Error, Result};

/// The workspace version, shown on the Settings ABOUT row.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_1_0_0() {
        assert_eq!(super::VERSION, "1.0.0");
    }
}
