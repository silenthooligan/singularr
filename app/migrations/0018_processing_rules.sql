-- Processing rules: Marquee's own rules that send a file to a flow of a processing service (the
-- processing module owns these tables). A rule is kept with every version it had; what it sent,
-- or would have sent, is logged file by file, so a rule can be watched before it is trusted.

-- processing ---------------------------------------------------------------------------------
CREATE TABLE processing_rules (
    id          bigserial PRIMARY KEY,
    name        text NOT NULL,
    -- An off rule sends nothing; what it would have sent is logged all the same.
    enabled     boolean NOT NULL DEFAULT false,
    -- import: for every file that is imported; request: for the files that match when a person
    -- asks.
    run_on      text NOT NULL CHECK (run_on IN ('import', 'request')),
    -- What the file must be: the policy's condition language, the libraries, the traits.
    conditions  jsonb NOT NULL DEFAULT '{}',
    -- The processing service (an integration id) and its flow.
    service     text NOT NULL,
    flow        text NOT NULL,
    version     integer NOT NULL DEFAULT 1,
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    updated_by  text NOT NULL,
    -- A removed rule stays for its log and its versions, and is never evaluated again.
    removed_at  timestamptz
);

-- Every version of a rule as it was saved.
CREATE TABLE processing_rule_versions (
    rule_id  bigint NOT NULL REFERENCES processing_rules (id) ON DELETE CASCADE,
    version  integer NOT NULL,
    body     jsonb NOT NULL,
    saved_at timestamptz NOT NULL DEFAULT now(),
    saved_by text NOT NULL,
    PRIMARY KEY (rule_id, version)
);

-- What a rule did with a file it matched: shadow (the rule is off), held (it is on and could
-- not send: the detail says why), skipped (sent before, protected, or what processing made),
-- sent, refused (the service said no), failed.
CREATE TABLE processing_rule_log (
    id           bigserial PRIMARY KEY,
    rule_id      bigint NOT NULL REFERENCES processing_rules (id) ON DELETE CASCADE,
    rule_version integer NOT NULL,
    at           timestamptz NOT NULL DEFAULT now(),
    -- The import the file came with; null for a run on request.
    event_id     bigint,
    title_id     bigint REFERENCES titles (id) ON DELETE SET NULL,
    copy_id      bigint,
    path         text NOT NULL,
    service      text NOT NULL,
    flow         text NOT NULL,
    outcome      text NOT NULL
                 CHECK (outcome IN ('shadow', 'held', 'skipped', 'sent', 'refused', 'failed')),
    detail       text,
    -- Why the file matched, in the condition language's words.
    evidence     text,
    -- The run a send opened, when the service named its job.
    run_id       text
);

CREATE INDEX processing_rule_log_rule ON processing_rule_log (rule_id, at DESC);
-- One file is sent once, by any rule.
CREATE UNIQUE INDEX processing_rule_log_sent_once ON processing_rule_log (path)
    WHERE outcome = 'sent';

-- Where the rules pass stands in the journal: the last event it read.
CREATE TABLE processing_rules_cursor (
    id       integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    event_id bigint NOT NULL
);
