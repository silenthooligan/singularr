//! Parse, score and decide: thin over `singularr-core`, plus the shadow comparison with what
//! the arrs recorded.

pub mod model;

#[cfg(feature = "ssr")]
pub mod assess;
#[cfg(feature = "ssr")]
pub mod service;
