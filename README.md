<p align="center">
  <img alt="Singularr: self-hosted media library management for any media server and download client. Beta 0.1, work in progress." src="./brand/Campaign/Singularr%20launch%20card.png">
</p>

<p align="center">
<a href="#install">Install</a> ·
<a href="docs/configuration.md">Configuration</a> ·
<a href="docs/roadmap.md">Roadmap</a> ·
<a href="https://github.com/users/silenthooligan/projects/2">Project board</a> ·
<a href="CONTRIBUTING.md">Contributing</a> ·
<a href="https://singularr.org">singularr.org</a>
</p>
<p align="center">
<a href="https://github.com/silenthooligan/singularr/releases"><img src="https://img.shields.io/github/v/release/silenthooligan/singularr?include_prereleases&label=beta" alt="Latest beta"></a>
<a href="https://github.com/silenthooligan/singularr/actions/workflows/ci.yml"><img src="https://github.com/silenthooligan/singularr/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
<a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache_2.0-blue" alt="Apache 2.0 license"></a>
<a href="docs/roadmap.md"><img src="https://img.shields.io/badge/status-work_in_progress-BBA4FD" alt="Work in progress"></a>
<a href="CONTRIBUTING.md"><img src="https://img.shields.io/badge/contributions-welcome-5B3D92" alt="Contributions welcome"></a>
</p>

Singularr is a self-hosted media library manager. It is built to replace the separate apps that manage
a library today: Radarr, Sonarr, Seerr, Tautulli, Bazarr, Maintainerr and Cleanuparr. One application
covers the library, the quality rules and the interface. Each release verdict lists the rules behind it.

Singularr connects to services by kind: media servers, download clients, indexers and transcoders.
Each kind has one layer. Each service has an adapter on its layer. Version 0.1.1 has adapters for
Plex, Jellyfin and Emby (media servers), qBittorrent, Transmission, Deluge, SABnzbd and NZBGet
(download clients) and Tdarr (transcoder). Kodi is next.

## Status

Singularr 0.1 is a beta and a work in progress. Version 0.1 still searches, grabs and
imports through Radarr and Sonarr. Seerr still fulfills requests. Singularr scores every release
itself and drives those apps through their APIs. It takes over each of those jobs in turn. Run 0.1
beside an existing setup and retire each app when Singularr takes over its job.

