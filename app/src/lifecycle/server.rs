//! The server pass: what a media server says, read every hour (`SINGULARR_SYNC_SERVER_SECS`).
//! Each media server (Plex, Jellyfin, Emby) has a pass of its own, through the same port.
//!
//! One pass reads the server's identity, its sections (written as libraries, each mapped to
//! the Radarr or Sonarr integration whose folders it holds), the items of every movie and show
//! section (matched to the catalog's titles: Rotten Tomatoes ratings, plays, the edition) and
//! the music of every music section (artists, albums, tracks). Everything is written in one
//! transaction; a pass that fails writes nothing and the loop goes on.

use super::sync::{SyncDeps, jitter};
use crate::catalog::model::Library;
use crate::catalog::service as catalog;
use crate::catalog::store::{self as catalog_store, ServerLibraryWrite, TitleServerWrite};
use crate::core::jobs::{self, JobSpec};
use crate::core::observe;
use crate::engines::service::{PassOk, pass_result};
use crate::engines::{MediaServer, ServerSection};
use crate::music::store as music_store;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

/// The last folder name of a path, lower case: `/media/movies` and `/movies` are both
/// `movies`. The managers and the server see the share at different mount points.
fn folder_name(path: &str) -> Option<String> {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .map(str::to_ascii_lowercase)
        .filter(|n| !n.is_empty())
}

/// The kind of library a section is: `movie`, `show`, `music`, `homevideo` (personal media) or
/// `photo`. Plex says `artist` for music and marks personal media by its agent (a movie section
/// with no metadata agent); the other adapters give the kind in these words already.
pub fn section_kind(s: &ServerSection) -> String {
    // Plex's two agents for personal media: the current one and the one before it.
    let personal = matches!(
        s.agent.as_deref(),
        Some("tv.plex.agents.none" | "com.plexapp.agents.none")
    );
    match s.kind.as_str() {
        "artist" => "music".to_string(),
        "movie" if personal => "homevideo".to_string(),
        other => other.to_string(),
    }
}

/// Whether a library is a plain video library: one the pages show as the files the media
/// server has, with no manager behind them. A section the server marks as personal media
/// (`homevideo`) is one; so is a movie or show section that no manager looks after, whatever
/// agent it has. The workspace record, the libraries panel and the recordings route all ask
/// this one rule, so they agree on which library that is.
pub fn plain_video(kind: Option<&str>, has_section: bool, managed: bool) -> bool {
    match kind {
        Some("homevideo") => true,
        Some("movie" | "show") => has_section && !managed,
        _ => false,
    }
}

/// A section as a library, mapped to the arr integration whose root folders it holds.
pub fn section_library(
    server: &str,
    s: &ServerSection,
    arr_libraries: &[Library],
) -> ServerLibraryWrite {
    let kind = section_kind(s);
    let mine: Vec<String> = s.locations.iter().filter_map(|l| folder_name(l)).collect();
    let managed_by = arr_libraries
        .iter()
        .filter(|l| l.section_key.is_none() && l.kind.as_deref() == Some(kind.as_str()))
        .find(|l| {
            l.root_folders
                .iter()
                .filter_map(|r| folder_name(r))
                .any(|r| mine.contains(&r))
        })
        .map(|l| l.integration.clone().unwrap_or_else(|| l.id.clone()));
    ServerLibraryWrite {
        id: format!("{server}-{}", s.key),
        name: s.title.clone(),
        kind,
        integration: server.to_string(),
        section_key: s.key.clone(),
        managed_by,
        locations: s.locations.clone(),
    }
}

/// Why a listing of the server is refused, when it is far shorter than what the read model
/// holds: no section where there were some, or fewer than half of the items of a server
/// that had 20 or more matched. A small library may really be emptied, and a section may
/// really go; a server does not lose every section or half its titles between two passes.
fn implausible_listing(
    known_sections: i64,
    known_items: i64,
    sections: usize,
    items: usize,
) -> Option<String> {
    if known_sections > 0 && sections == 0 {
        return Some(format!(
            "the server listed no section where {known_sections} are known; the answer was not applied"
        ));
    }
    let items = i64::try_from(items).unwrap_or(i64::MAX);
    if known_items >= 20 && items.saturating_mul(2) < known_items {
        return Some(format!(
            "the server listed {items} items where {known_items} are known; the answer was not applied"
        ));
    }
    None
}

