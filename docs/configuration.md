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
| `SINGULARR_REQUESTS_DB_PASSWORD` | unset | the password of the portal's database role, letters and digits only: the compose file puts it into the portal's database address as it is (`openssl rand -hex 32`). Set, each start creates the role when it is missing and gives it the grants of `deploy/requests-role.sql` again. The password reaches PostgreSQL as a SCRAM-SHA-256 verifier. No statement holds the password itself. Creating the role needs CREATEROLE (the compose file's database user has it). Without it the start logs one warning naming the script and goes on. Unset, the role is left as it is |
| `SINGULARR_REQUESTS_DB_ROLE` | `singularr_requests` | the portal role's name: lowercase letters, digits and `_`. The compose file uses it in the portal's database address too. An existing role is taken only when it looks like a portal role: it logs in, holds no power and no membership, has no member but Singularr's login, and owns neither the database nor anything in the schema `public`. Otherwise the start logs one warning and leaves it alone |
| `SINGULARR_LISTEN` | `0.0.0.0:3000` | the address the server listens on |
| `SINGULARR_FACE` | `owner` | `requests` starts Singularr Requests, the viewer portal (below) |
| `SINGULARR_WORKSPACE_NAME`, `SINGULARR_PORTAL_NAME` | `Singularr`, `Singularr Requests` | the names shown in the workspace and the portal |
| `SINGULARR_OWNER_URL` | unset | the workspace's public address, linked for the owner on the portal |
| `SINGULARR_REQUESTS_PUBLIC_URL` | unset | the portal's public address, linked from the workspace |
| `SINGULARR_SIGN_OUT_URL` | unset | the target of the workspace's Sign out (the sign-in proxy's sign-out address) |
| `SINGULARR_INSTANCE` | unset | an instance file (`singularr.toml`) with workspace names and integrations beyond the environment's (below) |
| `SINGULARR_DATA_DIR` | `/data` | the folder Singularr writes to: the location database it downloads (Statistics, below) and the media tools' scratch files (`scratch`). The compose file mounts the volume `data` there. The image creates `/data` owned by uid 10001: a new volume starts out writable. `singularrd doctor` writes a file there and removes it (`Data folder`) |
| `RUST_LOG` | `info` | log filter |
| `TZ` | `UTC` | time zone of the container |

The image's health check is the binary's own: `singularrd health` asks `/readyz` at
`SINGULARR_LISTEN`. It exits 0 when the server is ready, else 1. Singularr is ready when the database
answers and the migrations are applied. Singularr Requests (`SINGULARR_FACE=requests`) is ready when
its database role can read its views. The call goes straight to the listen address: a proxy set in
the environment (`HTTP_PROXY`, `ALL_PROXY`) does not carry it. The image holds no curl.
`docker exec <container> /app/singularrd health` asks the same.

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
| `PLEX_URL`, `PLEX_TOKEN` | the Plex server (id `plex`): libraries, ratings, plays, music, sessions and images. Read only |
| `JELLYFIN_URL`, `JELLYFIN_API_KEY` | a Jellyfin server (id `jellyfin`): libraries, ratings, plays, music, sessions, users and images. The key is an API key from the server's dashboard. Read only |
| `EMBY_URL`, `EMBY_API_KEY` | an Emby server (id `emby`), read the same way as Jellyfin. Read only. Not tried against a real Emby server yet: its tests read answers made from Emby's API reference |
| `JELLYFIN_WEB_URL`, `EMBY_WEB_URL` | where people open the server's web app, for the links to its items, when that is not the address Singularr reads it at. Default: the server's address |
| `RADARR_URL`, `RADARR_API_KEY`, `RADARR_NAME` | Radarr (id `radarr`). With write access: search, grab, import a refused download, remove a download |
| `SONARR_URL`, `SONARR_API_KEY`, `SONARR_NAME` | Sonarr (id `sonarr`), the same as Radarr |
| `QBIT_URL`, `QBIT_USER`, `QBIT_PASS` | qBittorrent (id `qbittorrent`). Without a user and password Singularr does not sign in |
| `TRANSMISSION_URL`, `TRANSMISSION_USER`, `TRANSMISSION_PASS` | Transmission (id `transmission`): its torrents through its RPC. The user and password are needed only when its RPC asks for them |
| `DELUGE_URL`, `DELUGE_PASS` | Deluge (id `deluge`): its torrents through its Web UI, signed in with the web password. The Web UI must be connected to its daemon. The category is the Label plugin's label |
| `SABNZBD_URL`, `SABNZBD_API_KEY` | SABnzbd (id `sabnzbd`): its queue and the jobs of its history that finished in the last 7 days, at most 100 |
| `NZBGET_URL`, `NZBGET_USER`, `NZBGET_PASS` | NZBGet (id `nzbget`): its queue, the jobs of its history that finished in the last 7 days (at most 100) and its download rate |
| `SEERR_URL`, `SEERR_API_KEY` | Seerr (id `seerr`): requests and their accounts |
| `TAUTULLI_URL`, `TAUTULLI_API_KEY` | Tautulli (id `tautulli`): its play history imports into Statistics |
| `TDARR_URL`, `TDARR_API_KEY` | Tdarr (id `tdarr`): nodes, workers, libraries, flows and reports. With write access: worker limits, pause, cancel, requeue, scan, and a library's flow |
| `PROWLARR_URL`, `PROWLARR_API_KEY` | listed as an integration. Not read yet |
| `<TYPE>_ACCESS` | `read` or `act` for that integration, set by the environment. Without it, the switch in Settings, Integrations decides |

