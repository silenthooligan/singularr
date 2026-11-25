# Roadmap

Singularr is built to manage a media library as one application in place of a set of single-purpose
apps. It is designed for any media server (Plex, Jellyfin, Emby, Kodi) and any download client.

## Principles

- **Built in:** logic over the library. Profiles and naming, requests, statistics, subtitles,
  retention, download cleanup, notifications and processing rules.
- **Integrated:** engines that run as their own services. Media servers, download clients, the indexer
  manager (Prowlarr) and the transcoder (Tdarr). Singularr reads their state and configures and drives
  them through their APIs. Daily work does not need their own interfaces.
- **Access:** every integration starts with read access. A feature that changes a service needs write
  access for that integration (Settings, Integrations). That page lists each kind of change.
- **Adapters:** one layer per kind of service and one adapter per service.
- **Interface text:** [docs/copy.md](copy.md).

## Jobs

Singularr takes over each job separately. Run Singularr beside an existing setup and retire one app at
a time.

| Job | Done today by | 0.1 (beta) | Planned |
|---|---|---|---|
| Library: titles, editions, copies | Radarr, Sonarr | read from the managers and the media server | Singularr's own catalog (stage 3) |
| Quality profiles, lists and naming ([profiles.md](profiles.md)) | custom formats, profiles, naming screens | Singularr's rules score every release; the managers apply their own | Singularr's rules only (stage 3) |
| Search and grab | Radarr, Sonarr | through the managers, with Singularr's verdict beside theirs | through indexers and download clients directly (stage 2) |
| Import, renaming and upgrades | Radarr, Sonarr, import scripts | the managers | Singularr places, names, verifies and upgrades files (stage 2) |
| Requests | Seerr, Jellyseerr | Singularr Requests in front of Seerr | Singularr fulfills them (stage 3) |
| Play statistics | Tautulli | from Plex, with a Tautulli import | recorded from each media server's sessions |
| Notifications | Notifiarr, Apprise | in 0.1 | |
| Processing rules | import scripts, Tdarr's own screens | in 0.1; Tdarr remains the transcoder | |
| Subtitles | Bazarr | | stage 4 |
| Retention | Maintainerr | | stage 4 |
| Download cleanup | Cleanuparr | reported | acted on (stage 4) |
| Collections and gaps | Gaps | | stage 4 |
| Sign-in and first-run setup | an authenticating proxy | | built in (before 1.0) |

## Services

| Kind | 0.1 (beta) | Next |
|---|---|---|
| Media servers and players | Plex: libraries, items, sessions, plays, images, viewer sign-in | on main: Jellyfin and Emby (read only; Emby untested against a real server); then Kodi as a player, viewer sign-in for Jellyfin and Emby |
| Download clients | qBittorrent | on main: Transmission, Deluge, SABnzbd, NZBGet (read only) |
| Indexers | through the managers | Prowlarr, Jackett, Torznab and Newznab (stage 2) |
| Transcoders | Tdarr: flows, workers, libraries, rules | others on the same layer |
| Requests | Seerr | Singularr (stage 3) |
| Metadata | TMDB, TVDB | |

## Features to match

- **Tautulli:** history per person with pauses and percent watched; delivery per stream (direct play,
  direct stream, transcode); platforms and players; home network or outside; concurrent streams;
  graphs by day, hour and weekday; newsletters and notifications on playback.
- **Bazarr:** wanted subtitles per file and language; providers with scores and a minimum score; forced
  and hearing-impaired handling; sync to the audio; upgrades of earlier subtitles; a blocklist.
- **Maintainerr:** rules over plays, age, ratings and requester; a media server collection of the
  titles about to leave; a grace period; exclusions; the action on removal.
- **Cleanuparr:** strikes for stalled, slow, failed-import and metadata-stuck downloads; detection of
  fake or unwanted files in a torrent; removal with or without a blocklist entry; a new search after
  removal; seeding rules by tracker.
