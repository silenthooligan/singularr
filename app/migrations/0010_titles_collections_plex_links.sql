-- The title record as rich as the interface asks: the collection a movie belongs to by its
-- TMDB id (so the collections the library has a part of can be listed), a movie's original
-- title (for search), and the Plex server's machine identifier (so a title's Plex link can be
-- built). The metadata cache forgets the show records asked before shows were asked with
-- their aggregate credits.

-- catalog ------------------------------------------------------------------------------------
-- Radarr gives both; Sonarr gives neither. Filled by the next titles pass.
ALTER TABLE titles
    ADD COLUMN collection_tmdb_id bigint,
    ADD COLUMN original_title     text;
CREATE INDEX titles_collection_tmdb ON titles (collection_tmdb_id)
    WHERE collection_tmdb_id IS NOT NULL;

-- The libraries of a Plex server carry its machine identifier, written by the server pass.
ALTER TABLE libraries
    ADD COLUMN machine_id text;

-- metadata -----------------------------------------------------------------------------------
-- A show record is now asked with `aggregate_credits` (every season's cast), which is another
-- key; the answers asked with `credits` are never read again.
DELETE FROM metadata_cache WHERE key LIKE '/tv/%?append_to_response=credits,%';
