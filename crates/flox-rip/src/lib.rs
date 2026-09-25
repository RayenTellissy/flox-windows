//! The ingest pipeline behind the queue: page sniffing, direct and DASH downloads,
//! 4KHDHub resolution, ffmpeg muxing, probing, splitting and uploading.

/// The browser every request of a job presents: desktop Chrome 128 on Windows. Sent by the
/// downloaders, the 4KHDHub client and ffmpeg's HLS pulls (`-user_agent`).
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

pub mod download;
pub mod hub;
pub mod job;
pub mod mux;
pub mod names;
mod pipeline;
pub mod probe;
pub mod process;
pub mod queue;
pub mod split;
pub mod temp;
pub mod tools;
