-- How each pass of each integration went: one row per integration and pass. The passes are the
-- loops' own: titles, queue and history (managers), downloads (download clients), server and
-- plays (media servers), requests (Seerr), processing (processing services), plays (Tautulli).
-- An integration is up while every pass of it is. It is down with the error of its worst pass.
-- A later pass that succeeds no longer hides an earlier one that failed. At start the rows of the
-- passes the process does not run are removed.
--
-- engine_state keeps what the passes found (version, latency, counts). It is written as before
-- until every reader of it has moved.
CREATE TABLE integration_health (
    integration  text NOT NULL,
    pass         text NOT NULL,
    ok           boolean NOT NULL,
    last_run_at  timestamptz NOT NULL DEFAULT now(),
    last_ok_at   timestamptz,
    last_error   text,
    -- Failed runs in a row.
    failures     integer NOT NULL DEFAULT 0,
    -- The round trip of the last run that succeeded, in milliseconds.
    latency_ms   integer,
    PRIMARY KEY (integration, pass)
);
