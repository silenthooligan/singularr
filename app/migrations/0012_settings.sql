-- Settings saved in the interface (PATCH /api/v1/settings). A value that the environment or
-- the instance file fixes is never stored here: it is read from there, and the interface shows
-- it read-only with its source. `scope` is `instance` for the whole installation, or
-- `user:<who>` for one person's own value (their appearance, say). The journal holds every
-- change as a `setting_changed` event.
CREATE TABLE settings (
    scope    text NOT NULL CHECK (scope = 'instance' OR scope LIKE 'user:%'),
    key      text NOT NULL CHECK (key ~ '^[a-z][a-zA-Z0-9]*(\.[a-z][a-zA-Z0-9]*)+$'),
    value    jsonb NOT NULL,
    saved_by text NOT NULL,
    saved_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scope, key)
);
