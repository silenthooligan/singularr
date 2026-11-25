-- How a download moves, as the download client last reported it, for the Activity page: bytes
-- per second, seconds left, and the seeds and peers it is connected to. Kept only while the
-- download is not complete, so a seeding torrent's row does not change with every pass. And the
-- indexer the release came from, as a manager's queue names it.

-- lifecycle ----------------------------------------------------------------------------------
ALTER TABLE downloads
    ADD COLUMN speed_bytes integer,
    ADD COLUMN eta_seconds integer,
    ADD COLUMN seeds       integer,
    ADD COLUMN peers       integer,
    ADD COLUMN indexer     text;
