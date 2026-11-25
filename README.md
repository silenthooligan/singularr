<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./brand/Compact/Singularr%20-%20dark.svg">
    <img alt="Singularr" width="390" src="./brand/Compact/Singularr%20-%20light.svg">
  </picture>
</p>

<p align="center"><strong>Escape container sprawl.</strong></p>
<p align="center">One self-hosted app for your whole media library, whatever server plays it.</p>
<p align="center">
<a href="#install">Install</a> ·
<a href="docs/configuration.md">Configure</a> ·
<a href="#how-it-stays-safe">Security</a> ·
<a href="docs/roadmap.md">Roadmap</a>
</p>
<p align="center">
<a href="https://github.com/silenthooligan/singularr/releases"><img src="https://img.shields.io/github/v/release/silenthooligan/singularr?include_prereleases&label=beta" alt="Latest beta"></a>
<a href="https://github.com/silenthooligan/singularr/actions/workflows/ci.yml"><img src="https://github.com/silenthooligan/singularr/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
<a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache_2.0-blue" alt="Apache 2.0 license"></a>
</p>

![A luminous violet event horizon beside the words “Escape container sprawl.”](./brand/Campaign/Singularr%20launch%20card.png)

Singularr runs a media library from end to end in one app. It finds and downloads movies and shows,
imports and names them, keeps their editions and subtitles, takes requests from the people who watch,
records what they play, and clears out what is no longer wanted. It replaces the stack of
single-purpose apps a library usually needs (Radarr, Sonarr, Seerr, Tautulli, Bazarr, Maintainerr,
Cleanuparr) with one library, one set of rules and one interface, and it explains every decision it
makes.

Singularr brings the engine and the integrations; the services are yours to choose. Media servers and
players, download clients, indexers and transcoders each plug in through one layer per kind, with an
adapter per service, and Singularr drives them through their APIs. 0.1 speaks Plex, qBittorrent,
Prowlarr and Tdarr; Jellyfin, Emby, Kodi, Transmission, Deluge, SABnzbd and NZBGet come next
([roadmap](docs/roadmap.md)).

> **Beta (0.x).** Singularr manages a real library every day. In 0.1 it still hands some jobs to the
> apps it replaces: Radarr and Sonarr search, grab and import (Singularr drives them and judges every
> release), Seerr carries out requests, and Plex is the media server. Each job moves into Singularr in
> turn ([roadmap](docs/roadmap.md)), so you can run 0.1 beside your current setup and retire each app
> as its job lands. Until 1.0 a release may change settings or behavior; the [changelog](CHANGELOG.md)
> says what changed.

## One library, two views

**Singularr** is the workspace for the people who run the library. Movies and shows have their own
views. Every title shows its files, editions and copies, and the activity queue names the title,
edition or episode, the service responsible and its current state.

**Singularr Requests** is the portal for viewers. They sign in with their media server account (Plex
in 0.1), find a movie or show and
choose exactly what they want, down to a single episode: a request for *Signal Coast* S02E03 names one
episode when the owner reviews it.

## What it does

- **Library.** Movies, shows, music and home videos from your media server's libraries, with editions as a first
  class idea: a title can hold a theatrical cut and an open matte copy side by side.
- **Activity.** The download queue with each item's stage and, when it is stuck, why; the managers'
  history; and the wanted list (missing, and below the profile's cutoff).
- **Search and grab.** An interactive search through Radarr and Sonarr that sets Singularr's own verdict
  and score beside the manager's for every release. Grab, import a download the manager refused, or
  remove and blocklist it.
- **Quality profiles.** Profiles built from grouped lists of release groups and traits instead of a
  hundred separate rules, a lab to test release names against them, and naming previews that follow
  the managers' own conventions.
- **Requests.** Requests from the portal or from Seerr, with a season limit per viewer and the owner's
  review.
- **Processing.** Tdarr's flows, workers and libraries, and rules that decide which files go to which
  flow, watched before they are trusted.
- **Statistics.** Who played what, where and how it was delivered, from the media server, with an
  import from Tautulli.
- **Notifications.** Slack, Discord, email, webhooks and more, chosen per event.
- **Journal.** Every change and decision, with who or what made it.
- **API.** Everything the interface does is in `/api/v1`, described by OpenAPI at
  `/api/v1/openapi.json`.

## Install

You need Docker with Compose, a media server (Plex in 0.1), a download client (qBittorrent in 0.1),
Radarr or Sonarr (until Singularr's own search and import land), and a reverse proxy that signs people in (oauth2-proxy, Authelia, Authentik
or similar). PostgreSQL comes with the compose file.

```bash
git clone https://github.com/silenthooligan/singularr.git
cd singularr
cp .env.example .env    # the database password, the owner, the sign-in proxy and the integrations
docker compose up -d
```

Compose pulls the beta image from `ghcr.io/silenthooligan/singularr`; `docker compose up -d --build`
builds it from this checkout instead. Then point your proxy at port 7300.
[docs/configuration.md](docs/configuration.md) describes every setting.

### Sign-in

Singularr does not sign people in itself yet (it is on the [roadmap](docs/roadmap.md)). Your proxy
signs the owner in and passes the address in `X-Auth-Request-Email`, together with a shared secret in
`X-Edge-Token`. Singularr honors those headers only from the addresses in `SINGULARR_TRUSTED_PROXY_IPS`
when the secret matches `SINGULARR_EDGE_TOKEN`, and only for an address listed in
`SINGULARR_OWNER_EMAILS`. With no trusted proxy configured, no identity header is trusted at all.

Programs use API keys instead (Settings, API), each with its own scopes.

## How it stays safe

- **Read first.** Every integration starts read only. You allow one to act in Settings, Integrations,
  and the screen lists what that allows.
- **Declared calls only.** Each adapter lists the exact calls it may send to its service. Anything
  else is refused before it leaves Singularr, whatever the access says: deleting files, shutting a
  service down, changing its settings or profiles.
- **Your clicks.** Searches, grabs, imports and removals happen when you ask for them, never from a
  background job.
- **The portal is separate.** Singularr Requests runs as its own process and database role, sees only
  views made for it, and holds no integration secret.

Found a security problem? See [SECURITY.md](SECURITY.md).

## Develop

The gate that CI runs: formatting, clippy with warnings as errors, and every test, the database-backed
ones against a throwaway PostgreSQL 18.

```bash
docker buildx build --target check --output type=cacheonly .
```

`python3 tools/blockcheck.py` checks that no common content blocker list (uBlock Origin, AdGuard and
Brave's defaults) blocks one of the client's own requests. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Identity

The name is **Singularr**, with two final r's, and the viewer portal is **Singularr Requests**. Logos,
icons, colors and type are in [brand/](brand/README.md).

## License

Singularr is open source under [Apache 2.0](LICENSE). The interface fonts, DM Sans and IBM Plex Mono,
are under the SIL Open Font License; see [NOTICE](NOTICE).
