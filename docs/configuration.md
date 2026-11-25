# Configuration

Singularr reads its settings from the environment. With Docker Compose they are in `.env` (start from
`.env.example`). An empty value counts as unset. An integration without its key is skipped with a
warning. Settings that have a place on the Settings page are saved there when the environment leaves
them unset.

## The server

| Setting | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | required | the PostgreSQL database (PostgreSQL 18 is the tested version). Migrations are built in and run at start |
| `SINGULARR_DB_MAX_CONNECTIONS` | 10 | pool size |
| `SINGULARR_LISTEN` | `0.0.0.0:3000` | the address the server listens on |
| `SINGULARR_FACE` | `owner` | `requests` starts Singularr Requests, the viewer portal (below) |
| `SINGULARR_WORKSPACE_NAME`, `SINGULARR_PORTAL_NAME` | `Singularr`, `Singularr Requests` | the names shown in the workspace and the portal |
| `SINGULARR_OWNER_URL` | unset | the workspace's public address, linked for the owner on the portal |
| `SINGULARR_REQUESTS_PUBLIC_URL` | unset | the portal's public address, linked from the workspace |
| `SINGULARR_SIGN_OUT_URL` | unset | the target of the workspace's Sign out (the sign-in proxy's sign-out address) |
| `SINGULARR_INSTANCE` | unset | an instance file (`singularr.toml`) with workspace names and integrations beyond the environment's (below) |
| `RUST_LOG` | `info` | log filter |
| `TZ` | `UTC` | time zone of the container |

## Sign-in

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_OWNER_EMAILS` | unset | comma-separated addresses of the owner |
| `SINGULARR_TRUSTED_PROXY_IPS` | unset | the addresses of the sign-in proxy. Identity headers from any other peer are ignored. With none set, no identity header is trusted |
| `SINGULARR_EDGE_TOKEN` | unset | a shared secret the proxy sends as `X-Edge-Token` with every request |

The proxy signs the person in and passes the address in `X-Auth-Request-Email`. That is
oauth2-proxy's header. Authelia, Authentik and Caddy's `forward_auth` can set it too. Programs use API
keys from Settings, API, sent as `Authorization: Bearer <key>`. The scopes are `read`, `library`,
`acquire`, `settings` and `admin`.

## Integrations

Each integration is read only until the owner grants write access in Settings, Integrations. Each one
sends only the calls its adapter declares. No adapter declares a call that deletes or edits a title or
its files, shuts down or restarts a service, or changes a service's settings.

| Setting | Meaning |
|---|---|
| `PLEX_URL`, `PLEX_TOKEN` | the Plex server: libraries, ratings, plays, music, sessions and images. Read only |
| `RADARR_URL`, `RADARR_API_KEY`, `RADARR_NAME` | Radarr (id `radarr`). With write access: search, grab, import a refused download, remove a download |
| `SONARR_URL`, `SONARR_API_KEY`, `SONARR_NAME` | Sonarr (id `sonarr`), the same as Radarr |
| `QBIT_URL`, `QBIT_USER`, `QBIT_PASS` | qBittorrent (id `qbittorrent`). Without a user and password Singularr does not sign in |
| `SEERR_URL`, `SEERR_API_KEY` | Seerr (id `seerr`): requests and their accounts |
| `TAUTULLI_URL`, `TAUTULLI_API_KEY` | Tautulli (id `tautulli`): its play history imports into Statistics |
| `TDARR_URL`, `TDARR_API_KEY` | Tdarr (id `tdarr`): nodes, workers, libraries, flows and reports. With write access: worker limits, pause, cancel, requeue, scan, and a library's flow |
| `PROWLARR_URL`, `PROWLARR_API_KEY` | listed as an integration. Not read yet |
| `<TYPE>_ACCESS` | `read` or `act` for that integration, set by the environment. Without it, the switch in Settings, Integrations decides |

A setup with more than one integration of a type (a second Radarr for 4K, for example) lists its
integrations in the instance file named by `SINGULARR_INSTANCE`:

```toml
[workspace]
name = "Singularr"
portal = "Singularr Requests"
url = "https://singularr.example.org"
portal_url = "https://requests.example.org"

