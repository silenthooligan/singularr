-- The processing rules claim a file before they send it (B9c). A row of the log in `sending`
-- commits before the call. The row names the job that claims the file. The call's answer then
-- settles it: sent, held, refused or failed. An answer that is not a clear no keeps the row in
-- `sending`: the file is never sent again on its own. The processing pass settles such a row as
-- sent once the service shows the run the send opened. One file holds one claim or one send, by
-- any rule.
ALTER TABLE processing_rule_log DROP CONSTRAINT processing_rule_log_outcome_check;
ALTER TABLE processing_rule_log ADD CONSTRAINT processing_rule_log_outcome_check
    CHECK (outcome IN ('shadow', 'held', 'skipped', 'sending', 'sent', 'refused', 'failed'));
-- The job of the runtime that claimed the file. Null for a rule run from the API.
ALTER TABLE processing_rule_log ADD COLUMN job_id bigint;
DROP INDEX processing_rule_log_sent_once;
CREATE UNIQUE INDEX processing_rule_log_sent_once ON processing_rule_log (path)
    WHERE outcome IN ('sending', 'sent');
-- The claims each processing pass looks to settle by the service's runs.
CREATE INDEX processing_rule_log_sending ON processing_rule_log (service)
    WHERE outcome = 'sending';

-- Where the rules pass stands in the journal moves into the markers: rules.cursor, as
-- {"id": <the last event read>}.
INSERT INTO markers (scope, name, value)
SELECT 'singularr', 'rules.cursor', jsonb_build_object('id', event_id) FROM processing_rules_cursor
ON CONFLICT (scope, name) DO NOTHING;
DROP TABLE processing_rules_cursor;
