//! The API: `/api/v1` routes, the OpenAPI document, the event stream and API keys.
//! Long answers leave compressed (`compress`).

pub mod model;

#[cfg(feature = "ssr")]
pub mod acquire;
#[cfg(feature = "ssr")]
pub mod acts;
#[cfg(feature = "ssr")]
pub mod compress;
#[cfg(feature = "ssr")]
pub mod config;
#[cfg(feature = "ssr")]
pub mod journal;
#[cfg(feature = "ssr")]
pub mod metadata;
pub mod naming;
#[cfg(feature = "ssr")]
pub mod notifications;
#[cfg(feature = "ssr")]
pub mod pages;
#[cfg(feature = "ssr")]
pub mod panels;
#[cfg(feature = "ssr")]
pub mod processing;
#[cfg(feature = "ssr")]
pub mod processing_rules;
pub mod profiles;
#[cfg(feature = "ssr")]
pub mod routes;
#[cfg(feature = "ssr")]
pub mod rules;
#[cfg(feature = "ssr")]
pub mod search;
#[cfg(feature = "ssr")]
pub mod sections;
#[cfg(feature = "ssr")]
pub mod server;
#[cfg(feature = "ssr")]
pub mod state;
#[cfg(feature = "ssr")]
pub mod stats;
pub mod worker;
