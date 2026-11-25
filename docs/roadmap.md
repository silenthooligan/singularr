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

## Where it stands

| What | Stands in for | State |
|---|---|---|
| Answers that travel: compressed, read in parts, at addresses no content blocker blocks | | live; paging and pushed changes to come |
| Notifications: Slack, Discord, email, webhooks and more, per event | Notifiarr, Apprise | live |
| Play statistics: who played what, where and how; import from Tautulli | Tautulli | live |
| Quality profiles, lists and naming ([profiles.md](profiles.md)) | the managers' profile, custom format and naming screens | live; applying them to the managers is next |
| Processing: flows, workers and libraries, and rules that send files to a flow | Tdarr's own screens for daily work | live |
| Interactive search and grab, import a refused download, remove a download | the managers' search and queue screens | live, through the managers |
| Built-in sign-in and a first-run setup | an authenticating proxy | before 1.0 |
| Subtitles: wanted, providers, download, sync | Bazarr | planned |
| Retention: rules by plays, age and requester, with a grace period | Maintainerr | planned |
| Download cleanup: strikes for stalled, failed and fake downloads | Cleanuparr | planned |
| Media servers: Jellyfin, Emby, Kodi | | planned |
| Singularr's own search, grab and import | Radarr, Sonarr | later |

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
