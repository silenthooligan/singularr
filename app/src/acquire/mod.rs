//! Search, grab and import (DESIGN.md, `acquire`). Until Singularr searches through the indexer
//! manager itself, a manager (Radarr, Sonarr) searches, grabs and imports, and Singularr asks it
//! to, by permission: each ask is one of the manager's declared acts, allowed only while its
//! access is `act`. What a search found is judged here in the title's quality profile, beside
//! the manager's own word. Nothing here depends on who searched: a release is a
//! [`crate::engines::FoundRelease`] wherever it came from, so the same views serve Singularr's own
//! search later.
//!
//! - `verdict`: Singularr's verdict on each release, against the file held now (pure).
//! - `refusal`: what a manager's refusal of an import says, in Singularr's terms (pure).
//! - `model`: the answers.
//! - `service`: the searches, grabs, imports and removals, each one journaled.

pub mod model;
pub mod refusal;
pub mod service;
pub mod verdict;
