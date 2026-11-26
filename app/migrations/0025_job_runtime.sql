-- The job runtime (platform design, section 4). A schedule says how often a handler runs and on
-- which queue. A job is one run: queued, running under a lease, then done, failed, dead or
-- cancelled. One schedule has one queued or running job at a time. A marker keeps what a pass
-- knows across starts: the time of a whole read, a cursor.
CREATE TABLE job_schedules (
    key          text PRIMARY KEY,
    kind         text NOT NULL,
    integration  text,
    queue        text NOT NULL,
    every_secs   integer NOT NULL CHECK (every_secs > 0),
    floor_secs   integer NOT NULL CHECK (floor_secs > 0),
    jitter_secs  integer NOT NULL DEFAULT 0 CHECK (jitter_secs >= 0),
    lease_secs   integer NOT NULL DEFAULT 120 CHECK (lease_secs > 0),
    enabled      boolean NOT NULL DEFAULT true,
    next_run_at  timestamptz NOT NULL,
    failures     integer NOT NULL DEFAULT 0,
    last_run_at  timestamptz,
    last_ok_at   timestamptz,
    last_error   text,
    last_detail  text,
    last_took_ms integer,
    source       text NOT NULL DEFAULT 'default'
                 CHECK (source IN ('default', 'environment', 'file', 'saved')),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX job_schedules_due ON job_schedules (next_run_at) WHERE enabled;

CREATE TABLE jobs (
    id              bigserial PRIMARY KEY,
    kind            text NOT NULL,
    queue           text NOT NULL,
    schedule_key    text REFERENCES job_schedules (key) ON DELETE SET NULL,
    integration     text,
    payload         jsonb NOT NULL DEFAULT '{}',
    idempotency_key text,
    state           text NOT NULL DEFAULT 'queued'
                    CHECK (state IN ('queued', 'running', 'done', 'failed', 'dead', 'cancelled')),
    priority        smallint NOT NULL DEFAULT 0,
    run_at          timestamptz NOT NULL DEFAULT now(),
    attempts        integer NOT NULL DEFAULT 0,
    max_attempts    integer NOT NULL DEFAULT 5 CHECK (max_attempts > 0),
    locked_by       text,
    locked_until    timestamptz,
    last_error      text,
    error_class     text,
    result          jsonb,
    caused_by       bigint,
    created_at      timestamptz NOT NULL DEFAULT now(),
    started_at      timestamptz,
    finished_at     timestamptz,
    CHECK ((state = 'running') = (locked_until IS NOT NULL))
);
CREATE INDEX jobs_ready ON jobs (queue, priority DESC, run_at, id) WHERE state = 'queued';
CREATE INDEX jobs_leases ON jobs (locked_until) WHERE state = 'running';
CREATE UNIQUE INDEX jobs_one_per_schedule ON jobs (schedule_key) WHERE state IN ('queued', 'running');
CREATE UNIQUE INDEX jobs_idempotency ON jobs (kind, idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX jobs_finished ON jobs (finished_at) WHERE finished_at IS NOT NULL;
CREATE INDEX jobs_schedule ON jobs (schedule_key, id DESC);

CREATE TABLE markers (
    scope      text NOT NULL,
    name       text NOT NULL,
    value      jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scope, name)
);
