-- Marquee stage 1: the catalog read from the four arrs and qBittorrent, the journal,
-- API keys and the policy versions. Each table has one owner module (see DESIGN.md,
-- "SQL stays home"); the owner is named above each table.

-- catalog ------------------------------------------------------------------------------------

CREATE TABLE libraries (
    id           text PRIMARY KEY,
    name         text NOT NULL,
    root_folders text[] NOT NULL DEFAULT '{}',
    updated_at   timestamptz NOT NULL DEFAULT now()
);

INSERT INTO libraries (id, name) VALUES ('plex', 'Plex'), ('jellyfin', 'Jellyfin');

-- One row per title, whatever the number of libraries that hold it. ext_key is the identity
-- shared by the engines: movie:tmdb:<id> or show:tvdb:<id>.
CREATE TABLE titles (
    id                bigserial PRIMARY KEY,
    ext_key           text NOT NULL UNIQUE,
    kind              text NOT NULL CHECK (kind IN ('movie', 'show')),
    tmdb_id           bigint,
    tvdb_id           bigint,
    imdb_id           text,
    title             text NOT NULL,
    sort_title        text NOT NULL,
    year              integer,
    original_language text,
    poster_url        text,
    status            text,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX titles_kind_sort ON titles (kind, sort_title);
CREATE INDEX titles_tmdb ON titles (tmdb_id);
CREATE INDEX titles_tvdb ON titles (tvdb_id);
CREATE INDEX titles_imdb ON titles (imdb_id);

-- What one engine says about one title (the arr's own id, folder, profile and tags).
CREATE TABLE title_engines (
    engine            text NOT NULL,
    engine_title_id   bigint NOT NULL,
    title_id          bigint NOT NULL REFERENCES titles (id) ON DELETE CASCADE,
    library_id        text NOT NULL REFERENCES libraries (id),
    path              text,
    monitored         boolean NOT NULL DEFAULT false,
    profile           text,
    lane              text,
    tags              text[] NOT NULL DEFAULT '{}',
    status            text,
    added_at          timestamptz,
    file_count        integer NOT NULL DEFAULT 0,
    size_bytes        bigint NOT NULL DEFAULT 0,
    facts_hash        text NOT NULL DEFAULT '',
    files_synced_at   timestamptz,
    files_synced_count integer,
    updated_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (engine, engine_title_id)
);

CREATE INDEX title_engines_title ON title_engines (title_id);
CREATE INDEX title_engines_library ON title_engines (library_id);

CREATE TABLE seasons (
    id             bigserial PRIMARY KEY,
    title_id       bigint NOT NULL REFERENCES titles (id) ON DELETE CASCADE,
    season_number  integer NOT NULL,
    episode_count  integer NOT NULL DEFAULT 0,
    UNIQUE (title_id, season_number)
);

CREATE TABLE episodes (
    id             bigserial PRIMARY KEY,
    title_id       bigint NOT NULL REFERENCES titles (id) ON DELETE CASCADE,
    season_number  integer NOT NULL,
    episode_number integer NOT NULL,
    name           text,
    UNIQUE (title_id, season_number, episode_number)
);

-- A copy is one file of one title in one library.
CREATE TABLE copies (
    id                 bigserial PRIMARY KEY,
    title_id           bigint NOT NULL REFERENCES titles (id) ON DELETE CASCADE,
    library_id         text NOT NULL REFERENCES libraries (id),
    engine             text NOT NULL,
    engine_title_id    bigint NOT NULL,
    engine_file_id     bigint NOT NULL,
    lane               text,
    season_number      integer,
    episode_numbers    integer[] NOT NULL DEFAULT '{}',
    relative_path      text NOT NULL,
    scene_name         text,
    original_path      text,
    release_name       text NOT NULL,
    name_source        text NOT NULL,
    size_bytes         bigint NOT NULL DEFAULT 0,
    date_added         timestamptz,
    arr_quality        text,
    arr_group          text,
    arr_edition        text,
    arr_proper         boolean NOT NULL DEFAULT false,
    arr_repack         boolean NOT NULL DEFAULT false,
    arr_custom_formats text[] NOT NULL DEFAULT '{}',
    arr_score          integer,
    languages          text[] NOT NULL DEFAULT '{}',
    media_info         jsonb,
    -- parsed: the name as written. held: the release as scored (the arr's record first).
    parsed             jsonb NOT NULL,
    held               jsonb NOT NULL,
    verdict            jsonb,
    mq_quality         text,
    mq_group           text,
    mq_edition         text,
    mq_score           integer,
    diff_quality       boolean NOT NULL DEFAULT false,
    diff_group         boolean NOT NULL DEFAULT false,
    diff_edition       boolean NOT NULL DEFAULT false,
    diff_score         boolean NOT NULL DEFAULT false,
    protections        text[] NOT NULL DEFAULT '{}',
    arr_only_formats   text[] NOT NULL DEFAULT '{}',
    mq_only_traits     text[] NOT NULL DEFAULT '{}',
    assessed_with      text NOT NULL DEFAULT '',
    facts_hash         text NOT NULL DEFAULT '',
    first_seen_at      timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (engine, engine_file_id)
);

CREATE INDEX copies_title ON copies (title_id);
CREATE INDEX copies_engine_title ON copies (engine, engine_title_id);
CREATE INDEX copies_lane ON copies (lane);
CREATE INDEX copies_disagree ON copies (engine)
    WHERE diff_quality OR diff_group OR diff_edition OR diff_score;
CREATE INDEX copies_assessed_with ON copies (assessed_with);

-- lifecycle ----------------------------------------------------------------------------------

CREATE TABLE downloads (
    hash            text PRIMARY KEY,
    name            text NOT NULL,
    category        text,
    state           text NOT NULL,
    progress        double precision NOT NULL DEFAULT 0,
    size_bytes      bigint NOT NULL DEFAULT 0,
    added_at        timestamptz,
    completed_at    timestamptz,
    seeding_seconds bigint NOT NULL DEFAULT 0,
    save_path       text,
    owner           text,
    owner_source    text,
    title_id        bigint REFERENCES titles (id) ON DELETE SET NULL,
    queue_status    text,
    queue_state     text,
    queue_health    text,
    queue_messages  jsonb NOT NULL DEFAULT '[]',
    stuck           boolean NOT NULL DEFAULT false,
    stuck_reason    text,
    orphan          boolean NOT NULL DEFAULT false,
    orphan_reason   text,
    first_seen_at   timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX downloads_owner ON downloads (owner);
CREATE INDEX downloads_attention ON downloads (hash) WHERE stuck OR orphan;

-- engines ------------------------------------------------------------------------------------

CREATE TABLE engine_state (
    engine         text PRIMARY KEY,
    kind           text NOT NULL,
    library_id     text,
    ok             boolean NOT NULL DEFAULT false,
    version        text,
    last_sync_at   timestamptz,
    last_ok_at     timestamptz,
    last_error     text,
    latency_ms     integer,
    -- The last 20 round-trip times, oldest first.
    latency_history integer[] NOT NULL DEFAULT '{}',
    counts         jsonb NOT NULL DEFAULT '{}',
    failures       integer NOT NULL DEFAULT 0,
    updated_at     timestamptz NOT NULL DEFAULT now()
);

-- core: the journal and API keys -----------------------------------------------------------------

CREATE TABLE events (
    id            bigserial PRIMARY KEY,
    at            timestamptz NOT NULL DEFAULT now(),
    kind          text NOT NULL,
    subject_type  text NOT NULL,
    subject_id    text NOT NULL,
    actor         text NOT NULL,
    cause         bigint,
    body          jsonb NOT NULL DEFAULT '{}'
);

CREATE INDEX events_kind ON events (kind, id DESC);
CREATE INDEX events_subject ON events (subject_type, subject_id, id DESC);
CREATE INDEX events_title ON events ((body ->> 'title_id'), id DESC);

CREATE TABLE api_keys (
    id          bigserial PRIMARY KEY,
    name        text NOT NULL,
    prefix      text NOT NULL UNIQUE,
    sha256      text NOT NULL,
    scopes      text[] NOT NULL,
    created_by  text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    last_used   timestamptz,
    revoked_at  timestamptz
);

-- policy -------------------------------------------------------------------------------------

CREATE TABLE policy_versions (
    hash       text PRIMARY KEY,
    name       text NOT NULL,
    version    text NOT NULL,
    source     text NOT NULL,
    loaded_at  timestamptz NOT NULL DEFAULT now(),
    text       text NOT NULL
);
