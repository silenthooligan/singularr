# Emby fixtures

The Emby adapter has not been tried against a real Emby server. These files are the only answers its tests read.

Emby has no public demo server. Every file here is synthesized. Each follows the shape Emby's REST API reference gives for its route (https://dev.emby.media/reference/RestAPI.html, read on 24 November 2026), with made-up values: documentation addresses, two open movies by their own TMDB and IMDb ids, and invented ids for the server, its users and its items.

| File | Route | Shape in the reference |
|---|---|---|
| `system_info.json` | `GET /emby/System/Info` | `SystemInfo` |
| `virtual_folders.json` | `GET /emby/Library/VirtualFolders/Query` | `QueryResult_VirtualFolderInfo` |
| `users.json` | `GET /emby/Users/Query` | `QueryResult_UserDto` |
| `items_movies.json` | `GET /emby/Items?ParentId=<Movies>&IncludeItemTypes=Movie&Recursive=true` | `QueryResult_BaseItemDto` |
| `sessions.json` | `GET /emby/Sessions` | `SessionInfo[]`: a movie that transcodes, an episode in direct play, and an idle session |

Emby takes the API key in the `X-Emby-Token` header (https://dev.emby.media/doc/restapi/API-Key-Authentication.html). Its item ids are numbers written as text; its user and server ids are 32 hexadecimal digits.
