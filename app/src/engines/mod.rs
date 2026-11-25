//! Engines behind ports. An engine is another program Singularr talks to: the Radarr and Sonarr
//! integrations (legacy, read in stage 1), the download clients (qBittorrent, Transmission,
//! Deluge, SABnzbd, NZBGet), Seerr, the media servers (Plex, Jellyfin, Emby, each behind
//! [`MediaServer`]), the processing services (a worker such as
//! Tdarr, a bridge such as HUSH) and a play history (Tautulli). An adapter is created per
//! configured integration; nothing here names an instance.
//!
//! Singularr reads every engine and acts on one only by permission. What it may do with an
//! integration is its access (`access`): `read` by default, `act` where the owner allows it
//! and the type's adapter declares actions. An adapter sends GET, the calls it declares as
//! reads (a sign-in, a query a service takes with POST), and the calls it declares as acts;
//! `core::outbound` refuses everything else for an engine's host, and the acts while the
//! access is `read`.

pub mod model;

#[cfg(feature = "ssr")]
pub mod access;
#[cfg(feature = "ssr")]
pub mod arr;
#[cfg(feature = "ssr")]
pub mod clients;
#[cfg(feature = "ssr")]
pub mod deluge;
#[cfg(feature = "ssr")]
pub mod hush;
#[cfg(feature = "ssr")]
pub mod jellyfin;
#[cfg(feature = "ssr")]
pub mod manifests;
#[cfg(feature = "ssr")]
pub mod nzbget;
#[cfg(feature = "ssr")]
pub mod plex;
#[cfg(feature = "ssr")]
pub mod qbit;
#[cfg(feature = "ssr")]
pub mod sabnzbd;
#[cfg(feature = "ssr")]
pub mod seerr;
#[cfg(feature = "ssr")]
pub mod service;
#[cfg(feature = "ssr")]
pub mod store;
#[cfg(feature = "ssr")]
pub mod tautulli;
#[cfg(feature = "ssr")]
pub mod tdarr;
#[cfg(feature = "ssr")]
pub mod transmission;

/// The kinds (the ports) and the items they hand over: the SDK's, at their old paths.
#[cfg(feature = "ssr")]
pub use singularr_sdk::kinds::*;

/// The name Singularr gives a service for itself when it hands a file over, so the
/// service's own record of the job says who asked.
#[cfg(feature = "ssr")]
pub const SENDER: &str = "singularr";

/// The names this product sent under before, which a service's records still hold.
#[cfg(feature = "ssr")]
pub const EARLIER_SENDERS: &[&str] = &["marquee"];
