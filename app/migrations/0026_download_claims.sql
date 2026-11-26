-- What the managers claim of the downloads (platform design, section 4.8, step B9b): each arr's
-- queue as last read and the download ids of its grab history. The queue job of an arr replaces
-- its queue rows with each read. The history job adds the ids of the pages it reads. The
-- downloads job of each client reads the claims of the ids it lists. A download id is a torrent's
-- hash or a usenet client's job id, upper case. A queue holds one row per download id: the first
-- item of the read that names it, as the owner is judged by it. seen_at is when a claim was first
-- stored. A history read stops at the first page that holds a grab stored by a complete read.
CREATE TABLE download_claims (
    manager         text NOT NULL,
    source          text NOT NULL CHECK (source IN ('queue', 'history')),
    download_id     text NOT NULL CHECK (download_id = upper(download_id)),
    -- The queue item, for a claim of the queue.
    queue_id        bigint,
    engine_title_id bigint,
    title           text,
    status          text,
    tracked_state   text,
    tracked_status  text,
    messages        text[] NOT NULL DEFAULT '{}',
    indexer         text,
    seen_at         timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (manager, source, download_id),
    CHECK ((source = 'queue') = (queue_id IS NOT NULL))
);

-- The downloads job asks by download id.
CREATE INDEX download_claims_by_id ON download_claims (download_id);
