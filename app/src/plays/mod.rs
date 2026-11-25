//! Plays: who played what, when, on which device, and how it was delivered.
//!
//! Three sources tell of plays: the media server's sessions while they play, its own history
//! of what was played, and Tautulli where one is configured. Each play is one row whoever told
//! of it, with each source's own record beside it (`service::absorb`). The statistics are read
//! from that table (`store`). A play links to the catalog title it belongs to (an episode's
//! show) and keeps the media server's key of its item, so a play the catalog cannot place
//! still counts.
//!
//! The sources are only read: the media server and Tautulli with GET, through their ports
//! (`engines`). A player's address is kept for the owner while the setting says so (the
//! default); otherwise a play says home network or outside and nothing more. An address is
//! placed on this machine with a location database (`locations`). It never goes to a log and
//! never reaches Singularr Requests.

pub mod locations;
pub mod model;
pub mod service;
pub mod store;
