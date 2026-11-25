-- The metadata service (DESIGN.md, API: the routes the interface needs). TMDB's answers are kept
-- by the path they were asked with: a title, a season, a person or a collection for a week, a
-- list or a search for a day. A refresh that fails serves the stored answer. `body` is null
-- when TMDB had no such record. The metadata module owns the table.
-- metadata -----------------------------------------------------------------------------------
CREATE TABLE metadata_cache (
    key        text PRIMARY KEY,
    fetched_at timestamptz NOT NULL DEFAULT now(),
    body       jsonb
);
CREATE INDEX metadata_cache_fetched ON metadata_cache (fetched_at);
