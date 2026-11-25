# The web client

This folder is Singularr's interface: static pages that read `/api/v1` and nothing else. The server hands them out at clean addresses (`app/src/client.rs`); there is one interface and no preview beside it. `DESIGN.md` ("Product and instance", "Web app", "Two faces") is the contract; this file says how the client is put together.

## Live and demo

A page asks `GET /api/v1/whoami` before it draws (`findServer` in `js/core.js`).

- A JSON answer: a server is behind the page. `MQ.live` is true and data comes from `/api/v1`.
- A 401 or 403 in JSON: the page says "Not signed in".
- Anything else (a static host, a file): the demo. Data comes from the sample set in `data/`.

A page asks for a record by name, `MQ.data('activity.json')`, and never knows which source answered. `ROUTES` in `js/core.js` maps each name to its route and holds a mark that tells this client's shape from an older answer. While a record's route is missing or answers another shape, the page gets the sample record and a note under the top bar says "Snapshot data on this page". **Each sample file is the contract of one route**: same shape, same field names.

| Record | Route |
|---|---|
| `workspace.json` | `GET /api/v1/workspace` (until it exists the client composes it from `/whoami`, `/integrations`, `/libraries`, `/summary`) |
| `home.json` | `GET /api/v1/home` |
| `discover.json`, `browse/<key>.json` | `GET /api/v1/discover`, `GET /api/v1/discover/<key>` |
| `library-movies.json`, `library-shows.json` | `GET /api/v1/library/movies`, `/library/shows` |
| a title, a season | `GET /api/v1/metadata/titles/{kind}/{tmdb}`, `/metadata/shows/{tmdb}/seasons/{n}`, `/metadata/titles/{kind}/{tmdb}/credits` |
| a person, a collection, `collections.json` | `GET /api/v1/metadata/people/{id}`, `/metadata/collections/{id}`, `/collections` |
| `requests.json` | `GET /api/v1/requests` |
| `activity.json` | `GET /api/v1/activity/queue` (the Activity page reads `/activity/history` and `/activity/wanted` itself) |
| `calendar.json` | `GET /api/v1/calendar?from=&to=` |
| `music.json`, an album | `GET /api/v1/music`, `/music/albums/{id}` |
| `homevideo.json` | `GET /api/v1/library/videos?id=` |
| `rules.json` | `GET /api/v1/rules` (More reads its editions, protections and request rules) |
| `profiles.json`, `lists.json`, `lists-unlisted.json` | `GET /api/v1/profiles`, `/lists`, `/lists/unlisted`; the editors write with `PUT /api/v1/profiles/<id>`, `POST /api/v1/lists`, `PUT /api/v1/lists/<id>` and rank with `POST /api/v1/profiles/preview` |
| `naming.json` | `GET /api/v1/naming` |
| `journal.json` | `GET /api/v1/journal` |
| `settings.json` and its panels | `GET /api/v1/settings`, `PATCH /api/v1/settings`, `/integrations`, `/libraries`, `/people`, `/jobs`, `/system`, `/keys` |
| search | `GET /api/v1/search?q=` (the demo searches `data/search.json`) |
| the processing page | `GET /api/v1/processing` (`processing.json`), and live only: `/processing/workers/{id}` (the worker: nodes, queue, libraries, flows, resources, history; its changes are POSTs below it), `/processing/rules` (rules, `preview`, `{id}/log`, `{id}/run`) |
| a title's releases (live only) | `GET /api/v1/titles/{id}/releases?season=&episode=`: a manager's interactive search with Singularr's verdict beside the manager's (`js/releases.js`); the acts `POST /api/v1/titles/{id}/grab`, `/titles/{id}/search`, `/activity/queue/{id}/import`, `DELETE /activity/queue/{id}?blocklist=`, `POST /api/v1/search` (the wanted list) |

