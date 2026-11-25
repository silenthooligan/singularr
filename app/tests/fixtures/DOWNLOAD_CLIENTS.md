# Download client fixtures

The answers in `transmission/`, `deluge/`, `sabnzbd/` and `nzbget/` are captured from the real
programs, on 25 Nov 2026, each in a throwaway container on the build runner. The containers had no
network (`--network none`). Nothing reached a tracker, a peer or a news server outside them. They
were removed with their volumes and images afterwards. The bodies are the programs' own, indented
and otherwise unedited. Only the answers are kept: no request header, cookie, session id or key.

What was downloaded is made up. The torrents were made for the capture: a public Linux ISO's name
over random pieces, a release name over random pieces and a magnet whose metadata never came, all
added paused. One more is a torrent of a local file of random bytes. The client checked it and
seeds it.
The NZBs name articles that exist nowhere, except those served by NZBGet's own test news server
(NServ) on the container's loopback, from files of random bytes.

| Folder | Program | Answers |
|---|---|---|
| `transmission` | Transmission 4.1.4 (RPC 6.0.1) | `session_get.json` (`session-get` with the version fields); `torrent_get.json` (`torrent-get`: three torrents stopped, one seeding) |
| `deluge` | Deluge 2.2.0, its Web UI with the Label plugin on | `login.json`, `login_refused.json` (`auth.login`); `not_authenticated.json` (`web.update_ui` without a session); `update_ui.json` (three torrents paused, one seeding); `update_ui_disconnected.json` (the web interface with no daemon) |
| `sabnzbd` | SABnzbd 5.1.3 | `version.json`; `queue.json` (two jobs paused, one downloading at 100 KB/s from NServ); `history.json` (one completed, one failed: its articles exist nowhere) |
| `nzbget` | NZBGet 26.3 | `version.json`, `status.json` (its download rate); `listgroups.json` (two jobs paused, one downloading); `history.json` (two completed, one failed, one deleted as a copy of another); `listgroups_post_processing.json` (two jobs being moved and waiting to be post-processed) |

Two NZBGet jobs share one `drone` parameter (the downloading one and the one deleted as its copy):
the capture added the same NZB twice under one value. The reconcile keeps the first of two
downloads with one download id.

A usenet client's history is read over a window of 7 days. The adapters' tests read the
histories at a clock an hour after the capture. A test that reads one through the real clock
moves its times to an hour ago.

The qBittorrent answer (`qbit_torrents.json`) is older and synthesized: hashes and values written
by hand in qBittorrent's shape.
