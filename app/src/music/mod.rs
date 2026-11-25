//! The music library: artists, albums and tracks read from the music folder (a read-only
//! mount), with formats, covers, missing years and duplicate folders. Stage 1 reads file names
//! and sizes; tags, audio and any change to the folder come later.

pub mod model;
pub mod scan;

#[cfg(feature = "ssr")]
pub mod service;
#[cfg(feature = "ssr")]
pub mod store;
