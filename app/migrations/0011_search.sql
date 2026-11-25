-- Search (the search module owns these tables). The built-in provider searches its own
-- copy of the library's documents: movies, shows, collections, albums and artists, written
-- in full by the index pass. A Meilisearch, when one is configured, gets the same documents;
-- this copy stays the fallback.

-- search -------------------------------------------------------------------------------------
CREATE TABLE search_documents (
    id         text PRIMARY KEY,
    -- movie, show, collection, album, artist
    kind       text NOT NULL,
    -- The title in lower case, for exact and prefix matches.
    title      text NOT NULL,
    -- Every searchable word in lower case: title, original title, other names, year.
    words      text NOT NULL,
    words_tsv  tsvector GENERATED ALWAYS AS (to_tsvector('simple', words)) STORED,
    -- The document as written, returned as it is.
    body       jsonb NOT NULL,
    indexed_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX search_documents_kind ON search_documents (kind);
CREATE INDEX search_documents_tsv ON search_documents USING gin (words_tsv);

-- Typo tolerance where the trigram extension can be had (it ships with PostgreSQL; creating
-- it needs the right on the database). Without it search matches words and prefixes only.
DO $$
BEGIN
    CREATE EXTENSION IF NOT EXISTS pg_trgm;
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'pg_trgm is not available (%): built-in search runs without typo tolerance', SQLERRM;
END
$$;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm') THEN
        EXECUTE 'CREATE INDEX search_documents_trgm ON search_documents USING gin (words gin_trgm_ops)';
    END IF;
END
$$;

-- What each provider was last given: documents, when, and how it went.
CREATE TABLE search_state (
    provider     text PRIMARY KEY,
    documents    bigint NOT NULL DEFAULT 0,
    last_indexed timestamptz,
    -- ok, warn, down
    state        text NOT NULL,
    detail       text,
    updated_at   timestamptz NOT NULL DEFAULT now()
);
