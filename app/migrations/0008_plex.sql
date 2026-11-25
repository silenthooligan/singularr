-- The Plex server, read-only (DESIGN.md, Engines: MediaServer). Three read models from it:
-- its library sections as libraries, what it says about each catalog title (Rotten Tomatoes
-- ratings, plays, the edition), and its music (artists, albums, tracks). Nothing is deleted.
-- catalog ------------------------------------------------------------------------------------
-- A library from the server: its section key, the folders as the server names them, and the
-- Radarr or Sonarr integration that manages it (none for music and home video).
ALTER TABLE libraries
    ADD COLUMN section_key text,
    ADD COLUMN managed_by  text,
    ADD COLUMN locations   text[] NOT NULL DEFAULT '{}';
-- One server item matched to a catalog title by its TMDB, TVDB or IMDb id. Ratings are 0 to 10
-- with the image key that names their source (`rottentomatoes://image.rating.ripe`).
CREATE TABLE title_server (
    server          text NOT NULL,
    rating_key      text NOT NULL,
    title_id        bigint NOT NULL REFERENCES titles (id) ON DELETE CASCADE,
    library_id      text,
    critic_rating   double precision,
    critic_image    text,
    audience_rating double precision,
    audience_image  text,
    plays           bigint NOT NULL DEFAULT 0,
    last_played     timestamptz,
    edition         text,
    added_at        timestamptz,
    seen_at         timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (server, rating_key)
);
CREATE INDEX title_server_title ON title_server (title_id);
-- music --------------------------------------------------------------------------------------
-- The server's music, replaced by every server pass. The folder scan stays to report the
-- files the server does not know.
CREATE TABLE server_artists (
    server     text NOT NULL,
    rating_key text NOT NULL,
    name       text NOT NULL,
    genres     text[] NOT NULL DEFAULT '{}',
    countries  text[] NOT NULL DEFAULT '{}',
    summary    text,
    thumb      text,
    added_at   timestamptz,
    PRIMARY KEY (server, rating_key)
);
CREATE TABLE server_albums (
    server     text NOT NULL,
    rating_key text NOT NULL,
    artist_key text,
    artist     text,
    title      text NOT NULL,
    year       integer,
    released   text,
    genres     text[] NOT NULL DEFAULT '{}',
    label      text,
    thumb      text,
    added_at   timestamptz,
    plays      bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (server, rating_key)
);
CREATE INDEX server_albums_artist ON server_albums (server, artist_key);
CREATE TABLE server_tracks (
    server      text NOT NULL,
    rating_key  text NOT NULL,
    album_key   text NOT NULL,
    number      integer,
    disc        integer,
    title       text NOT NULL,
    artist      text,
    duration_ms bigint,
    codec       text,
    bitrate     bigint,
    channels    bigint,
    container   text,
    size_bytes  bigint,
    file        text,
    PRIMARY KEY (server, rating_key)
);
CREATE INDEX server_tracks_album ON server_tracks (server, album_key);