The [roadmap](docs/roadmap.md) lists what works today and what comes next. The
[project board](https://github.com/users/silenthooligan/projects/2) tracks each item. The work is grouped
in two [milestones](https://github.com/silenthooligan/singularr/milestones): MVP, then Product launch. A release before
1.0 may change settings or behavior. The [changelog](CHANGELOG.md) lists the changes.

## Contributing

Contributions are welcome. An adapter for another media server or download client is self-contained
work. Every job on the roadmap is open: native search and import, subtitles, retention, download
cleanup and built-in sign-in. Bug reports from other setups are welcome too.
[CONTRIBUTING.md](CONTRIBUTING.md) covers setup and conventions. Use
[Discussions](https://github.com/silenthooligan/singularr/discussions) for ideas and questions.

## Workspace and portal

**Singularr** is the workspace for the people who run the library. Movies and shows have separate
views. Each title lists its files, editions and copies. The activity queue shows each item's title,
edition or episode, the service handling it and its state.

**Singularr Requests** is the portal for viewers. Viewers sign in with their Plex account and request a movie, a
season or a single episode. Sign-in with Jellyfin and Emby accounts is on the roadmap.

## Features

- **Library:** movies, shows, music and home videos. Editions of one title (a theatrical cut and an
  open matte copy) appear side by side.
- **Activity:** the download queue with each item's stage and the reason an item is stuck; the
  managers' history; the wanted list (missing titles and titles below the profile's cutoff).
- **Search and grab:** interactive search through Radarr and Sonarr. Each release shows Singularr's
  verdict and score next to the manager's. Grab a release, import a download the manager refused, or
  remove a download and blocklist it.
- **Quality profiles:** profiles built from grouped lists of release groups and traits. A lab tests
  release names. Naming previews follow the managers' conventions.
- **Requests:** requests from the portal and from Seerr in one list. Each viewer may have a limited
  number of seasons of a show at a time.
- **Processing:** Tdarr's flows, workers and libraries, and rules that pick the files for a flow.
- **Statistics:** every play of every media server, from its sessions, a Plex server's history or
  Tautulli's. Overview, History, Users, Libraries, Players and Graphs, each with the same filters.
  Each play names the media server, the player app and platform with their versions, the device,
  the client's address and its location, and how the stream was delivered (direct play, direct
  stream or transcode, the file and the stream sent, and why the server transcoded). Locations are
  looked up on the server in DB-IP's IP to City Lite database, downloaded once a month unless the
  owner turns that off. IP geolocation by [DB-IP](https://db-ip.com).
- **Calendar:** episodes and movies by air and release date.
- **Notifications:** a choice of channels per event: Slack, Discord, email, webhooks, ntfy, Telegram,
  Pushover and Gotify.
- **Journal:** every change and decision and the person or job behind it.
- **API:** every action in the interface is available under `/api/v1`. The OpenAPI description is at
  `/api/v1/openapi.json`.

## Install

Requirements: Docker with Compose, a media server (Plex, Jellyfin or Emby), a download client
(qBittorrent, Transmission, Deluge, SABnzbd or NZBGet), Radarr or Sonarr (until native search and import), and a reverse proxy that signs people in
(oauth2-proxy, Authelia, Authentik or similar). PostgreSQL and Meilisearch (the search) are part of
the compose file.

```bash
git clone https://github.com/silenthooligan/singularr.git
cd singularr
cp .env.example .env    # database password, search key, owner, sign-in proxy, integrations
docker compose up -d
```

Compose pulls the beta image from `ghcr.io/silenthooligan/singularr`. `docker compose up -d --build`
builds from the checkout instead. Point the proxy at port 7300. The volume `data` (at `/data`) holds
the files Singularr downloads: the location database, once a month unless turned off. The volume
`search` holds Meilisearch's index. Singularr writes it again when it is lost.
[docs/configuration.md](docs/configuration.md) describes every setting.

### Sign-in

Built-in sign-in is on the roadmap. Until then, a reverse proxy signs the owner in and passes the
owner's address in `X-Auth-Request-Email`. Singularr accepts that header only when:

- the request comes from an address in `SINGULARR_TRUSTED_PROXY_IPS`;
- `X-Edge-Token` matches `SINGULARR_EDGE_TOKEN` (when that is set);
- the address is in `SINGULARR_OWNER_EMAILS`.

Singularr trusts no identity header while `SINGULARR_TRUSTED_PROXY_IPS` is empty. Programs use API keys
from Settings, API. Each key has its own scopes.

## Security

- Every action of an integration starts with read access. The owner grants write access per action
  in Settings, Integrations. Each action has its own switch there.
- Each adapter declares the exact calls it may send. Singularr refuses every other call before it is
  sent. No adapter declares a call that deletes or edits a title or its files, shuts down or restarts
  a service, or changes a service's settings or quality profiles.
- Searches, grabs, imports and removals run only when the owner starts them. No background job starts
  them.
- Singularr Requests runs as its own process with its own database role. It reads only the views made
  for it and has no integration secrets.
- The players' addresses in Statistics are the owner's alone: no API key reads them and Singularr
  Requests cannot reach them. No address is written to the log or sent anywhere to be placed.

Report security problems privately as described in [SECURITY.md](SECURITY.md).

## Development

CI runs formatting, clippy with warnings as errors, and every test. Database-backed tests run against a
throwaway PostgreSQL 18.

```bash
docker buildx build --target check --output type=cacheonly .
```

`python3 tools/blockcheck.py` checks that the default lists of uBlock Origin, AdGuard and Brave block
none of the client's own requests. [CONTRIBUTING.md](CONTRIBUTING.md) has the rest.

## Brand

The application is **Singularr**. The viewer portal is **Singularr Requests**. Logos, icons, colors and
type are in [brand/](brand/README.md).

## License

Apache 2.0 ([LICENSE](LICENSE)). The interface fonts, DM Sans and IBM Plex Mono, are licensed under the
SIL Open Font License ([NOTICE](NOTICE)).
