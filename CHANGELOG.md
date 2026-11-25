# Changelog

Versions follow semantic versioning. Releases before 1.0 are betas. A minor release before 1.0 may
change settings or behavior.

## Unreleased

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
