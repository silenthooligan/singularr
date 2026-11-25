//! Downloads hygiene now (owners, stuck downloads, orphans), cleanup profiles later. The
//! sync loops live here, above the engines and the catalog they connect.

pub mod model;

#[cfg(feature = "ssr")]
pub mod downloads;
#[cfg(feature = "ssr")]
pub mod server;
#[cfg(feature = "ssr")]
pub mod service;
#[cfg(feature = "ssr")]
pub mod store;
#[cfg(feature = "ssr")]
pub mod sync;
