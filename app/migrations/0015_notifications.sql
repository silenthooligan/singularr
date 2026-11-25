-- Notifications: channels and what was sent to them (the notify module owns both tables).
-- A channel's secrets (a webhook address, a token, a password) are in its row, apart from
-- the values an answer may show: write-only through the API, never in an answer, a journal
-- event or a log line.

-- notify ----------------------------------------------------------------------------------------
CREATE TABLE notification_channels (
    id          bigserial PRIMARY KEY,
    -- slack, discord, email, webhook, ntfy, telegram, pushover, gotify
    type        text NOT NULL,
    name        text NOT NULL,
    enabled     boolean NOT NULL DEFAULT true,
    -- The values an answer shows, by field.
    settings    jsonb NOT NULL DEFAULT '{}',
    -- The values it never shows, by field.
    secrets     jsonb NOT NULL DEFAULT '{}',
    -- The events it subscribes to, and the kinds it narrows to (none: every kind).
    events      text[] NOT NULL DEFAULT '{}',
    kinds       text[] NOT NULL DEFAULT '{}',
    -- The journal event the dispatcher has gone through for this channel: nothing at or
    -- before it is sent. A channel starts at the event that records its creation.
    last_event  bigint NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL DEFAULT now(),
    created_by  text NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    updated_by  text NOT NULL
);

CREATE TABLE notification_deliveries (
    id          bigserial PRIMARY KEY,
    channel_id  bigint NOT NULL REFERENCES notification_channels (id) ON DELETE CASCADE,
    -- The journal event it is for; NULL for a test.
    event_id    bigint,
    -- The notification event (file.upgraded), or test.
    event       text NOT NULL,
    -- What the message is made from: the event described, without a secret.
    notice      jsonb NOT NULL,
    -- pending: waiting for its next attempt; sending: an attempt is under way; sent; failed:
    -- three attempts were not taken, or a test was not.
    state       text NOT NULL CHECK (state IN ('pending', 'sending', 'sent', 'failed')),
    attempts    integer NOT NULL DEFAULT 0,
    next_at     timestamptz NOT NULL DEFAULT now(),
    created_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    -- The destination's status (HTTP), when it answered.
    status      integer,
    -- The destination's own words, or why nothing reached it.
    detail      text,
    test        boolean NOT NULL DEFAULT false,
    -- One message per event and channel.
    UNIQUE (channel_id, event_id)
);

CREATE INDEX notification_deliveries_due ON notification_deliveries (next_at)
    WHERE state = 'pending';
CREATE INDEX notification_deliveries_channel ON notification_deliveries (channel_id, id DESC);

-- processing ------------------------------------------------------------------------------------
-- When a run was first seen stuck, so that it is journaled once (the processing module owns
-- this column, as it owns the table).
ALTER TABLE processing_runs ADD COLUMN stuck_noted_at timestamptz;
