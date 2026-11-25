-- Marquee Requests, R1 (shadow). The public face runs as its own database role,
-- `marquee_requests` (deploy/requests-role.sql), which reads only the `face_*` views below and
-- inserts only into `request_intents`, `request_sessions` and `request_session_ends`. The
-- full process writes everything else. The requests module owns every table and view here.
--
-- Who a face request is about comes from the database, not from the face: the face puts the
-- raw session token in the transaction setting `marquee.session_token`, and the views and the
-- intent trigger hash it and look the session up. The raw token is never stored.

-- The accounts of both Seerr sites, synced read-only by the full process: who may sign in
-- (a Plex account plexseerr knows) and which Seerr account a request goes out as. No address.
CREATE TABLE seerr_users (
    site             text NOT NULL,
    seerr_user_id    bigint NOT NULL,
    display_name     text NOT NULL,
    plex_id          bigint,
    plex_username    text,
    jellyfin_user_id text,
    user_type        integer,
    -- One of the owner's accounts (MARQUEE_REQUEST_OWNERS, by site and Seerr user id).
    is_owner         boolean NOT NULL DEFAULT false,
    synced_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (site, seerr_user_id)
);

CREATE INDEX seerr_users_plex ON seerr_users (plex_id) WHERE plex_id IS NOT NULL;

-- The discover lists as the full process last read them from Seerr, so the face can show them
-- without a Seerr key.
CREATE TABLE discover_items (
    list          text NOT NULL,
    position      integer NOT NULL,
    kind          text NOT NULL CHECK (kind IN ('movie', 'show')),
    tmdb_id       bigint NOT NULL,
    title         text NOT NULL,
    year          integer,
    overview      text,
    rating        double precision,
    poster_path   text,
    backdrop_path text,
    fetched_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (list, position)
);