A download client the configuration stops listing (its variables lost, its id renamed) keeps its
downloads for 30 days, out of every list. Its return finds them as they were. Two download
clients at one address are one client: the first is read.

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
type = "radarr"              # plex, jellyfin, emby, radarr, sonarr, qbittorrent, transmission,
                             # deluge, sabnzbd, nzbget, prowlarr, tdarr, hush, seerr, tautulli
name = "Radarr 4K"
url = "http://radarr-4k:7878"
secret = "RADARR_4K_API_KEY" # the variable with the key (default <ID>_API_KEY)
# web_url = "https://watch.example.org" # jellyfin and emby only: where its items open
```

The file contains no secrets: `secret` names the environment variable. Unknown keys and types stop the
start.

Singularr reads one media server of each type: the first Plex, Jellyfin and Emby server the
configuration lists. Each one has its own libraries, server pass and sessions. Its plays count in
Statistics. An album or artist of the first media server has the id `album-<key>`. One of another
server has `album-<server>_<key>`.

## Statistics

Statistics counts every play of every media server: each server's sessions, a Plex server's own
history, and Tautulli's history when one is configured. Each play records the media server, the
player (the app and its version, the platform and its version, the device), the client's address
with its location, and how the stream was delivered. The statistics are the owner's alone. No API
key reads them. Singularr Requests cannot reach them.

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_GEOIP_DOWNLOAD` | unset (on) | DB-IP's IP to City Lite database is downloaded once a month into the data folder. `off` turns the download off. `on` fixes it on. Unset, the switch in Settings, Jobs, Play history decides, on by default |
| `SINGULARR_GEOIP_PATH` | unset | a location database the operator provides: a MaxMind DB file with city records, such as DB-IP's City Lite or MaxMind's GeoLite2 City. Set, nothing is downloaded |

Locations are looked up on this machine. No address is sent to any service. The download is one
GET of `https://download.db-ip.com/free/dbip-city-lite-YYYY-MM.mmdb.gz` (some 60 MB), the only
address on that host Singularr may call. The database takes some 130 MB of memory while in use.
Without a database, Statistics shows each address without a location. A private, loopback or
link-local address reads Local network.

The players' addresses are kept by default (Settings, Jobs, Play history, Keep IP addresses).
Turned off, the plays keep only home network or outside. The addresses already kept are removed.
An address is never written to the log.

