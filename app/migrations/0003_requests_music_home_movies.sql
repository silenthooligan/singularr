-- Three sections that had no data: the requests on both Seerr sites, the music library and
-- the home movies. Stage 1 reads them; the home movies also take the owner's annotations,
-- the one place stage 1 writes, and only to these tables. Each table has one owner module.

-- requests -----------------------------------------------------------------------------------

-- One request on one Seerr site, as the last pass read it. requested_by is a display name,
-- never an email address. status is the one word the Requests page shows, derived from
-- request_status and media_status (Seerr's two codes, as words).
CREATE TABLE requests (
    id               bigserial PRIMARY KEY,
    site             text NOT NULL,
    seerr_id         bigint NOT NULL,
    kind             text NOT NULL CHECK (kind IN ('movie', 'show')),
    tmdb_id          bigint,
    tvdb_id          bigint,
    is_4k            boolean NOT NULL DEFAULT false,
    request_status   text NOT NULL,
    media_status     text NOT NULL,
    status           text NOT NULL,
    requested_by     text NOT NULL,
    requested_by_id  bigint,
    requested_at     timestamptz NOT NULL,
    seerr_updated_at timestamptz,
    seasons          integer[] NOT NULL DEFAULT '{}',
    -- The catalog title, when Marquee holds it (by TMDB id, then TVDB id).
    title_id         bigint REFERENCES titles (id) ON DELETE SET NULL,
    first_seen_at    timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (site, seerr_id)
);

CREATE INDEX requests_status ON requests (status);
CREATE INDEX requests_tmdb ON requests (kind, tmdb_id);
CREATE INDEX requests_requested_at ON requests (requested_at DESC);
CREATE INDEX requests_title ON requests (title_id);

-- What Seerr says a requested title is. The request list carries only ids, so each title is
-- looked up once and kept here for both sites. A failed lookup is kept with its error and
-- asked again a day later.
CREATE TABLE request_media (
    kind          text NOT NULL CHECK (kind IN ('movie', 'show')),
    tmdb_id       bigint NOT NULL,
    title         text,
    year          integer,
    poster_path   text,
    backdrop_path text,
    tvdb_id       bigint,
    lookup_error  text,
    looked_up_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (kind, tmdb_id)
);

-- music --------------------------------------------------------------------------------------

-- One artist folder under the music root.
CREATE TABLE music_artists (
    id            bigserial PRIMARY KEY,
    folder        text NOT NULL UNIQUE,
    name          text NOT NULL,
    sort_name     text NOT NULL,
    first_seen_at timestamptz NOT NULL DEFAULT now()
);

-- One album folder (Artist/Album (Year)). name_key is the name in lower case with single
-- spaces: two folders of one artist with the same key are duplicates. cover_path is relative
-- to the music root.
CREATE TABLE music_albums (
    id            bigserial PRIMARY KEY,
    artist_id     bigint NOT NULL REFERENCES music_artists (id) ON DELETE CASCADE,
    folder        text NOT NULL,
    name          text NOT NULL,
    name_key      text NOT NULL,
    year          integer,
    track_count   integer NOT NULL DEFAULT 0,
    size_bytes    bigint NOT NULL DEFAULT 0,
    formats       text[] NOT NULL DEFAULT '{}',
    cover_path    text,
    other_files   integer NOT NULL DEFAULT 0,
    facts_hash    text NOT NULL DEFAULT '',
    first_seen_at timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (artist_id, folder)
);

CREATE INDEX music_albums_name_key ON music_albums (artist_id, name_key);

-- One audio file of an album. file_name is relative to the album folder (a disc folder
-- included). Only names and sizes are read; tags and audio are not.
CREATE TABLE music_tracks (
    id           bigserial PRIMARY KEY,
    album_id     bigint NOT NULL REFERENCES music_albums (id) ON DELETE CASCADE,
    file_name    text NOT NULL,
    track_number integer,
    title        text,
    format       text NOT NULL,
    size_bytes   bigint NOT NULL DEFAULT 0,
    UNIQUE (album_id, file_name)
);

-- The last scan of the music folder: one row.
CREATE TABLE music_scan (
    id           smallint PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    root         text NOT NULL,
    ok           boolean NOT NULL,
    last_scan_at timestamptz NOT NULL,
    last_ok_at   timestamptz,
    took_ms      bigint,
    error        text,
    counts       jsonb NOT NULL DEFAULT '{}'
);

-- home movies --------------------------------------------------------------------------------

-- One tape: one video file under the home movies root (path is relative to it). read_* is what
-- the file name says and is written by every scan. annotation holds what the owner set
-- (title, years, uncertain, chapters, people, places, private, notes) and wins over the
-- reading. A tape whose file is gone keeps its row and its annotation, with missing_since set.
-- A tape that was never annotated is in the inbox.
CREATE TABLE home_movies (
    id             bigserial PRIMARY KEY,
    path           text NOT NULL UNIQUE,
    size_bytes     bigint NOT NULL DEFAULT 0,
    modified_at    timestamptz,
    read_title     text NOT NULL,
    read_year_from integer,
    read_year_to   integer,
    read_uncertain boolean NOT NULL DEFAULT true,
    read_chapters  jsonb NOT NULL DEFAULT '[]',
    annotation     jsonb NOT NULL DEFAULT '{}',
    annotated_at   timestamptz,
    missing_since  timestamptz,
    first_seen_at  timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

-- The last scan of the home movies folder: one row.
CREATE TABLE home_movie_scan (
    id           smallint PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    root         text NOT NULL,
    ok           boolean NOT NULL,
    last_scan_at timestamptz NOT NULL,
    last_ok_at   timestamptz,
    took_ms      bigint,
    error        text,
    counts       jsonb NOT NULL DEFAULT '{}'
);