/// One pass over the server.
pub async fn server_pass(deps: &SyncDeps, server: &Arc<dyn MediaServer>) -> Result<PassOk, String> {
    let started = Instant::now();
    let identity = server.identity().await.map_err(|e| e.to_string())?;
    let took = started.elapsed();
    let sections = server.sections().await.map_err(|e| e.to_string())?;
    let arr_libraries = catalog::libraries(&deps.pool)
        .await
        .map_err(|e| e.to_string())?;
    let libraries: Vec<ServerLibraryWrite> = sections
        .iter()
        .map(|s| section_library(server.id(), s, &arr_libraries))
        .collect();
    let mut titles: Vec<TitleServerWrite> = Vec::new();
    let mut artists = Vec::new();
    let mut albums = Vec::new();
    let mut tracks = Vec::new();
    for (s, lib) in sections.iter().zip(&libraries) {
        match lib.kind.as_str() {
            "movie" | "show" => {
                for item in server.items(s).await.map_err(|e| e.to_string())? {
                    titles.push(TitleServerWrite {
                        rating_key: item.rating_key,
                        kind: lib.kind.clone(),
                        library_id: lib.id.clone(),
                        tmdb: item.tmdb,
                        tvdb: item.tvdb,
                        imdb: item.imdb,
                        critic_rating: item.critic_rating,
                        critic_image: item.critic_image,
                        audience_rating: item.audience_rating,
                        audience_image: item.audience_image,
                        plays: item.plays,
                        last_played: item.last_played,
                        edition: item.edition,
                        added_at: item.added,
                    });
                }
            }
            "music" => {
                artists.extend(server.artists(&s.key).await.map_err(|e| e.to_string())?);
                albums.extend(server.albums(&s.key).await.map_err(|e| e.to_string())?);
                tracks.extend(server.tracks(&s.key).await.map_err(|e| e.to_string())?);
            }
            _ => {}
        }
    }
    // A server that answers an empty or a cut listing (it is starting, its answer changed
    // shape) must not empty the read model: an answer far shorter than what is stored is
    // refused whole, the way a manager's listing is. The next pass tries again.
    let (known_sections, known_items) = catalog_store::server_counts(&deps.pool, server.id())
        .await
        .map_err(|e| e.to_string())?;
    if let Some(why) =
        implausible_listing(known_sections, known_items, libraries.len(), titles.len())
    {
        return Err(why);
    }
    let write = async {
        let mut tx = deps.journal.begin(&deps.pool).await?;
        let gone =
            catalog_store::write_server_libraries(tx.conn(), server.id(), &libraries).await?;
        catalog_store::write_server_machine(tx.conn(), server.id(), identity.machine_id.as_deref())
            .await?;
        let matched = catalog_store::write_title_server(tx.conn(), server.id(), &titles).await?;
        music_store::replace_server_music(tx.conn(), server.id(), &artists, &albums, &tracks)
            .await?;
        tx.commit().await?;
        Ok::<(u64, u64), sqlx::Error>((gone, matched))
    };
    let (gone, matched) = write.await.map_err(|e| e.to_string())?;
    Ok(PassOk {
        took,
        version: identity.version,
        counts: json!({
            "sections": libraries.len(),
            "sections_removed": gone,
            "items": titles.len(),
            "titles_matched": matched,
            "artists": artists.len(),
            "albums": albums.len(),
            "tracks": tracks.len(),
            "plex_pass": identity.plex_pass,
            // The server's own name and platform, for the workspace (`server.name`).
            "name": identity.name,
            "platform": identity.platform,
        }),
    })
}

