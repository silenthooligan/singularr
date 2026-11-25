# Contributing to Singularr

Singularr is one workspace over a Plex library and the services around it. A change should make the
library easier to understand, or a service easier to manage, without making any of them less safe.

Singularr is a work in progress, built in the open: the [roadmap](docs/roadmap.md) is the plan, and
every part of it is open to help. Before a large change, open an issue or a
[discussion](https://github.com/silenthooligan/singularr/discussions) so we can agree on its shape first.

## Where help is wanted

- **Adapters.** Every kind of service plugs in through one layer with an adapter per service. Media
  servers and players (Jellyfin, Emby, Kodi), download clients (Transmission, Deluge, SABnzbd, NZBGet),
  indexers (Prowlarr, Jackett, Torznab and Newznab directly) and transcoders are each a self-contained
  piece of work once their layer is in place; the roadmap says which layers are.
- **Native jobs.** Search and grab through your indexers and download client, import and renaming,
  subtitles, retention rules, download cleanup, built-in sign-in and a first-run setup.
- **Your setup.** Run the beta beside your current stack and report what it gets wrong: a release it
  scores oddly, a file it misreads, a page that is slow or unclear.
- **Docs and words.** Guides for other servers and proxies, and interface text that follows
  [docs/copy.md](docs/copy.md).

## Report a problem

Open an [issue](https://github.com/silenthooligan/singularr/issues/new/choose). Include the version
(Settings, System), which integrations you use, what you did and what happened. For a failed request,
include the status, the problem's `code` and the time. Check logs for keys, tokens and addresses
before posting. Report security problems through the [private process](SECURITY.md).

## Development setup

The [CI workflow](.github/workflows/ci.yml) is the reference. Everything builds in Docker:

```bash
git clone https://github.com/silenthooligan/singularr.git
cd singularr
docker buildx build --target check --output type=cacheonly .   # fmt, clippy, every test
docker build -t singularr .                                     # the image
```

Outside Docker you need the nightly toolchain that `rust-toolchain.toml` names and, for the
database-backed tests, PostgreSQL 18 (`tools/gate.sh` starts a throwaway one).

## Find the code

| Area | Directory |
|---|---|
| Release parsing, the policy and its evaluation (no I/O) | `crates/singularr-core/` |
| The server: configuration, the journal, the outbound allowlist | `app/src/core/` |
| One adapter per service (Plex, the managers, Tdarr, Seerr, Tautulli) | `app/src/engines/` |
| The routes under `/api/v1` | `app/src/api/` |
| Search, grab and import through the managers | `app/src/acquire/` |
| The web client (plain JS and CSS, no build step) | `web/` |
| The default policy and the editions | `rules/` |

## How changes are made

- **Every call to a service is declared.** An adapter lists what it may send (`Calls` in
  `app/src/core/outbound.rs`); a new call comes with a test that it passes in its declared form and is
  refused in any other.
- **Words follow [docs/copy.md](docs/copy.md):** labels, not sentences, and the interface never
  addresses the person.
- **A new route passes the blocker lists:** `python3 tools/blockcheck.py`.
- **Tests come with the change**, against recorded answers in `app/tests/fixtures`: no test calls a
  real service.

Open a pull request against `main`. Small, focused changes are easiest to review.
