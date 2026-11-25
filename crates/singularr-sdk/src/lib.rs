//! Singularr's connector SDK.
//!
//! - [`http`]: a call, its answer and the transport that carries it. The calls a connector
//!   declares ([`http::Calls`]) and the rules built from them ([`http::Allowlist`]).
//!   [`http::Allowlist::check_call`] decides whether a call may leave. It does no IO.
//! - [`error`]: what the way out says of a call it refused or that got no usable answer.
//! - [`manifest`]: what a connector type is, stated once: its kinds, group, reads, variables,
//!   secret, actions, passes and the words of what it counts.
//! - [`kinds`]: the kinds of engine a connector fills, each a trait, with the items each hands
//!   over.
//! - [`model`]: the items the catalog keeps (a title and a file as an engine reports them, the
//!   kind of a title) and an arr's own reading of a name. Features `openapi` and `sqlx` add the
//!   derives the app needs.
//! - [`read`]: readers for a service's answers.
//! - `testkit` (feature `testkit`): a transport that answers from a table and keeps every call.
//!   Tests in every crate use it.
//!
//! The crate holds no HTTP client, no database and no file access. The app sends the calls.
#![forbid(unsafe_code)]

pub mod error;
pub mod http;
pub mod kinds;
pub mod manifest;
pub mod model;
pub mod read;
#[cfg(feature = "testkit")]
pub mod testkit;
