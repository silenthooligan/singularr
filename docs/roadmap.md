# Roadmap

The aim: one management tool for a media library, whatever media server plays it (Plex, Jellyfin, Emby,
Kodi), in place of a pipeline of single-purpose containers.

## Principles

- **Built in** when it is logic over the library: profiles and naming, requests, statistics, subtitles,
  retention, download cleanup, notifications, rules for processing.
- **Its own container, integrated natively** when it is a heavy engine with its own life: the
  transcoder (Tdarr), the indexer manager (Prowlarr), the download client. Singularr shows their state,
  configures them through their API where they have one, and uses them, so daily work never needs
  their own screens.
- **Read first, then act by permission.** Every integration starts read only. A feature that changes a
  service works once that integration is given write access in Settings, Integrations, and each kind
  of change is listed there.
- **Any media server.** Everything about playback goes through one port (libraries, items, sessions,
  history, collections, scans), with an adapter per server.
- **One interface language.** [docs/copy.md](copy.md).

## What Singularr takes over

Singularr replaces the apps that manage a library. Each job moves in on its own, so an existing setup can
keep running beside Singularr and retire one app at a time.

| Job | Done today by | In 0.1 (beta) | In Singularr |
|---|---|---|---|
| The library: titles, editions, copies | Radarr, Sonarr | read from the managers and the media server | Singularr's own catalog (stage 3) |
| Quality profiles, lists and naming ([profiles.md](profiles.md)) | custom formats, profiles, naming screens | Singularr's rules judge every release; the managers still apply their own | the only rules (stage 3) |
| Search and grab | Radarr, Sonarr | through the managers, with Singularr's verdict beside theirs | through your indexers and download client directly (stage 2) |
| Import, renaming and upgrades | Radarr, Sonarr, import scripts | the managers | Singularr places, names, verifies and upgrades files (stage 2) |
| Requests | Seerr, Jellyseerr | Singularr Requests in front, Seerr behind | Singularr carries them out (stage 3) |
| Play statistics | Tautulli | live: from Plex, with a Tautulli import | recorded by Singularr from any media server's sessions |
| Notifications | Notifiarr, Apprise | live | live |
| Processing rules | import scripts, Tdarr's own screens | live; Tdarr stays the transcoder | live |
| Subtitles | Bazarr | | stage 4 |
| Retention | Maintainerr | | stage 4 |
| Download cleanup | Cleanuparr | reported | acted on (stage 4) |
| Collections and gaps | Gaps | | stage 4 |
| Built-in sign-in and first-run setup | an authenticating proxy | | before 1.0 |

## Any service, one layer per kind

Singularr brings the engine (the library, the rules, the decisions) and the integrations. The services
are yours to choose: each kind plugs in through one layer, with an adapter per service, and Singularr
drives it through its API, so daily work never needs that service's own screens.

| Kind | 0.1 (beta) | Next |
|---|---|---|
| Media servers and players | Plex: libraries, items, sessions, plays, images, viewer sign-in | Jellyfin and Emby (the two APIs share a lineage), Kodi as a player |
| Download clients | qBittorrent | Transmission, Deluge, SABnzbd, NZBGet |
| Indexers | through the managers | Prowlarr, Jackett, and Torznab or Newznab indexers directly (stage 2) |
| Transcoders | Tdarr: flows, workers, libraries, rules | others through the same layer |
| Requests engine | Seerr | Singularr itself (stage 3) |
| Metadata | TMDB, TVDB | |

Play statistics do not depend on a server's own history: Singularr records plays from each media
server's sessions, the way Tautulli does for Plex, so they work the same on every server.

## What each replaced program does that must not be lost

- **Tautulli:** history per person with pauses and percent watched, delivery (direct play, direct
  stream, transcode) per stream, platforms and players, home network against outside, concurrent
  streams, graphs by day, hour and weekday, newsletters and notifications on playback.
- **Bazarr:** wanted subtitles per file and language, providers with scores, a minimum score, forced and
  hearing-impaired handling, sync to the audio, upgrades of earlier subtitles, a blacklist.
- **Maintainerr:** rules over plays, age, ratings and requester, a collection in the media server that
  shows what is about to leave, a grace period, exclusions, and what happens on removal.
- **Cleanuparr:** strikes for stalled, slow, failed-import and metadata-stuck downloads, detection of fake
  or unwanted files in a torrent, removal with or without blocklisting, a new search after removal,
  seeding rules by tracker.
