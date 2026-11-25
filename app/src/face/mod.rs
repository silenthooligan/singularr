//! Singularr Requests, the public face: Discover, search, a title page with its Request button,
//! and the person's own requests. Nothing else. See DESIGN.md, "Two faces".
//!
//! The same image runs it with `SINGULARR_FACE=requests`. Its router (`routes::router`) is built
//! from these routes alone, so no owner route exists on it. It holds no engine key: its
//! environment is its database login (the role of `deploy/requests-role.sql`), its session secret, the TMDB
//! read token and its public address (`core::config::RequestsFaceConfig`, which refuses to
//! start when the full process's secrets are present). It reaches TMDB and plex.tv only
//! (`Allowlist::for_requests_face`).
//!
//! People sign in with Plex (`plex`), and only with a Plex account plexseerr knows. A request
//! is an intent: a row in `request_intents`, which the full process picks up
//! (`requests::intents`). In R1 nothing reaches Seerr.
//!
//! The pages are plain HTML drawn on the server (`html`), with no script, so the content
//! security policy forbids every script.

pub mod html;
pub mod plex;
pub mod routes;
pub mod session;
