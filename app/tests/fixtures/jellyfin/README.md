# Jellyfin fixtures

Answers of Jellyfin's public demo server (https://demo.jellyfin.org/stable, Jellyfin 12.4.0), read on 24 November 2026 with the demo account and GET alone. They are trimmed to a few records. No test calls a real server: the tests read these files.

| File | What it is |
|---|---|
| `system_info.json` | `GET /System/Info`, as the demo answered |
| `users.json` | `GET /Users`, as the demo answered |
| `user_views.json` | `GET /UserViews?userId=`, the demo account's libraries, as the demo answered |
| `virtual_folders.json` | Synthesized. `GET /Library/VirtualFolders` needs an administrator's key. The demo account was refused (403). The file has the shape of `VirtualFolderInfo` in Jellyfin's OpenAPI document, with the names and ids of `user_views.json` and the folders the demo's items lie in |
| `items_movies.json` | `GET /Items?ParentId=<Movies>&IncludeItemTypes=Movie&Recursive=true&Fields=ProviderIds,Path,MediaSources,DateCreated,Genres&Limit=3`: the first 3 of 11 movies, their media streams trimmed to the main fields |
| `items_shows.json`, `items_artists.json`, `items_albums.json`, `items_tracks.json` | The same for shows, music artists, albums and the tracks of one album, as the demo answered |
| `sessions.json` | `GET /Sessions`: two sessions that played (an episode and a movie, both direct play) and two idle ones. The players' addresses and device ids are documentation values in place of other visitors' own |
| `sessions_idle.json` | The two idle sessions of `sessions.json` |
| `sessions_transcode.json` | Synthesized. A session that transcodes: a real session of the demo, a movie of its library and the transcoding block the demo answered for another session, with the play method and the video stream changed to a transcode and a player on the home network |
| `image_primary.jpg` | `GET /Items/<id>/Images/Primary?maxWidth=64&quality=60`, as the demo answered |

Jellyfin 12.4.0 refused a key sent in `X-Emby-Token` (401) and a sign-in with `X-Emby-Authorization` (400). It took both in `Authorization: MediaBrowser ...`, where the adapter sends its key.

The shapes were checked against Jellyfin's OpenAPI document, https://api.jellyfin.org/openapi/jellyfin-openapi-stable.json (version 12.4.0, 1.9 MB, not kept here).