[[integration]]
id = "radarr-4k"             # a-z, 0-9 and -, unique; also the id of its library
type = "radarr"              # plex, radarr, sonarr, qbittorrent, prowlarr, tdarr, seerr, tautulli
name = "Radarr 4K"
url = "http://radarr-4k:7878"
secret = "RADARR_4K_API_KEY" # the variable with the key (default <ID>_API_KEY)
```

The file contains no secrets: `secret` names the environment variable. Unknown keys and types stop the
start.

## Metadata and search

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_TMDB_TOKEN` | unset | a TMDB API read token |
| `SINGULARR_TVDB_KEY` | unset | a TVDB v4 API key |
| `SINGULARR_WATCH_REGION` | `US` | the region of release dates, certifications and where to watch |
| `SINGULARR_SEARCH_URL`, `SINGULARR_SEARCH_KEY` | unset | a Meilisearch for search, used when both are set. Otherwise search is built in (PostgreSQL, typo tolerant with the `pg_trgm` extension) |
| `SINGULARR_SEARCH_PREFIX` | `singularr_` | the start of every Meilisearch index name |
| `SINGULARR_PLEX_WEB_URL` | `https://app.plex.tv/desktop` | Plex's web app, for a title's Plex link |

## Policy and editions

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_POLICY` | `/app/rules/policy.toml` | the policy file. A missing file falls back to the built-in policy with a warning. An invalid file stops the start |
| `SINGULARR_POLICY_SOURCE` | `file` when `SINGULARR_POLICY` is set, else `saved` | the source of the policy in force: the file (read only in Rules, with Import) or the saved quality profiles and lists |
| `SINGULARR_EDITIONS` | `/app/rules/editions.toml` | the list of editions |
| `SINGULARR_MOVIE_ROOTS`, `SINGULARR_MOVIE_SCAN_SECS` | unset, 21600 | movie folders mounted read only at Radarr's paths. Singularr scans them for files Radarr does not track |

## Requests

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_REQUEST_OWNERS` | `1` | the owner's Seerr user ids, exempt from the season limit |
| `SINGULARR_REQUEST_MAX_SEASONS` | 2 | the number of seasons of a show a viewer may have at a time |
| `SINGULARR_REQUEST_SEASON_GATE` | on | `off` turns the season limit off |

## Folders

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_MUSIC_ROOT`, `SINGULARR_MUSIC_SCAN_SECS` | `/media/music`, 21600 | the music folder in the container and its scan interval |
| `SINGULARR_HOME_MOVIES_ROOT`, `SINGULARR_HOME_MOVIES_SCAN_SECS` | `/media/homemovies`, 21600 | the home video folder and its scan interval |

## Sync intervals

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_SYNC_ENABLED` | true | `false` serves the stored data and reads no service |
| `SINGULARR_SYNC_TITLES_SECS` | 900 | movie and series lists |
| `SINGULARR_SYNC_FILES_FULL_SECS` | 21600 | a full pass over every show's episode files |
| `SINGULARR_SYNC_FILES_CONCURRENCY` | 2 | shows read at once (1 to 4) |
| `SINGULARR_SYNC_QUEUE_SECS` | 60 | the managers' queues |
| `SINGULARR_SYNC_QBIT_SECS` | 30 | qBittorrent |
| `SINGULARR_SYNC_HISTORY_SECS` | 900 | the newest page of each manager's grab history |
| `SINGULARR_SYNC_HISTORY_MAX_PAGES` | 40 | pages of history read at the first start |
| `SINGULARR_SYNC_REQUESTS_SECS` | 300 | Seerr's requests |
| `SINGULARR_SYNC_SERVER_SECS` | 3600 | the Plex server |
| `SINGULARR_SYNC_PROCESSING_SECS` | 60 | the processing services |
| `SINGULARR_SYNC_JITTER_SECS` | 20 | a random delay before each loop's first pass |
| `SINGULARR_HTTP_TIMEOUT_SECS` | 30 | the time limit of an outbound call |

## Singularr Requests

The portal (`SINGULARR_FACE=requests`) runs as its own process with its own database role. It refuses
to start when a setting of the full process is present: `DATABASE_URL`, `SINGULARR_INSTANCE`, an
integration secret, `SINGULARR_TVDB_KEY`, `SINGULARR_SEARCH_KEY`, `SINGULARR_EDGE_TOKEN` or
`SINGULARR_OWNER_EMAILS`.

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_REQUESTS_DATABASE_URL` | required | the login of the role `singularr_requests` (create it with `deploy/requests-role.sql`) |
| `SINGULARR_REQUESTS_SESSION_SECRET` | required | at least 32 characters. Signs the Plex sign-in cookie |
| `SINGULARR_REQUESTS_PUBLIC_URL` | required | the portal's own address. Plex returns the sign-in to it |
| `SINGULARR_TMDB_TOKEN` | unset | for search and title pages |
| `SINGULARR_REQUESTS_DB_MAX_CONNECTIONS` | 5 | pool size |
