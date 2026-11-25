-- Processing: what the processing services did to each file after its import (the processing
-- module owns this table). One row per job of a service, written by the processing pass from
-- the service's own history and kept after the service no longer lists the job.

-- processing ---------------------------------------------------------------------------------
CREATE TABLE processing_runs (
    -- <integration id>:<the service's job id>
    id              text PRIMARY KEY,
    integration     text NOT NULL,
    job_id          text NOT NULL,
    -- The flow, by the service's id of it, and what started the job, in the service's words.
    flow            text NOT NULL,
    trigger_text    text NOT NULL DEFAULT '',
    -- staged, verifying, imported, failed, refused, pruned, superseded. A staged run that a
    -- worker is on is shown as working; that is joined when it is read, never stored.
    state           text NOT NULL,
    -- The service's own times: when the job started, when it last changed, when it closed.
    created_at      timestamptz NOT NULL,
    updated_at      timestamptz,
    finished_at     timestamptz,
    -- Where the file waits for a worker.
    queue_path      text,
    source_path     text,
    source_size     bigint,
    -- What the worker made, before it is imported.
    output_path     text,
    output_size     bigint,
    -- The imported file.
    result_path     text,
    result_size     bigint,
    -- The manager's id of the imported file, when the service names it.
    manager_file_id bigint,
    -- The files the result replaced: [{path, size, still_there}].
    replaced        jsonb NOT NULL DEFAULT '[]',
    -- Why the job was refused or failed, in the service's words.
    detail          text,
    -- [{at, event, actor, note}], oldest first.
    events          jsonb NOT NULL DEFAULT '[]',
    -- The job exactly as the service gave it.
    raw             jsonb NOT NULL,
    -- The title whose folder the source or the result lies in, and the episode the file name
    -- reads as.
    title_id        bigint REFERENCES titles (id) ON DELETE SET NULL,
    season_number   integer,
    episode_number  integer,
    -- A digest of what the service said about the job, so a pass rewrites only what changed.
    fingerprint     text NOT NULL DEFAULT '',
    first_seen_at   timestamptz NOT NULL DEFAULT now(),
    -- The last pass that listed the job, to within ten minutes.
    last_seen_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX processing_runs_state ON processing_runs (state);
CREATE INDEX processing_runs_title ON processing_runs (title_id) WHERE title_id IS NOT NULL;
CREATE INDEX processing_runs_created ON processing_runs (created_at DESC);
