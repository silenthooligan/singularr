//! Singularr, a self-hosted workspace for media management on Plex, Jellyfin or Emby. One binary, a
//! modular monolith.
//!
//! The modules are layers, lowest first: `core`, `settings`, `policy`, `catalog`, `releases`,
//! `engines`, `metadata`, `lifecycle`, `acquire`, `processing`, `plays`, `requests`, `music`,
//! `homemovies`, `search`, `notify`, `face`, `api`, `client`. A module imports only the modules before it in that
//! list, `singularr-core`, `singularr-sdk` and `singularr-platform`. SQL lives only in a module's own `store.rs`. Every outbound HTTP
//! call goes through `core::outbound`. See DESIGN.md.
//!
//! The interface is not in this crate: it is the web client in `web/`, static files that read
//! `/api/v1`, which `client` hands out.
#![forbid(unsafe_code)]
#![recursion_limit = "512"]

pub mod core;

/// Every setting described as data, where each value comes from, and the values saved here.
#[cfg(feature = "ssr")]
pub mod settings;

pub mod policy;

pub mod catalog;

pub mod releases;

pub mod engines;

pub mod metadata;

pub mod lifecycle;

/// Search, grab and import through the managers: every release a search found with Singularr's
/// verdict beside the manager's, and the managers' acts, each journaled.
#[cfg(feature = "ssr")]
pub mod acquire;

/// What the processing services did to each file after its import: their runs, kept and linked
/// to titles.
#[cfg(feature = "ssr")]
pub mod processing;

/// Who played what, when, where and how: the media server's sessions and history, and
/// Tautulli, as one row per play.
#[cfg(feature = "ssr")]
pub mod plays;

pub mod requests;

pub mod music;

pub mod homemovies;

/// Search over what the library has: built in (PostgreSQL), or Meilisearch when configured.
pub mod search;

/// Notifications: channels, the events they subscribe to, and the messages they are sent.
#[cfg(feature = "ssr")]
pub mod notify;

/// Singularr Requests, the public face (`SINGULARR_FACE=requests`).
#[cfg(feature = "ssr")]
pub mod face;

pub mod api;

/// The web client's files (`web/` in the repository), handed out at clean addresses.
#[cfg(feature = "ssr")]
pub mod client;
