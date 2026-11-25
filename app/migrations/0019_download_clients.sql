-- Downloads from every download client Singularr reads (qBittorrent, Transmission, Deluge,
-- SABnzbd, NZBGet): torrents and usenet downloads side by side. Each row is the download of
-- one client. The lifecycle module owns this table.
--
-- `hash` stays the id the managers know a download by: a torrent's hash, a usenet client's job
-- id. The key is now the client and that id. Two clients can hold the same torrent. `item_id`
-- is the client's own id as it writes it. `status` is where the download stands in the
-- product's words, beside the client's own `state`.
--
-- Every row stored so far came from the one qBittorrent integration read before. Each is given
-- that integration's id: the engine whose state is of the kind `qbittorrent`, else the id of
-- the environment form's.
--
-- `absent_since` marks the rows of a client the configuration no longer lists. The start sets
-- them aside. Every read leaves them out. The client's return brings them back as they were.
-- They are removed after 30 days.

-- lifecycle ----------------------------------------------------------------------------------
ALTER TABLE downloads
    ADD COLUMN client       text,
    ADD COLUMN protocol     text NOT NULL DEFAULT 'torrent' CHECK (protocol IN ('torrent', 'usenet')),
    ADD COLUMN item_id      text,
    ADD COLUMN status       text,
    ADD COLUMN absent_since timestamptz;

UPDATE downloads SET client = coalesce(
    (SELECT engine FROM engine_state WHERE kind = 'qbittorrent'
     ORDER BY last_ok_at DESC NULLS LAST, engine LIMIT 1),
    'qbittorrent');

ALTER TABLE downloads ALTER COLUMN client SET NOT NULL;
ALTER TABLE downloads DROP CONSTRAINT downloads_pkey;
ALTER TABLE downloads ADD PRIMARY KEY (client, hash);
CREATE INDEX downloads_hash ON downloads (hash);

-- requests face ------------------------------------------------------------------------------
-- The portal reads a download in motion by the rule Activity reads it by: a torrent not all
-- there yet, a usenet download its client still works on. A row set aside is none.
CREATE OR REPLACE VIEW face_availability WITH (security_barrier) AS
WITH held AS (
    SELECT t.kind, t.tmdb_id FROM titles t
    WHERE t.tmdb_id IS NOT NULL AND EXISTS (SELECT 1 FROM copies c WHERE c.title_id = t.id)
),
moving AS (
    SELECT t.kind, t.tmdb_id FROM downloads d JOIN titles t ON t.id = d.title_id
    WHERE t.tmdb_id IS NOT NULL AND d.absent_since IS NULL
      AND CASE WHEN d.protocol = 'usenet'
               THEN coalesce(d.status NOT IN ('completed', 'failed'), true)
               ELSE d.progress < 1 END
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
