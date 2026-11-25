-- The database role of Singularr Requests, the viewer portal (SINGULARR_FACE=requests).
--
-- Run once, connected to the `singularr` database as a role that may create roles and grant on
-- its objects (the database owner, or postgres), after the full Singularr process has started
-- once and applied its migrations:
--
--   psql -v ON_ERROR_STOP=1 -d singularr -f deploy/requests-role.sql
--
-- Then give the role its password, outside this file and outside git:
--
--   ALTER ROLE singularr_requests PASSWORD '...';
--
-- and put the login in SINGULARR_REQUESTS_DATABASE_URL. Running the file again is safe.
--
-- What the role can do, and nothing else:
--   SELECT on the face's read models, the `face_*` views: the signed-in session, the sign-in
--          check for one Plex account, per title only a state (available, processing,
--          requested), the seasons of a show with a state each, the discover lists, the
--          request rules, and the signed-in person's own requests.
--   INSERT into request_intents (kind, TMDB id, seasons, title, year: who it is for comes
--          from the session, by a trigger), request_sessions (a new sign-in, for a Plex
--          account Seerr knows: the Seerr account comes from seerr_users, by a trigger)
--          and request_session_ends (sign-out).
-- It reads no table: not copies, api_keys, events, requests (with requested_by), seerr_users,
-- request_sessions, the music tables or the home movies tables.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'singularr_requests') THEN
        CREATE ROLE singularr_requests LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT
            NOREPLICATION NOBYPASSRLS CONNECTION LIMIT 10;
    END IF;
END $$;

-- Start from nothing, then grant.
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM singularr_requests;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM singularr_requests;
DO $$
BEGIN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO singularr_requests', current_database());
END $$;
GRANT USAGE ON SCHEMA public TO singularr_requests;

GRANT SELECT ON face_session, face_account, face_availability, face_seasons, face_discover,
    face_rules, face_my_requests TO singularr_requests;

GRANT INSERT (kind, tmdb_id, seasons, title, year) ON request_intents TO singularr_requests;
GRANT USAGE ON SEQUENCE request_intents_id_seq TO singularr_requests;
GRANT INSERT (token_sha256, plex_account_id, expires_at) ON request_sessions TO singularr_requests;
GRANT USAGE ON SEQUENCE request_sessions_id_seq TO singularr_requests;
GRANT INSERT (token_sha256) ON request_session_ends TO singularr_requests;
