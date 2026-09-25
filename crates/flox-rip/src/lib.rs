//! The ingest pipeline behind the queue: page sniffing, direct and DASH downloads,
//! 4KHDHub resolution, ffmpeg muxing, probing, splitting and uploading.

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
