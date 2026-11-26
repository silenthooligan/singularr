//! Plumbing: configuration, the database, the journal, the one way out, callers and errors.
//!
//! Most of it is the platform's (`singularr-platform`). Its modules keep their paths here. The
//! app keeps the configuration, its migrations (`db`), the location database, the API's
//! documents and the units.

pub mod model;

#[cfg(feature = "ssr")]
pub use singularr_platform::{
    auth, error, http, identity, jobs, journal, media, observe, outbound, runtime, smtp, store,
};

#[cfg(feature = "ssr")]
pub mod config;
#[cfg(feature = "ssr")]
pub mod db;
#[cfg(feature = "ssr")]
pub mod geoip;
#[cfg(feature = "ssr")]
pub mod schema;
#[cfg(feature = "ssr")]
pub mod units;
