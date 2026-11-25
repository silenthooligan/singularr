//! Titles, seasons, episodes, copies and libraries: one catalog over every engine.

pub mod model;

#[cfg(feature = "ssr")]
pub mod ingest;
#[cfg(feature = "ssr")]
pub mod scan;
#[cfg(feature = "ssr")]
pub mod service;
#[cfg(feature = "ssr")]
pub mod store;
#[cfg(feature = "ssr")]
pub mod sync;