-- What the face says about the request rules and where the owner face is: one row, written by
-- the full process from its configuration.
CREATE TABLE face_settings (
    id          smallint PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    max_seasons integer NOT NULL,
    gated_sites text[] NOT NULL DEFAULT '{}',
    owner_url   text,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- A signed-in person on the face. token_sha256 is the hex SHA-256 of the cookie's token.
CREATE TABLE request_sessions (
    id              bigserial PRIMARY KEY,
    token_sha256    text NOT NULL UNIQUE CHECK (token_sha256 ~ '^[0-9a-f]{64}$'),
    plex_account_id bigint NOT NULL,
    display_name    text NOT NULL,
    site            text NOT NULL DEFAULT 'plex' CHECK (site = 'plex'),
    seerr_user_id   bigint NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz NOT NULL,
    CHECK (expires_at <= created_at + interval '31 days')
);

CREATE INDEX request_sessions_expires ON request_sessions (expires_at);

-- A session is only ever for a Plex account plexseerr knows: the Seerr account and the name
-- come from seerr_users, whatever the insert says, and a session lives at most 30 days.
CREATE FUNCTION request_sessions_from_account() RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
DECLARE
    u record;
BEGIN
    SELECT su.seerr_user_id, su.display_name INTO u FROM seerr_users su
    WHERE su.site = 'plex' AND su.plex_id = NEW.plex_account_id
    ORDER BY su.seerr_user_id LIMIT 1;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'this Plex account is not known to the Plex request site' USING ERRCODE = '28000';
    END IF;
    NEW.seerr_user_id := u.seerr_user_id;
    NEW.display_name := u.display_name;
    NEW.site := 'plex';
    NEW.created_at := now();
    IF NEW.expires_at IS NULL OR NEW.expires_at > now() + interval '30 days' THEN
        NEW.expires_at := now() + interval '30 days';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER request_sessions_from_account BEFORE INSERT ON request_sessions
    FOR EACH ROW EXECUTE FUNCTION request_sessions_from_account();

-- Sign-out: the face cannot change a session, it adds its end.
CREATE TABLE request_session_ends (
    token_sha256 text PRIMARY KEY CHECK (token_sha256 ~ '^[0-9a-f]{64}$'),
    at           timestamptz NOT NULL DEFAULT now()
);

-- A request asked for on the face: an intent. The full process picks it up, applies the
-- request rules, and in R1 records what it would send to Seerr (state `shadow`, the rule
-- outcome in `outcome`). It never calls Seerr. HD only: the face offers no 4K.
CREATE TABLE request_intents (
    id              bigserial PRIMARY KEY,
    plex_account_id bigint NOT NULL,
    display_name    text NOT NULL,
    site            text NOT NULL DEFAULT 'plex' CHECK (site = 'plex'),
    seerr_user_id   bigint NOT NULL,
    kind            text NOT NULL CHECK (kind IN ('movie', 'show')),
    tmdb_id         bigint NOT NULL CHECK (tmdb_id > 0),
    seasons         integer[] NOT NULL DEFAULT '{}',
    is_4k           boolean NOT NULL DEFAULT false CHECK (NOT is_4k),
    title           text,
    year            integer,
    created_at      timestamptz NOT NULL DEFAULT now(),
    state           text NOT NULL DEFAULT 'new' CHECK (state IN ('new', 'shadow', 'failed')),
    processed_at    timestamptz,
    outcome         jsonb,
    CHECK (kind = 'show' OR cardinality(seasons) = 0)
);

CREATE INDEX request_intents_new ON request_intents (id) WHERE state = 'new';
CREATE INDEX request_intents_account ON request_intents (plex_account_id, created_at DESC);

-- The session of the transaction's token: who is asking.
CREATE FUNCTION face_token_sha256() RETURNS text
    LANGUAGE sql STABLE
    AS $$ SELECT encode(sha256(convert_to(current_setting('marquee.session_token', true), 'UTF8')), 'hex') $$;

-- Every face insert into request_intents is about the session of its transaction: who, the
-- site and the Seerr account come from the session, whatever the insert says. Without a live
-- session it is refused. At most 30 intents an hour per person, whatever the face allows.
CREATE FUNCTION request_intents_from_session() RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
DECLARE
    s record;
    recent integer;
BEGIN
    SELECT rs.plex_account_id, rs.display_name, rs.site, rs.seerr_user_id INTO s
    FROM request_sessions rs
    WHERE rs.token_sha256 = face_token_sha256()
      AND rs.expires_at > now()
      AND NOT EXISTS (SELECT 1 FROM request_session_ends e WHERE e.token_sha256 = rs.token_sha256);
    IF NOT FOUND THEN
        RAISE EXCEPTION 'no signed-in session for this request' USING ERRCODE = '28000';
    END IF;
    SELECT count(*) INTO recent FROM request_intents
    WHERE plex_account_id = s.plex_account_id AND created_at > now() - interval '1 hour';
    IF recent >= 30 THEN
        RAISE EXCEPTION 'too many requests in the last hour' USING ERRCODE = '53400';
    END IF;
    NEW.plex_account_id := s.plex_account_id;
    NEW.display_name := s.display_name;
    NEW.site := s.site;
    NEW.seerr_user_id := s.seerr_user_id;
    NEW.is_4k := false;
    NEW.state := 'new';
    NEW.created_at := now();
    NEW.processed_at := NULL;
    NEW.outcome := NULL;
    RETURN NEW;
END $$;

CREATE TRIGGER request_intents_from_session BEFORE INSERT ON request_intents
    FOR EACH ROW EXECUTE FUNCTION request_intents_from_session();

-- Wake the full process: it listens on this channel and also polls.
CREATE FUNCTION request_intents_notify() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN PERFORM pg_notify('request_intents', NEW.id::text); RETURN NEW; END $$;

CREATE TRIGGER request_intents_notify AFTER INSERT ON request_intents
    FOR EACH ROW EXECUTE FUNCTION request_intents_notify();

-- The face's read models. Each is a security barrier view owned by the migration role, so the
-- face role needs no privilege on any table behind it.

-- The signed-in person of the transaction's token, while the session lives.
CREATE VIEW face_session WITH (security_barrier) AS
SELECT rs.plex_account_id, rs.display_name, rs.site, rs.seerr_user_id, rs.expires_at,
       COALESCE(u.is_owner, false) AS is_owner
FROM request_sessions rs
LEFT JOIN seerr_users u ON u.site = rs.site AND u.seerr_user_id = rs.seerr_user_id
WHERE rs.token_sha256 = face_token_sha256()
  AND rs.expires_at > now()
  AND NOT EXISTS (SELECT 1 FROM request_session_ends e WHERE e.token_sha256 = rs.token_sha256);

-- The plexseerr account of the Plex account in `marquee.plex_account`, set by the face after
-- plex.tv named the account: the sign-in check. At most one row; no list of accounts.
CREATE VIEW face_account WITH (security_barrier) AS
SELECT u.plex_id AS plex_account_id, u.display_name, u.seerr_user_id, u.is_owner
FROM seerr_users u
WHERE u.site = 'plex'
  AND u.plex_id IS NOT NULL
  AND u.plex_id = NULLIF(current_setting('marquee.plex_account', true), '')::bigint;

-- Per title only a state: `available` (a copy is held), `processing` (on the way: a download, or
-- a request Seerr approved), `requested` (a request waits), or no row. Never who asked, nor how
-- many. 4K requests do not count: the face is HD only.
CREATE VIEW face_availability WITH (security_barrier) AS
WITH held AS (
    SELECT t.kind, t.tmdb_id FROM titles t
    WHERE t.tmdb_id IS NOT NULL AND EXISTS (SELECT 1 FROM copies c WHERE c.title_id = t.id)
),
moving AS (
    SELECT t.kind, t.tmdb_id FROM downloads d JOIN titles t ON t.id = d.title_id
    WHERE t.tmdb_id IS NOT NULL AND d.progress < 1
    UNION
    SELECT r.kind, r.tmdb_id FROM requests r
    WHERE r.tmdb_id IS NOT NULL AND NOT r.is_4k AND r.status IN ('approved', 'processing')
),
waiting AS (
    SELECT r.kind, r.tmdb_id FROM requests r
    WHERE r.tmdb_id IS NOT NULL AND NOT r.is_4k AND r.status = 'pending'
),
keys AS (SELECT * FROM held UNION SELECT * FROM moving UNION SELECT * FROM waiting)
SELECT k.kind, k.tmdb_id,
       CASE WHEN EXISTS (SELECT 1 FROM held h WHERE h.kind = k.kind AND h.tmdb_id = k.tmdb_id) THEN 'available'
            WHEN EXISTS (SELECT 1 FROM moving m WHERE m.kind = k.kind AND m.tmdb_id = k.tmdb_id) THEN 'processing'
            ELSE 'requested' END AS state
FROM keys k;

-- The seasons of a show by TMDB id, each with only a state: `available` (episode files held),
-- `processing` or `requested` (as above, by the seasons a request names).
CREATE VIEW face_seasons WITH (security_barrier) AS
WITH held AS (
    SELECT t.tmdb_id, s.season_number FROM titles t JOIN seasons s ON s.title_id = t.id
    WHERE t.kind = 'show' AND t.tmdb_id IS NOT NULL
      AND EXISTS (SELECT 1 FROM copies c WHERE c.title_id = t.id AND c.season_number = s.season_number)
),
asked AS (
    SELECT r.tmdb_id, n AS season_number, r.status FROM requests r, unnest(r.seasons) AS n
    WHERE r.kind = 'show' AND r.tmdb_id IS NOT NULL AND NOT r.is_4k
      AND r.status IN ('pending', 'approved', 'processing')
)
SELECT tmdb_id, season_number,
       CASE WHEN bool_or(src = 'held') THEN 'available'
            WHEN bool_or(src IN ('approved', 'processing')) THEN 'processing'
            ELSE 'requested' END AS state
FROM (SELECT tmdb_id, season_number, 'held' AS src FROM held
      UNION ALL SELECT tmdb_id, season_number, status FROM asked) x
GROUP BY tmdb_id, season_number;

CREATE VIEW face_discover WITH (security_barrier) AS
SELECT list, position, kind, tmdb_id, title, year, overview, rating, poster_path, backdrop_path,
       fetched_at
FROM discover_items;

CREATE VIEW face_rules WITH (security_barrier) AS
SELECT max_seasons, gated_sites, owner_url FROM face_settings WHERE id = 1;

-- The signed-in person's own requests: what they asked here (intents, with what Marquee would
-- send) and their requests on the Plex site. Nobody else's.
CREATE VIEW face_my_requests WITH (security_barrier) AS
SELECT 'intent'::text AS source, i.id, i.kind, i.tmdb_id, i.seasons, i.title, i.year,
       i.created_at AS asked_at, i.state AS status, i.outcome -> 'gate' ->> 'result' AS gate,
       i.outcome -> 'gate' ->> 'reason' AS gate_reason, NULL::text AS poster_path, false AS is_4k
FROM request_intents i JOIN face_session s ON s.plex_account_id = i.plex_account_id
UNION ALL
SELECT 'seerr'::text, r.id, r.kind, r.tmdb_id, r.seasons, m.title, m.year, r.requested_at,
       r.status, NULL, NULL, m.poster_path, r.is_4k
FROM requests r
JOIN face_session s ON r.site = s.site AND r.requested_by_id = s.seerr_user_id
LEFT JOIN request_media m ON m.kind = r.kind AND m.tmdb_id = r.tmdb_id;
