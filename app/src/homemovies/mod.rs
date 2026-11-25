//! The home movies: one tape per video file in the home movies folder (a read-only mount),
//! with years, chapters, people, places and notes. The file names give a first reading; the
//! owner's annotations, kept in Singularr's own tables, correct it. This is the one place stage 1
//! writes, and only to Singularr's database. No outside metadata is used and nothing about the
//! tapes leaves the server.

pub mod model;
pub mod scan;

#[cfg(feature = "ssr")]
pub mod service;
#[cfg(feature = "ssr")]
pub mod store;