/// The server loop: a pass, the result recorded in the engine state, then the interval.
pub async fn server_loop(deps: SyncDeps, server: Arc<dyn MediaServer>) {
    let job = jobs::registry().register(JobSpec::of(server.id(), "server", deps.config.server));
    job.wait(jitter(deps.config.jitter)).await;
    loop {
        let started = job.begin();
        let outcome = server_pass(&deps, &server).await;
        job.end(started, pass_result(&outcome));
        observe::sync_pass(server.id(), "server", started.elapsed(), outcome.is_ok());
        if let Err(e) = &outcome {
            tracing::warn!(server = server.id(), error = %e, "server pass failed");
        }
        if let Err(e) = deps
            .engines
            .report(&deps.pool, &deps.journal, server.id(), "server", outcome)
            .await
        {
            tracing::error!(server = server.id(), error = %e, "the engine state could not be recorded");
        }
        job.wait(deps.config.server).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(id: &str, kind: &str, roots: &[&str]) -> Library {
        Library {
            id: id.to_string(),
            name: id.to_string(),
            kind: Some(kind.to_string()),
            integration: Some(id.to_string()),
            section_key: None,
            managed_by: None,
            locations: Vec::new(),
            root_folders: roots.iter().map(|r| r.to_string()).collect(),
            engines: vec![id.to_string()],
            titles: 0,
            copies: 0,
            size_bytes: 0,
        }
    }

    fn section(
        key: &str,
        kind: &str,
        title: &str,
        agent: &str,
        locations: &[&str],
    ) -> ServerSection {
        ServerSection {
            key: key.to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            agent: Some(agent.to_string()),
            locations: locations.iter().map(|l| l.to_string()).collect(),
        }
    }

    #[test]
    fn sections_are_mapped_to_the_arr_whose_folders_they_hold() {
        let arrs = [
            library("radarr", "movie", &["/media/movies", "/media/4kmovies"]),
            library("sonarr", "show", &["/media/tv"]),
        ];
        let movies = section_library(
            "plex",
            &section(
                "1",
                "movie",
                "Movies",
                "tv.plex.agents.movie",
                &["/4kmovies", "/movies/"],
            ),
            &arrs,
        );
        assert_eq!(
            (
                movies.id.as_str(),
                movies.kind.as_str(),
                movies.managed_by.as_deref()
            ),
            ("plex-1", "movie", Some("radarr"))
        );
        let shows = section_library(
            "plex",
            &section("2", "show", "Shows", "tv.plex.agents.series", &["/tv"]),
            &arrs,
        );
        assert_eq!(shows.managed_by.as_deref(), Some("sonarr"));
        let home = section_library(
            "plex",
            &section(
                "5",
                "movie",
                "Home Movies",
                "tv.plex.agents.none",
                &["/homemovies"],
            ),
            &arrs,
        );
        assert_eq!((home.kind.as_str(), home.managed_by), ("homevideo", None));
        // The agent before the current one marks personal media too.
        let older = section_library(
            "plex",
            &section(
                "7",
                "movie",
                "Tapes",
                "com.plexapp.agents.none",
                &["/tapes"],
            ),
            &arrs,
        );
        assert_eq!(older.kind, "homevideo");
        // A video section no manager looks after is a plain video library whatever its agent
        // is; one a manager looks after is not, and neither is music.
        assert!(plain_video(Some("homevideo"), true, false));
        assert!(plain_video(Some("homevideo"), false, false));
        assert!(plain_video(Some("movie"), true, false));
        assert!(plain_video(Some("show"), true, false));
        assert!(!plain_video(Some("movie"), true, true));
        assert!(!plain_video(Some("movie"), false, false));
        assert!(!plain_video(Some("music"), true, false));
        assert!(!plain_video(None, true, false));
        // A listing far shorter than what is stored is refused; a small or a real change is not.
        assert!(implausible_listing(4, 4700, 0, 0).is_some_and(|w| w.contains("no section")));
        assert!(implausible_listing(4, 4700, 4, 2000).is_some_and(|w| w.contains("2000 items")));
        assert!(implausible_listing(4, 4700, 3, 4600).is_none());
        assert!(implausible_listing(4, 12, 4, 0).is_none());
        assert!(implausible_listing(0, 0, 0, 0).is_none());
        assert!(implausible_listing(0, 0, 4, 4700).is_none());
        let music = section_library(
            "plex",
            &section("6", "artist", "Music", "tv.plex.agents.music", &["/music"]),
            &arrs,
        );
        assert_eq!((music.kind.as_str(), music.managed_by), ("music", None));
    }
}
