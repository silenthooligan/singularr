-- Integrations are configuration, and this version is for Plex only (DESIGN.md, Product and
-- instance). Two changes to the read models; the events journal keeps all of its history.
--
-- 1. The rows of the Jellyfin side go: what the second Radarr and Sonarr (`radarrjelly`,
--    `sonarrjelly`, library `jellyfin`) and the Jellyfin Seerr site reported. That is their
--    copies, untracked files and title links, the titles only they held, their downloads,
--    the requests and accounts of the Jellyfin site, and their engine state. The server also
--    applies the general rule at every start (rows of an engine the configuration does not
--    list are removed); this migration is the known case, so the first start is right at once.
-- 2. A library belongs to an integration. Until libraries come from the Plex server, each
--    Radarr or Sonarr integration has a library of its own with the integration's id, so the
--    library `plex` becomes `radarr` and `sonarr`. A library row also says its kind and its
--    integration. The Seerr integration of the environment form is `seerr` now.
-- catalog ------------------------------------------------------------------------------------
DELETE FROM copies WHERE engine IN ('radarrjelly', 'sonarrjelly') OR library_id = 'jellyfin';
DELETE FROM untracked_files WHERE engine IN ('radarrjelly', 'sonarrjelly') OR library_id = 'jellyfin';
DELETE FROM title_engines WHERE engine IN ('radarrjelly', 'sonarrjelly') OR library_id = 'jellyfin';
-- Every title comes from an engine's list: a title no engine holds any more goes, with its
-- seasons, episodes and editions. Requests and downloads that named it keep their rows.
DELETE FROM titles t WHERE NOT EXISTS (SELECT 1 FROM title_engines te WHERE te.title_id = t.id);
-- Worked-out editions that no file holds any more go; a title left without a default takes
-- the Theatrical when it holds it, else its first edition in the list's order.
DELETE FROM title_editions te
WHERE te.set_by IS NULL
  AND NOT EXISTS (SELECT 1 FROM copies c WHERE c.title_id = te.title_id AND c.edition_key = te.key)
  AND NOT EXISTS (SELECT 1 FROM untracked_files u WHERE u.title_id = te.title_id AND u.edition_key = te.key);
UPDATE title_editions te SET is_default = true
FROM (
    SELECT DISTINCT ON (t.title_id) t.title_id, t.key FROM title_editions t
    WHERE NOT EXISTS (SELECT 1 FROM title_editions d WHERE d.title_id = t.title_id AND d.is_default)
    ORDER BY t.title_id, (t.key <> 'theatrical'), t.rank, t.key
) d
WHERE te.title_id = d.title_id AND te.key = d.key;

ALTER TABLE libraries
    ADD COLUMN kind        text,
    ADD COLUMN integration text;
-- One library per arr integration, named by its id until the configuration names it.
INSERT INTO libraries (id, name, kind, integration)
SELECT DISTINCT te.engine, te.engine, t.kind, te.engine
FROM title_engines te JOIN titles t ON t.id = te.title_id
ON CONFLICT (id) DO NOTHING;
INSERT INTO libraries (id, name, integration)
SELECT DISTINCT c.engine, c.engine, c.engine FROM copies c
ON CONFLICT (id) DO NOTHING;
INSERT INTO libraries (id, name, integration)
SELECT DISTINCT u.engine, u.engine, u.engine FROM untracked_files u
ON CONFLICT (id) DO NOTHING;
UPDATE title_engines SET library_id = engine WHERE library_id <> engine;
UPDATE copies SET library_id = engine WHERE library_id <> engine;
UPDATE untracked_files SET library_id = engine WHERE library_id <> engine;
DELETE FROM libraries l
WHERE l.id IN ('plex', 'jellyfin')
  AND NOT EXISTS (SELECT 1 FROM title_engines te WHERE te.library_id = l.id)
  AND NOT EXISTS (SELECT 1 FROM copies c WHERE c.library_id = l.id)
  AND NOT EXISTS (SELECT 1 FROM untracked_files u WHERE u.library_id = l.id);
UPDATE libraries l SET root_folders = COALESCE((
    SELECT array_agg(DISTINCT regexp_replace(te.path, '[/\\][^/\\]+[/\\]?$', ''))
    FROM title_engines te WHERE te.library_id = l.id AND te.path IS NOT NULL), '{}');

-- lifecycle ----------------------------------------------------------------------------------
DELETE FROM downloads WHERE owner IN ('radarrjelly', 'sonarrjelly');

-- requests -----------------------------------------------------------------------------------
-- A request's `site` is the media server it is for; `plex` is the one this version serves.
DELETE FROM requests WHERE site = 'jellyfin';
DELETE FROM seerr_users WHERE site = 'jellyfin';
ALTER TABLE seerr_users DROP COLUMN jellyfin_user_id;

-- engines ------------------------------------------------------------------------------------
DELETE FROM engine_state
WHERE engine IN ('radarrjelly', 'sonarrjelly', 'jellyseerr') OR library_id = 'jellyfin';
UPDATE engine_state SET engine = 'seerr'
WHERE engine = 'plexseerr' AND NOT EXISTS (SELECT 1 FROM engine_state WHERE engine = 'seerr');
UPDATE engine_state SET library_id = CASE WHEN kind IN ('radarr', 'sonarr') THEN engine END;
