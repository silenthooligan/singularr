-- What a manager said of its files' custom formats (Radarr's /moviefile), kept between titles
-- passes (platform design, section 4.9, step B9b). One row per file of the manager. The key holds
-- the facts of the file the formats were said about: its path, its size and the time it was
-- added. A file whose facts changed is asked about again. The marker formats.refreshed_at says
-- when every file was last asked about. The titles job writes what a pass changed in the
-- transaction that ends it.
CREATE TABLE manager_file_formats (
    manager        text NOT NULL,
    engine_file_id bigint NOT NULL,
    key            text NOT NULL,
    formats        text[] NOT NULL DEFAULT '{}',
    score          integer,
    updated_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (manager, engine_file_id)
);
