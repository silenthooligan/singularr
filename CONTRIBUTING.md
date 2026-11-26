# Contributing to Singularr

Singularr is a self-hosted media library manager. It is built to replace Radarr, Sonarr, Seerr,
Tautulli, Bazarr, Maintainerr and Cleanuparr with one application. The [roadmap](docs/roadmap.md) lists
the work. The [project board](https://github.com/users/silenthooligan/projects/2) tracks it. Issues
labeled help wanted are open to anyone. Open an issue or a
[discussion](https://github.com/silenthooligan/singularr/discussions) before a large change.

## Open work

- **Adapters:** media servers and players (Jellyfin, Emby, Kodi), download clients (Transmission,
  Deluge, SABnzbd, NZBGet), indexers (Prowlarr, Jackett, Torznab and Newznab) and transcoders. The
  roadmap shows which layers exist.
- **Native jobs:** search and grab through indexers and download clients, import and renaming,
  subtitles, retention, download cleanup, built-in sign-in and first-run setup.
- **Testing:** run the beta beside an existing setup and report what it gets wrong: a release scored
  oddly, a misread file, a slow or unclear page.
- **Docs:** guides for other media servers and proxies. Interface text that follows
  [docs/copy.md](docs/copy.md).

## Bug reports

Open an [issue](https://github.com/silenthooligan/singularr/issues/new/choose) with the version
(Settings, System), the integrations in use, the steps taken and the result. Include the status, the
problem's `code` and the time of a failed request. Remove keys, tokens and addresses from logs before
posting. Report security problems privately ([SECURITY.md](SECURITY.md)).

## Development setup

The [CI workflow](.github/workflows/ci.yml) is the reference. Everything builds in Docker:

```bash
git clone https://github.com/silenthooligan/singularr.git
cd singularr
docker buildx build --target check --output type=cacheonly .   # fmt, clippy, every test
docker build -t singularr .                                     # the image
```

A build outside Docker needs the nightly toolchain named in `rust-toolchain.toml`. The database-backed
tests need PostgreSQL 18 (`tools/gate.sh` starts a throwaway one).

## Code map

| Area | Directory |
|---|---|
| Release parsing, the policy and its evaluation (no I/O) | `crates/singularr-core/` |
| The connector SDK: the way out's types, the declared calls, the rules, the kinds of engine and their items (no I/O) | `crates/singularr-sdk/` |
| The platform: the bootstrap, the database and the migration runner, the journal, callers and API keys, the HTTP sender, mail, ffmpeg runs, the settings mechanism, the access per action | `crates/singularr-platform/` |
| Server core: configuration, the migrations, the location database. The platform's modules keep their paths here | `app/src/core/` |
| Adapters, one per service, and one manifest per type (`manifests.rs`) | `app/src/engines/` |
| Routes under `/api/v1` | `app/src/api/` |
| Search, grab and import through the managers | `app/src/acquire/` |
| Web client (plain JS and CSS, no build step) | `web/` |
| Default policy and editions | `rules/` |

## Conventions

- **Outbound calls:** each adapter lists the calls it may send (`Calls` in `crates/singularr-sdk/src/http.rs`).
  A new call needs a test that passes it in its declared form and refuses it in any other form.
- **Interface text:** follows [docs/copy.md](docs/copy.md). Controls have labels. The interface never
  addresses the person.
- **Routes:** a new route must pass the blocker lists (`python3 tools/blockcheck.py`).
- **Tests:** every change needs tests against recorded answers in `app/tests/fixtures`. No test calls a
  real service.

Open pull requests against `main`. Keep each one small and focused.
