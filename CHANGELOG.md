# Changelog

Versions follow semantic versioning. Releases before 1.0 are betas. A minor release before 1.0 may
change settings or behavior.

## 0.1.3 (beta)

- Job runtime in PostgreSQL: schedules, jobs under leases, markers. Every background pass runs on it. Last runs and results survive a restart. A failing schedule backs off five intervals at most.
- Write access per action of an integration: `<TYPE>_ACCESS` takes `read`, `act` or a list of actions. Integrations show partial write access.
- Health per integration and pass.
- Outbound calls: reads retried with backoff, `Retry-After` honored, a limit of calls in flight per host, a circuit breaker, a size limit per answer.
- Processing rules send a file at most once. A send with no clear answer shows as Unconfirmed.
- Plays: one play per item in a session (autoplay, the next track). A failed reading of the sessions is tried again after 10 s while something plays.
- Download ownership and Radarr's custom formats kept across restarts. A grab history read stops at the records it has.
- Image: `singularrd health` as the health check; ffmpeg and ffprobe (LGPL build, the source as a release of this repository). Compose: Meilisearch as the search.
- Singularr Requests: the portal's database role created from `SINGULARR_REQUESTS_DB_PASSWORD`.
- Crates `singularr-sdk` (connector kinds, manifests, the HTTP layer) and `singularr-platform` (database, journal, job runtime, settings, access).
- Migrations 0022 to 0028. An older build does not start on a database that ran them: restore a backup to go back.

## 0.1.2 (beta)

- Statistics: Overview, History, Users, Libraries, Players, Graphs; shared filters (dates, kind, library, person, media server, player, app, platform, delivery); History sorts every column; a drawer per play.
- Plays record media server and kind, player app and platform with versions, device id, client address, secure and relayed (Plex), bandwidth, source and sent codecs, channels and container, transcode reasons (Jellyfin, Emby).
- Addresses kept by default (Settings, Jobs, Play history, Keep IP addresses); owner only; never logged. Turning the setting off removes them at once. The first full Tautulli read refills past plays' addresses.
- Locations from DB-IP IP to City Lite, looked up on the server. Monthly download on by default (`SINGULARR_GEOIP_DOWNLOAD=off` turns it off); `SINGULARR_GEOIP_PATH` for an operator's file. Without a database: addresses only.
- Settings: `SINGULARR_GEOIP_DOWNLOAD`, `SINGULARR_GEOIP_PATH`, `SINGULARR_DATA_DIR` (default `/data`). Compose: volume `data` at `/data`.
- Migration 0021 (players and streams), backfilled from Tautulli history.

## 0.1.1 (beta)

- Media server layer: Plex, Jellyfin and Emby behind one port; read only; plays recorded from each server's sessions. Emby untested against a real server.
- Download client layer: qBittorrent, Transmission, Deluge, SABnzbd and NZBGet behind one port; read only; usenet jobs in Activity and matched to the managers' queues.
- Settings: `JELLYFIN_*`, `EMBY_*`, `TRANSMISSION_*`, `DELUGE_*`, `SABNZBD_*`, `NZBGET_*`; `SINGULARR_SYNC_DOWNLOADS_SECS` (the older `SINGULARR_SYNC_QBIT_SECS` still works).
- Outbound allowlist: rules per origin and base path; GETs restricted per adapter; strict JSON bodies.
- Singularr Requests refuses to start with any variable named like a secret (`*_API_KEY`, `*_TOKEN`, `*_PASS`).
- Migrations 0019 (download clients) and 0020 (open plays).

## 0.1.0 (beta)

First public release.

- Library: movies, shows, music and home videos; editions and copies; title pages with files, cast and
  crew; calendar of air and release dates.
- Activity: download queue with stage and stuck reason; manager history; wanted list.
- Search and grab through Radarr and Sonarr: Singularr verdict and score beside the manager's; grab;
  import a refused download; remove and blocklist.
- Quality profiles: grouped lists of release groups and traits; release lab; naming previews in the
  managers' conventions.
- Singularr Requests: viewer portal; Plex sign-in; requests down to a single episode.
- Processing: Tdarr flows, workers and libraries; rules that pick files for a flow.
- Statistics: plays from Plex; Tautulli import.
- Notifications per event: Slack, Discord, email, webhooks, ntfy, Telegram, Pushover, Gotify.
- Journal: every change and decision with its actor.
- Access: integrations read only until write access is granted; each adapter sends only its declared
  calls.