IP geolocation by [DB-IP](https://db-ip.com), licensed under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).

## Metadata and search

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_TMDB_TOKEN` | unset | a TMDB API read token |
| `SINGULARR_TVDB_KEY` | unset | a TVDB v4 API key |
| `SINGULARR_WATCH_REGION` | `US` | the region of release dates, certifications and where to watch |
| `SINGULARR_SEARCH_URL`, `SINGULARR_SEARCH_KEY` | unset | a Meilisearch for search, used when both are set. The compose file sets both to its `meilisearch` service and `MEILI_MASTER_KEY` unless `.env` sets them. Otherwise search is built in (PostgreSQL, typo tolerant with the `pg_trgm` extension). When Meilisearch does not answer, the built-in search does and `GET /api/v1/search/status` reports `provider: postgresql` with `state: warn`. Singularr waits for the compose file's Meilisearch to be healthy and starts without it when it never is |
| `MEILI_MASTER_KEY` | required by the compose file | the master key of the compose file's Meilisearch, at least 16 characters (`openssl rand -hex 32`). The compose file passes it to Meilisearch and to Singularr. Its volume `search` holds the index. The next index pass rebuilds the index when the volume is lost |
| `SINGULARR_SEARCH_PREFIX` | `singularr_` | the start of every Meilisearch index name |
| `SINGULARR_PLEX_WEB_URL` | `https://app.plex.tv/desktop` | Plex's web app, for a title's Plex link. Jellyfin and Emby titles open in the server's own web app (`JELLYFIN_WEB_URL`, `EMBY_WEB_URL`) |

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
| `SINGULARR_SYNC_DOWNLOADS_SECS` | 30 | every download client, each read on its own. `SINGULARR_SYNC_QBIT_SECS` is the older name |
| `SINGULARR_SYNC_HISTORY_SECS` | 900 | the newest page of each manager's grab history |
| `SINGULARR_SYNC_HISTORY_MAX_PAGES` | 40 | pages of history read at the first start |
| `SINGULARR_SYNC_REQUESTS_SECS` | 300 | Seerr's requests |
| `SINGULARR_SYNC_SERVER_SECS` | 3600 | each media server's libraries, items and music |
| `SINGULARR_SYNC_PROCESSING_SECS` | 60 | the processing services |
| `SINGULARR_SYNC_JITTER_SECS` | 20 | a random delay before each loop's first pass |
| `SINGULARR_HTTP_TIMEOUT_SECS` | 30 | the time limit of an outbound call |

## Singularr Requests

The portal (`SINGULARR_FACE=requests`) runs as its own process with its own database role. It refuses
to start when a setting of the full process is present: `DATABASE_URL`, `SINGULARR_INSTANCE`,
`SINGULARR_TVDB_KEY`, `SINGULARR_SEARCH_KEY`, `SINGULARR_EDGE_TOKEN`, `SINGULARR_OWNER_EMAILS`, or any
variable whose name ends `_API_KEY`, `_TOKEN` or `_PASS` (every integration secret does) other than
`SINGULARR_TMDB_TOKEN`.

| Setting | Default | Meaning |
|---|---|---|
| `SINGULARR_REQUESTS_DATABASE_URL` | required | the login of the role `singularr_requests`. Singularr creates the role when `SINGULARR_REQUESTS_DB_PASSWORD` is set (above). The compose file builds this address from that password. Without it, run `deploy/requests-role.sql` as a role that may create roles and give the role its password with psql's `\password singularr_requests` |
| `SINGULARR_REQUESTS_SESSION_SECRET` | required | at least 32 characters. Signs the Plex sign-in cookie |
| `SINGULARR_REQUESTS_PUBLIC_URL` | required | the portal's own address. Plex returns the sign-in to it |
| `SINGULARR_TMDB_TOKEN` | unset | for search and title pages |
| `SINGULARR_REQUESTS_DB_MAX_CONNECTIONS` | 5 | pool size |
