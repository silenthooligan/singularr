# Contributing to Singularr

Singularr is one workspace over a Plex library and the services around it. A change should make the
library easier to understand, or a service easier to manage, without making any of them less safe.

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
