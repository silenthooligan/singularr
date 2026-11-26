-- The act outbox (platform design, sections 4.5 to 4.7). An act is what Singularr asks a service
-- to do. It is a row here before it is sent. Its key names the request: a second request with the
-- key is the same act. Its open key holds while Singularr does not know whether the service has
-- the act (pending, sending or unknown): a second act with it is that act. A worker sends it by
-- compare and set. The answer settles it: sent, failed or unknown. A read confirms one that was
-- sent or is unknown.
CREATE TABLE acts (
    id              bigserial PRIMARY KEY,
    integration     text NOT NULL,
    -- An action the integration's type declares: 'send'.
    action          text NOT NULL,
    idempotency_key text NOT NULL UNIQUE,
    -- Held while the act is pending, sending or unknown: 'send:<integration>:<path>:<flow>'.
    open_key        text,
    -- SHA-256 of one document: the integration, the action and the request in its canonical
    -- form (`outbox::request_hash`). An act made below from a claim of 0028 holds the SHA-256 of
    -- its request's text alone: its key names that claim. No later request takes the key.
    request_hash    text NOT NULL,
    -- What is sent, secrets left out.
    request         jsonb NOT NULL,
    subject_type    text,
    subject_id      text,
    state           text NOT NULL DEFAULT 'pending'
                    CHECK (state IN ('pending', 'sending', 'sent', 'unknown', 'confirmed', 'failed',
                                     'cancelled')),
    -- Its sends so far.
    attempts        integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    -- What the service answered. A read's finding is under `confirmed`.
    result          jsonb,
    error           text,
    error_class     text,
    -- Who asked: 'user:<address>', 'key:<name>', 'rule:<id>'.
    actor           text NOT NULL,
    -- The event that led to it.
    caused_by       bigint,
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- When its last send went out.
    sent_at         timestamptz,
    -- When it was confirmed, failed or cancelled.
    settled_at      timestamptz,
    updated_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX acts_open ON acts (state, created_at)
    WHERE state IN ('pending', 'sending', 'sent', 'unknown');
CREATE INDEX acts_subject ON acts (subject_type, subject_id, id DESC);
CREATE INDEX acts_integration ON acts (integration, action, id DESC);
CREATE UNIQUE INDEX acts_open_key ON acts (open_key)
    WHERE open_key IS NOT NULL AND state IN ('pending', 'sending', 'unknown');

-- A processing rule's claim and its act are one thing: the claim names its act. The act's changes
-- settle the claim. A confirmed act is sent. A clear no frees the file. An unknown act keeps it.
-- The claims of 0028 in `sending` got no clear answer: each becomes an act in `unknown`. The act
-- holds the open key of its file and flow. The job that claimed a file is no longer needed for a
-- look. Nothing settles a claim by its service's runs now: 0028's index for that goes.
ALTER TABLE processing_rule_log
    ADD COLUMN act_id bigint REFERENCES acts (id) ON DELETE SET NULL;

INSERT INTO acts (integration, action, idempotency_key, open_key, request_hash, request,
                  subject_type, subject_id, state, attempts, error, error_class, actor, caused_by,
                  created_at, sent_at)
SELECT l.service, 'send',
       'rule:' || l.rule_id || ':' || COALESCE(l.copy_id::text, 'none') || ':' || l.id,
       'send:' || l.service || ':' || l.path || ':' || l.flow,
       encode(sha256(convert_to(r.request::text, 'UTF8')), 'hex'), r.request,
       CASE WHEN l.title_id IS NULL THEN NULL ELSE 'title' END, l.title_id::text,
       'unknown', 1, COALESCE(l.detail, 'No clear answer'), 'unknown', 'rule:' || l.rule_id,
       l.event_id, l.at, l.at
FROM processing_rule_log l
CROSS JOIN LATERAL (
    SELECT jsonb_build_object(
        'path', l.path,
        'flow', l.flow,
        'title', (SELECT jsonb_build_object('id', t.id, 'kind', t.kind, 'tmdb_id', t.tmdb_id,
                                            'title', t.title, 'year', t.year)
                  FROM titles t WHERE t.id = l.title_id)
    ) AS request
) r
WHERE l.outcome = 'sending';

UPDATE processing_rule_log l SET act_id = a.id
FROM acts a
WHERE l.outcome = 'sending'
  AND a.idempotency_key = 'rule:' || l.rule_id || ':' || COALESCE(l.copy_id::text, 'none') || ':'
                          || l.id;

CREATE INDEX processing_rule_log_act ON processing_rule_log (act_id) WHERE act_id IS NOT NULL;
DROP INDEX processing_rule_log_sending;

ALTER TABLE processing_rule_log DROP COLUMN job_id;
