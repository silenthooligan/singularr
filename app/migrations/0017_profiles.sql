-- Quality profiles, lists, advanced rules and naming conventions as a person edits them
-- (marquee_core::profile::Model), kept whole, one row per saved version. The policy module owns
-- this table.
--
-- The newest row is the saved model. While the policy's source is `saved` (the setting
-- system.policySource), it is compiled into the policy in force. Every save adds a row and a
-- `policy_model_saved` event in the same transaction, so a change can be compared with the
-- version before it and undone by restoring an earlier one (which adds a row again). Rows are
-- never changed or removed.
CREATE TABLE policy_models (
    version   bigint PRIMARY KEY,
    body      jsonb NOT NULL,
    -- SHA-256 of the compiled policy text.
    hash      text NOT NULL,
    -- What the save did: profile_saved, profile_deleted, list_saved, list_deleted,
    -- naming_saved, imported, restored, seeded.
    action    text NOT NULL,
    -- What it was done to: profile:<id>, list:<id>, naming:<kind>, policy.
    subject   text NOT NULL,
    saved_by  text NOT NULL,
    saved_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX policy_models_saved_at ON policy_models (saved_at);
