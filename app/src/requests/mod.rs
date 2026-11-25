//! Requests from both Seerr sites in one list, with the season gate and the friend-request
//! cleanup that apply to each, and the discover lists with what Singularr holds of each title.
//!
//! Stage 1 reads: every request on both sites every few minutes, each title looked up once,
//! the discover lists on demand (kept 30 minutes). Approving, declining and requesting are
//! stage 2. Seerr stays the interface outside users reach.

pub mod diff;
pub mod model;
pub mod rules;

#[cfg(feature = "ssr")]
pub mod discover;
#[cfg(feature = "ssr")]
pub mod intents;
/// The portal's database role, kept by the full process at each start.
#[cfg(feature = "ssr")]
pub mod role;
#[cfg(feature = "ssr")]
pub mod service;
#[cfg(feature = "ssr")]
pub mod store;
#[cfg(feature = "ssr")]
pub mod sync;
