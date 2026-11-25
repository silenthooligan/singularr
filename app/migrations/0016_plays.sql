-- Plays: who played what, when, on which device and how it was delivered (the plays module
-- owns these tables). One row per play, whichever sources told of it: the media server's
-- sessions while they play, its own history, and Tautulli. Each source's own record is kept
-- beside the play in `raw`.

-- plays --------------------------------------------------------------------------------------
CREATE TABLE plays (
    id                bigserial PRIMARY KEY,
    -- The media server integration the play was on.
    server            text NOT NULL,
    -- What: the server's key of the item and of an episode's show (so an item the catalog
    -- cannot place still counts), the catalog title it belongs to, and the item as named.
    rating_key        text,
    show_key          text,
    title_id          bigint REFERENCES titles (id) ON DELETE SET NULL,
    -- movie, episode, track, or the source's word.
    kind              text NOT NULL,
    title             text NOT NULL,
    show_title        text,
    season            integer,
    episode           integer,
    year              integer,
    -- The server's section, by its key.
    library_key       text,
    -- Who: the media server's account.
    account_id        bigint,
    account_name      text,
    -- When, and how much: seconds played with pauses left out, seconds paused, the item's
    -- own length, and how far it got (0 to 100).
    started_at        timestamptz NOT NULL,
    stopped_at        timestamptz,
    watched_secs      integer,
    paused_secs       integer,
    duration_secs     integer,
    progress          double precision,
    -- Where: the player's name, device, platform and app; on the home network or outside;
    -- the address only while the setting keeps addresses.
    player            text,
    device            text,
    platform          text,
    product           text,
    local             boolean,
    address           text,
    -- How: direct play, direct stream or transcode, for the play and its video and audio;
    -- the file's resolution and bitrate (kbit/s) and what was sent.
    decision          text,
    video_decision    text,
    audio_decision    text,
    source_resolution text,
    source_bitrate    bigint,
    stream_resolution text,
    stream_bitrate    bigint,
    -- The sources that know it, each by its own id, and when the server marked it played.
    session_id        text,
    history_id        bigint,
    tautulli_id       bigint,
    viewed_at         timestamptz,
    -- {"live": {...}, "server": {...}, "tautulli": {...}}: each source's record as it gave it.
    raw               jsonb NOT NULL DEFAULT '{}',
    first_seen_at     timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX plays_session ON plays (server, session_id) WHERE session_id IS NOT NULL;
CREATE UNIQUE INDEX plays_history ON plays (server, history_id) WHERE history_id IS NOT NULL;
CREATE UNIQUE INDEX plays_tautulli ON plays (server, tautulli_id) WHERE tautulli_id IS NOT NULL;
CREATE INDEX plays_started ON plays (started_at DESC, id DESC);
CREATE INDEX plays_item ON plays (server, rating_key, account_id, started_at);
CREATE INDEX plays_account ON plays (account_id, started_at DESC);
CREATE INDEX plays_title ON plays (title_id) WHERE title_id IS NOT NULL;
CREATE INDEX plays_unlinked ON plays ((COALESCE(show_key, rating_key))) WHERE title_id IS NULL;

-- Where the reading of each source stands: the newest id read (`<integration>:history`).
CREATE TABLE play_sources (
    source  text PRIMARY KEY,
    newest  bigint,
    read_at timestamptz NOT NULL DEFAULT now()
);