Writes go through `MQ.send(method, path, body)`. A control that cannot act (its service is read-only, absent, or there is no service behind it) is drawn with `MQ.act(label, { needs, kind })`: a disabled button (`aria-disabled="true"`, never the `disabled` attribute, so its tooltip still shows) whose tooltip says what it needs, "Needs write access to Radarr" or "Not available" (`MQ.needTip(needs, kind)`, the name taken from the workspace's integrations). `needs` is `'manager'` (with `kind` `'movie'` or `'show'` where the page knows it), `'requests'`, `'downloads'`, `'server'` or `'processing'`. A menu entry takes `disabled: true, tip: MQ.needTip(...)`; a button a page writes itself takes `MQ.off(needs, kind)`; a table's bulk item without `onClick` takes `needs` and `kind`. Pressing such a control does nothing, and no toast explains it. Only what Singularr owns is saved for real.

A control that acts once its service may be acted on is drawn with `MQ.actBtn(label, { needs, kind, data })`: a live button carrying `data` (the attribute the page's click handler finds it by) while `MQ.mayAct(needs, kind)` holds (a server is behind the page and the integration's `access` is `act`), else the same control as `MQ.act` draws it. An act that changes or removes something asks first with `MQ.ask(title, line, verb, { danger })` (the title a question, one line of consequence, Cancel and the verb). A read that takes long (a manager's search asks every indexer) passes `MQ.fetchJson(path, { timeout, tries: 1 })`, so it waits and is not asked twice.

## Layout

```
web/
  *.html              one small file per page; the shell and the content are rendered by scripts
  404.html            the page of an unknown address (its files are named from the root)
  css/singularr.css   the design system (tokens, shell, table, cards, title page, forms); css/pages/<page>.css for one page
  css/fonts/          DM Sans and IBM Plex Mono, each with its OFL notice
  brand/              the identity's marks (horizon symbol, Singularr Requests lockup, each for a dark and a light surface), icons and share card
  singularr.webmanifest  the workspace as an app; portal/requests.webmanifest is the portal's
  js/core.js          MQ: the two data sources, the shell (sidebar tree, search), drawer, menus, formats, shared pieces
  js/table.js         MQ.table: the one table
  js/request.js       MQ.request: the request dialog with the season and episode picker
  js/processing.js    MQ.proc: what processing did to a file (a run in words, its drawer, the table columns) and the hand-off dialog
  js/releases.js      MQ.releases: the interactive search (the file in the library, then every release with both verdicts and Grab), and the search asks of a title
  js/profiles.js      MQ.profiles: quality profile cards and the editor of five answers with the live ranking (Rules)
  js/lists.js         MQ.lists: lists of release groups and services as cards of chips, and Unlisted (Rules)
  js/naming.js        MQ.naming: naming conventions with the token picker and the live preview (Rules)
  js/pages/<page>.js  one file per page
  data/, art/         the demo's sample set (not in the image): records in the routes' shapes, and images with no public address
  portal/             the request portal's pages with their own copies of the shared files (tools/sync_portal.py); demo only until the request face serves them
  tools/              build_data.py and its parts: they build the sample set from fixtures and the metadata providers
```

Pages (addresses without `.html` when a server hands them out): Home (`index`), `discover`, `browse?key=<list>`, `search?q=`, `movies`, `shows`, `title?id=movie-<tmdb>|show-<tmdb>`, `person?id=person-<tmdb>`, `collection?id=collection-<tmdb>`, `music`, `album?id=`, `artist?id=`, `library?id=<library id>` (a plain video library), `requests`, `activity`, `processing` (where a processing service is configured), `calendar`, `rules`, `lab`, `journal`, `settings`, `setup`.

The sidebar's sections open into sub-pages (`navTree` in `js/core.js`). A sub-page is a state of its page, kept after the `#`: Discover, Requests and Activity by kind (`#kind=movie`, `#kind=show`), Requests also `#tab=people`, Rules by `#tab=profiles|lists|naming|more` (a quality profile's editor by `#profile=<id>`; an older `#tab=traits` or `#open=profile:<id>` still lands) plus the Release Lab, Settings by `#tab=<section>`.

Rules for page code:

- Classic scripts with `defer`, in this order: `js/core.js`, `js/table.js` (and `js/request.js` where a page asks), the page script. No modules, no build step, no dependencies. Everything hangs off `window.MQ`.
- A page script calls `MQ.page({ id, title, render })`. `render(root, workspace)` fills the page; the shell, the search and the drawer already exist.
- Addresses come from `MQ.to(page, query, hash)` and `MQ.link(id)`, never from a file name written out. State that should survive a reload or be linked (sub-page, tab, view, filters, sort) lives in the address after `#`.
- A page works on both sources and tolerates a missing optional field: it never prints "undefined", "null" or an empty label.
- Movies and shows are told apart: where both appear, each kind has a block, tab or table of its own.
- Words: file, files, "in the library", "not in the library", "missing". Never copy or held. What a file is (`WEB-DL`, `HEVC`, `AAC 5.1`, `DV HDR10`) comes worded from the server, the same in every table and record; a page words only language codes (`MQ.fmt.langs`).
- A block stands off the page by its tone: what sits on the page (a panel, a table, a row of tiles, a group of settings) is filled with `--panel`, its head with `--head`, the fields in it with `--sunk`. A page never fills a block with `--bg`, or the page reads as one flat sheet.
- A row of tiles (`mq-stats`) is three rows per tile: label, value, a line under it. A value that is a word takes `mq-stat-value--word`. No inline sizes.
- A file's name is shown where the file is listed, written out in full (the `mq-files` table of a movie's title page puts it under the file's cells).
- A form counts a field as changed only when its control no longer reads what it read when it was drawn (`MQ.cfg.wire`), and a select always holds the value it was given, in the list or not. A page that warns of unsaved changes has to be checked untouched.
- No wording of one installation in code or markup: names of libraries, integrations, profiles and people come from data.
- No em dashes or en dashes anywhere, US English, sentence case for labels.
- The brand marks come from `MQ.mark('horizon' | 'requests', alt)`: both files, the theme shows the one for its surface (`--mark-dark`, `--mark-light`). An episode coordinate (S02E03) drawn as markup takes `mq-coord`, a timestamp `mq-time` (a table's `date` cells have it): IBM Plex Mono.
- Themes: Event Horizon on `:root` (the default, `horizon`), Mocha and Graphite by `data-theme`. A theme changes tokens only; a light theme also swaps the marks and takes Ink violet `#5b3d92` as its accent.

## What a page is made of

`activity.html` with `js/pages/activity.js` (a block per kind) and `title.html` with `js/pages/title.js` (one page in sections) are the reference pages: copy their structure. The helpers, all on `MQ` (each is documented where it is defined in `js/core.js`, `js/table.js`, `js/request.js`):

| Need | Use |
|---|---|
| Markup | `MQ.html` (a tagged template that escapes every value; nest templates and arrays freely), `MQ.raw`, `MQ.join(list, separator)`, `MQ.el(markup)` |
| Data | `MQ.data('activity.json')`, `MQ.maybe(path)`, `MQ.rows(table)` for `{columns, rows}` files, `MQ.title(id)`, `MQ.person(id)`, `MQ.collection(id)`, `MQ.album(id)`, `MQ.param('id')` |
| Page | `MQ.page({id, title, flush, render(root, workspace)})`, `MQ.head({title, meta, sub, crumbs, actions})` |
| Lists | `MQ.table(host, config)`: views, search, filter chips, sort, columns, density, paging, selection, the API button, and a posters mode (`grid`). `toolbar: 'mini'` for several tables under one control row |
| Media | `MQ.card(card, opts)`, `MQ.slider(title, items, opts)`, `MQ.personCard(person)`, `MQ.art(path, size, label, cls)`, `MQ.img(path, size)`, `MQ.titleCell(card, secondLine, opts)` |
| Facts | `MQ.facts(pairs)`, `MQ.kv(pairs)`, `MQ.metaline(parts)`, `MQ.ratings(ratings)`, `MQ.quality(file)`, `MQ.trace(trace, total)`, `MQ.what(request.what)` |
| Status | `MQ.stateDot(key, label)` in tables and text, `MQ.badge(key, label)` on artwork and in headers, `MQ.progress(percent, tone, label)`, `MQ.steps(status, mini)` |
| Overlays | `MQ.drawer({...})`, `MQ.dialog({...})`, `MQ.menu(button, items)`, `MQ.toast(text, tone)`, `data-tip="..."` on any element |
| Controls | `MQ.tabs.mount(host, tabs, onChange)`, `MQ.seg(options, value, onChange)`, `MQ.act(label, opts)` for a control that cannot act (disabled, its tooltip says what it needs), `MQ.hash.get/set` for state in the address |
| Text | `MQ.fmt`: `date`, `day`, `time`, `datetime`, `rel`, `size`, `speed`, `dur`, `eta`, `clock`, `num`, `money`, `ep`, `lang`, `langs` (a list of language codes in words), `country`, `cap`; `MQ.plural`; `MQ.icon(name)`; `MQ.copy(text, what)` and `data-copy="text" data-copied="what"` on a button that copies |
| Requests | `MQ.request.open(record, opts)`: the dialog with the season and episode picker |
| Processing | `MQ.proc.summary()`, `MQ.proc.runs(query)`, `MQ.proc.mark(run)`, `MQ.proc.sentence(run)`, `MQ.proc.drawer(run)`, `MQ.proc.send(title, file)`, `MQ.proc.columns(kind)` (pages that load `js/processing.js`) |

The classes are in `css/singularr.css`, grouped by layer: page furniture (`mq-head`, `mq-tabs`, `mq-section`, `mq-panel`, `mq-stats`, `mq-grid-2`, `mq-empty`), controls (`mq-btn`, `mq-seg`, `mq-chip`, `mq-tag`, `mq-input`, `mq-select`, `mq-check`, `mq-switch`, `mq-badge`, `mq-state`, `mq-progress`, `mq-steps`), table, media (`mq-card`, `mq-grid`, `mq-slider`, `mq-person`, `mq-tile`, `mq-billboard`, `mq-ratings`), title page (`mq-hero`, `mq-facts`, `mq-season`, `mq-files` for a movie's files with each file's name under its cells, `mq-folder`), overlays, and pieces (`mq-list`, `mq-timeline`, `mq-trace`, `mq-kv`, `mq-code`, `mq-callout`, `mq-meter`). A page that needs a layout of its own puts it in `css/pages/<page>.css` and links that file after `singularr.css`.

## Status vocabulary

One list, one tone each, through `MQ.state(key)`:

| Key | Label | Tone |
|---|---|---|
| `available` | Available | ok (olive) |
| `partial` | Partial | ok, outlined |
| `requested` | Requested | warn (amber) |
| `approved` | Approved | info (blue) |
| `searching` | Searching | info |
| `downloading` | Downloading | info |
| `importing` | Importing | info |
| `processing` | Processing | proc (violet) |
| `seeding` | Seeding | mute |
| `missing` | Missing | mute, outlined |
| `unaired` | Not aired (episodes) | mute |
| `unreleased` | Not released (movies) | mute |
| `attention` | Needs attention | bad (red) |
| `failed` | Failed | bad |
| `declined` | Declined | mute |

## Data

Every file under `data/` has the shape its route answers (the table above). Images from TMDB are stored as paths (`/abc.jpg`); `MQ.img(path, size)` builds the address. Sizes are in bytes, times are ISO 8601, durations are minutes unless named otherwise.

Identifiers: `movie-<tmdb>`, `show-<tmdb>`, `person-<tmdb>`, `collection-<tmdb>`, `album-<n>`, `artist-<n>`, `r-<n>` (request), `q-<n>` (queue item).

**card** (every poster in a row or grid): `{id, kind, title, year, poster, rating, state?}`. `state` is absent for a title outside the library with no request.

**`workspace.json`**: `{name, portal, server:{name, version}, user:{name, role, initials}, libraries:[{id, name, kind: movie|show|music|homevideo, titles, episodes?, albums?, artists?, size}], counts:{requestsPending, queue, attention, wanted}, integrations:[{id, type, name, role, state: ok|warn|down, version, detail}], storage:[{name, path, used, total}], profiles:[{id, name, kind}], generated}`.

**`library-movies.json`**: `{columns:[names], rows:[[values]]}`, one row per movie, columns: `id, title, year, poster, rating, imdb, certification, runtime, genres[], studio, collection, state, monitored, library, profile, resolution, source, codec, hdr, audio, group, editions[], files, size, added, score, cutoffMet, released`.

**`library-shows.json`**: same form, columns: `id, title, year, poster, rating, certification, network, status (continuing|ended|upcoming), genres[], seasons, episodes, have, nextAir, library, profile, size, added, monitored, state`.

**`titles/<0..63>.json`**: `{id: record}`; the bucket is the sum of the id's character codes modulo 64. A record for every library title and every title a page links to.

- All records: `id, kind, tmdb, imdb, tvdb?, title, year, overview, certification, runtime, genres[], poster, backdrop, ratings:{tmdb:{score, votes}, imdb?:{score, votes}, rt?:{critics, audience?}, metacritic?}, links:{tmdb, imdb?, tvdb?, trakt?, letterboxd?, homepage?}, rich: bool, library: {...} | null`.
- Rich records add: `originalTitle?, tagline, status, dates:{theatrical?, digital?, physical?}` (movie) or `dates:{first, last?, next?}` (show), `budget?, revenue?, language, countries[], studios:[{id, name, logo?}], networks?:[{id, name, logo?}], keywords[], collection?:{id, name, poster, backdrop}, logo?, videos:[{site, key, name, type}], watch:[{name, logo}], crew:[{id, name, job, dept, photo?}], creators?:[{id, name, photo?}], cast:[{id, name, role, photo?}], recommendations:[card], similar:[card]`.
- `library` (movie): `{state, monitored, library, profile, path, added, editions:[{key, label, default, wanted, files:[{library, resolution, source, quality, codec, bitDepth, hdr, audio, audioLangs, subs[], size, runtime, fps, group, fileName, sceneName, added, score, formats[], cutoffMet, trace:[{label, points, why}]}]}]}`.
- `library` (show): `{state, monitored, library, profile, path, added, size, have, total}` and the record carries `seasons:[{number, name, airDate, poster?, have, total, episodes:[{number, title, airDate, runtime, overview, still?, state, file?:{quality, resolution, source, codec, hdr?, audio, size, group, score, added, fileName}}]}]`. A record that is not rich has seasons with counts and the inLibrary episode numbers only.
- Showcase records also carry `requests:[...]`, `history:[{at, event, detail, quality?, source?}]` and `queue:[q ids]`.

**`people/<0..15>.json`**: `{id: {id, name, photo, knownFor, born?, died?, birthplace?, biography?, aka[], links:{tmdb, imdb?}, credits:{cast:[card + role], crew:[card + job]}, rich}}`. A person who appears in any rich title has at least a light record built from those titles.

**`collections.json`**: `{id: {id, name, overview, poster, backdrop, parts:[card]}}`.

**`discover.json`**: `{billboard:[card + backdrop, overview, certification, runtime, genres, tagline], rows:[{id, title, items:[card]}], genres:{movie:[{id, name, backdrop}], show:[...]}, studios:[{id, name, logo}], networks:[{id, name, logo}]}`. **`browse/<key>.json`**: `{title, kind, items:[card]}` for each genre, studio and network.

**`requests.json`**: `{requests:[{id, title: card, what:{type: movie} | {type: seasons, seasons[]} | {type: episodes, items:[{season, episodes[]}]}, target: HD|4K, by:{id, name}, at, status, progress?, note?, decided?:{by, at, reason?}, rules:[{label, result, detail}], queue?:[q ids]}], people:[{id, name, role, requests, quota:{movies:{used, limit, days}, seasons:{used, limit, days}}, autoApprove, lastSeen}]}`.

**`activity.json`**: `{queue:[{id, kind, title: card, episode?:{season, number, title}, pack?:{season}, edition?, release, quality, resolution, source, codec, hdr?, group, indexer, client, manager, stage, attention?:{reason, since, fix}, progress, size, done, speed, eta, seeds?, peers?, added, profile, score, trace:[{label, points, why}], timeline:[{at, stage, detail}], request?}], history:[{id, at, kind, title, episode?, event: grabbed|imported|upgraded|processed|failed|removed, quality, from?, source, detail, size?, group?}], wanted:[{id, kind, title, episode?, why: missing|upgrade, released, profile, have?, want, lastSearch, monitored}]}`.

**`processing.json`** (`GET /api/v1/processing`): `{services:[{id, type, name, role: bridge|worker, state, detail, version, lastSync, facts?, nodes?}], workers:[{service, node, file, percent, fps, eta, status, kind, startedAt, run}], counts:{open, working, waiting, verifying, stuck, imported, failed, refused, pruned, superseded, total, savedBytes, since}, flows:[{id, modes[], label, about, service, tag?, needs?:{minResolution?, notCodec?[]}, runs, imported, lastRun, savedPercent, medianSeconds}], open:[run], recent:[run]}`. A **run** is one file sent through one flow: `{id, service, flow, flowLabel, trigger:{kind, label, raw}, state: staged|working|verifying|imported|failed|refused|pruned|superseded, at, updated, finished, took, title?: card, episode?:{season, number}, source:{file, path, size}, output?, result?, saved, replaced:[{file, path, size, stillThere}], detail, stuck, worker, events:[{at, event, actor, note}]}`. `GET /api/v1/processing/runs` pages through them and `/processing/runs/<id>` adds `raw`, the record exactly as the service gave it. A title record carries `processing:{runs, open, total}` (null when the installation reads no processing service) and each of its files `processing:{state: processed|processing, run}`. A page shows processing only where these are present: an installation without a processing service has no trace of it.

**`calendar.json`**: `{from, to, items:[{date, time?, kind, title, episode?, release?: theaters|digital|physical, state}]}`.

**`music.json`**: `{albums:[{id, title, artist, artistId, year, genre?, tracks, duration?, format, quality, size, cover:{local?}, released?, label?, added, plays, problems[]}], artists:[{id, name, albums, tracks, size, photo?, genre?, country?, summary?}], problemLabels}`. **`albums/<0..15>.json`**: `{id: {...album, tracks:[{n, disc, title, artist?, duration, format, bitrate?, size}]}}`. Durations are minutes.

**`profiles.json`**: `{source:{kind: file|saved, file, fileEnv, settingSource, settingEnv, version, savedAt, savedBy}, editable, version, profiles:[profile + summary, titles], lists:[{id, name, kind, members}], rules:[{id, name, category, why, when}], options:{groups:[{id, answer, label, choices:[{key, label}]}], sources:[{key, label}], resolutions, stances}}`. A **profile**: `{id, name, rev, managerProfiles[], target:{kind, resolution, sources:[[source]], acceptFrom, stopAt}, picture:{range, codec}, sound:{formats, channels, languages}, lists:{ranked, unlisted, crossing}, size:{minPerHour?, maxPerHour?, smallerWinsTie}, upgrades:{when}, more:{release, editions, rules}, scores?}`, each answer a list of picks `{key, stance: prefer|allow|avoid|block, required?, points?}` in order, the strongest first. `points` and `scores` are numbers an imported profile keeps; a change of order or stance drops them. **`lists.json`**: `{source, editable, version, lists:[{id, name, kind: group|service, members[], only?, rev, files, library:[{name, files}], usedBy:[{profile, name, stance}]}]}`. **`lists-unlisted.json`**: `{groups:[{name, files, downloads}], services:[...]}`.

**`homevideo.json`**, **`rules.json`**, **`journal.json`**, **`settings.json`**, **`home.json`**, **`search.json`**: see `tools/build_data.py`, which documents each next to the code that writes it.

`portal/data/` holds the same files reduced to public fields: no `library` object, no files, scores, requesters or activity. A show keeps each episode's `state` so a person can see what is missing.

## What is real and what is sample

Real: the movie and show libraries with their files and media details (exported from the managers), ratings and plays from Plex, all TMDB metadata, artwork, cast and crew, the calendar, the music library as Plex holds it with its covers and artist photos, the home video recordings (the people in their names invented, no stills), the titles of the requests and of the queue, and the imports of the last 14 days in History.

Sample: the people who request (names, limits, automatic approval), 19 of the 49 requests, the queue's release names, progress, speeds and indexers, grabs, failures and removals in History, most of the Journal, what is playing, storage, addresses and keys in Settings, the request, retention, processing and naming rules, the notes on five home video recordings, two of Avatar's three copies, and every candidate on a title's Releases tab. Pages mark sample parts where they show them.

323 titles and 250 people of the sample set carry their full record; the other titles carry a shorter one. With a server behind the page every title and person is read from it in full.

## Rebuild

`python web/tools/build_data.py` from the repository root (Python 3.11 or later). It reads `fixtures/`, asks TMDB with the token in `.env` (never printed; answers are cached in `web/tools/.cache/`, which git ignores) and writes `data/`, `portal/data/` and `art/`. After a change to a shared file (`css/singularr.css`, `js/core.js`, `js/table.js`, `js/request.js`) run `python web/tools/sync_portal.py` so the portal's copies follow.

## Notifications

`js/notify.js` draws the Notifications panel of Settings from `GET /api/v1/notifications` (the demo reads `data/notify.json`). The form of a channel is drawn from its type's own field list, so a new type needs no client change. `data/notify.json` must match `app/tests/fixtures/contract_notify.json`: a server test compares them, and `web/data` is not in the image. The older `data/notifications.json` is no longer read.

## Statistics

`stats.html` with `js/pages/stats.js` draws play statistics from `/api/v1/stats/*`; the demo reads `data/stats.json`, which holds each route's answer in that route's shape (invented people and devices, made once by a script outside the repository). Charts are inline SVG drawn by the page, in the page's tokens.

## Search, grab and import

The managers search, grab and import while Singularr may act on them (Settings, Integrations). Live only; the demo
draws each control as one that cannot act.

- **Title page.** Search asks the title's manager to search (a movie, a whole show; on a show's episodes and in the
  episode drawer, those episodes; for picked episodes, a season at a time). Interactive search (a movie in the
  header and its file drawer; an episode in its drawer; the picked episodes of one season) opens `MQ.releases`: the
  file in the library with its score both ways, then every release in one table (release name, quality, size, age,
  peers, indexer, languages, the verdict with its reasons as chips, the score, the manager's score and word), views
  All, Accepted and Accepted by the manager, a row's reasons and score lines in a dialog, Grab on each row. The
  search is a read: it answers under read access too, and Grab is then drawn as a control that cannot act.
- **Activity, Queue.** A download its manager refused to import carries `refusal`; its attention reads in Singularr's
  words (`Radarr refused the import · 300 against 600`) with the manager's sentence behind Details, and the drawer
  and the row menu offer Import anyway, Remove and Remove and blocklist. A download any manager owns offers Remove.
- **Activity, Wanted.** Each row's Search and the bulk Search ask the managers (`POST /api/v1/search`, at most 50 at
  a time). The header's Search missing stays a control that cannot act: it would search every wanted title at once.
