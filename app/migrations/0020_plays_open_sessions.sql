-- A session's id names its play only while that play is open. Jellyfin and Emby keep one session
-- per connected player, and a play's id is the session and its item together: the same pair comes
-- back when the item is played again on that player, days later. A play that ended keeps the id
-- as a record of where it came from, and a new play may take it up.
DROP INDEX plays_session;
CREATE UNIQUE INDEX plays_session ON plays (server, session_id)
    WHERE session_id IS NOT NULL AND stopped_at IS NULL;
