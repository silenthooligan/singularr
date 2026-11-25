-- Editions, stage 1 (read only). Every copy belongs to one edition of rules/editions.toml;
-- a title lists the editions it holds, which it wants and which one a library plays first;
-- the catalog's folder scan lists the files in a movie's folder that no engine tracks. The
-- catalog module owns all three tables.

-- catalog ------------------------------------------------------------------------------------

-- The edition the copy belongs to: a key of rules/editions.toml or `other:<slug>`, its label,
-- whether the name or the record names it (false: the default edition), and its place in the
-- list's order of precedence (for sorting).
ALTER TABLE copies
    ADD COLUMN edition_key    text    NOT NULL DEFAULT 'theatrical',
    ADD COLUMN edition_label  text    NOT NULL DEFAULT 'Theatrical',
    ADD COLUMN edition_marked boolean NOT NULL DEFAULT false,
    ADD COLUMN edition_rank   integer NOT NULL DEFAULT 0;

CREATE INDEX copies_title_edition ON copies (title_id, edition_key);

-- A video file in a movie's folder that the engine holding the folder does not track: another
-- edition already on disk, or a leftover. Listed, never acted on. `path` is the folder as the
-- engine names it plus the file name.
CREATE TABLE untracked_files (
    id             bigserial PRIMARY KEY,
    title_id       bigint NOT NULL REFERENCES titles (id) ON DELETE CASCADE,
    engine         text NOT NULL,
    library_id     text NOT NULL REFERENCES libraries (id),
    path           text NOT NULL,
    file_name      text NOT NULL,
    size_bytes     bigint NOT NULL DEFAULT 0,
    modified_at    timestamptz,
    edition_key    text NOT NULL,
    edition_label  text NOT NULL,
    edition_marked boolean NOT NULL DEFAULT false,
    edition_rank   integer NOT NULL DEFAULT 0,
    resolution     text,
    first_seen_at  timestamptz NOT NULL DEFAULT now(),
    seen_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (engine, path)
);

CREATE INDEX untracked_files_title ON untracked_files (title_id);

-- The editions of a title. A row whose set_by is null is worked out from what is held (every
-- edition a copy or an untracked file belongs to, wanted, the default edition the one a library
-- plays first) and follows it. A row the owner set (set_by 'owner', stage 2) is kept as it is.
CREATE TABLE title_editions (
    title_id   bigint NOT NULL REFERENCES titles (id) ON DELETE CASCADE,
    key        text NOT NULL,
    label      text NOT NULL,
    wanted     boolean NOT NULL DEFAULT true,
    is_default boolean NOT NULL DEFAULT false,
    rank       integer NOT NULL DEFAULT 0,
    set_by     text,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (title_id, key)
);

CREATE UNIQUE INDEX title_editions_one_default ON title_editions (title_id) WHERE is_default;
CREATE INDEX title_editions_key ON title_editions (key);

-- The last scan of the movie folders: one row.
CREATE TABLE movie_folder_scan (
    id           smallint PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    roots        text[] NOT NULL DEFAULT '{}',
    ok           boolean NOT NULL,
    last_scan_at timestamptz NOT NULL,
    last_ok_at   timestamptz,
    took_ms      bigint,
    error        text,
    counts       jsonb NOT NULL DEFAULT '{}'
);

-- Backfill from what is known: the editions of each copy as held (the arr's record and the
-- name together, as the parser named them). The first start of a build with this migration
-- assesses every copy again with rules/editions.toml, which settles any difference; this only
-- makes the answers right from the first request on. The names are the parser's, in the
-- list's order of precedence; a named cut outside the list is `other:<slug>`, ranked just
-- before IMAX as `others_before` says.
CREATE TEMPORARY TABLE edition_names (name text PRIMARY KEY, key text, label text, rank integer);
INSERT INTO edition_names (name, key, label, rank) VALUES
    ('director''s cut',   'directors-cut',    'Director''s Cut',     1),
    ('final cut',         'final-cut',        'Final Cut',           3),
    ('extended',          'extended',         'Extended',            5),
    ('unrated',           'unrated',          'Unrated',             7),
    ('uncut',             'uncut',            'Uncut',               9),
    ('uncensored',        'uncut',            'Uncut',               9),
    ('ultimate',          'ultimate',         'Ultimate',           11),
    ('black and chrome',  'black-and-chrome', 'Black and Chrome',   13),
    ('alternate cut',     'alternate-cut',    'Alternate Cut',      15),
    ('special edition',   'special-edition',  'Special Edition',    17),
    ('anniversary',       'anniversary',      'Anniversary',        19),
    ('collector''s',      'collectors',       'Collector''s Edition', 21),
    ('imax',              'imax',             'IMAX',               23),
    ('imax enhanced',     'imax',             'IMAX',               23),
    ('open matte',        'open-matte',       'Open Matte',         25),
    ('remastered',        'remastered',       'Remastered',         27),
    ('4k remaster',       'remastered',       'Remastered',         27),
    ('restored',          'restored',         'Restored',           29),
    ('criterion',         'criterion',        'Criterion',          31),
    ('theatrical',        'theatrical',       'Theatrical',         33);

WITH named AS (
    SELECT c.id, e.ord,
           COALESCE(n.key, 'other:' || trim(both '-' from regexp_replace(
               lower(regexp_replace(e.name, '[''`]', '', 'g')), '[^a-z0-9]+', '-', 'g'))) AS key,
           COALESCE(n.label, e.name) AS label,
           COALESCE(n.rank, 22) AS rank
    FROM copies c
    CROSS JOIN LATERAL jsonb_array_elements_text(
        CASE WHEN jsonb_typeof(c.held -> 'editions') = 'array' THEN c.held -> 'editions' ELSE '[]' END
    ) WITH ORDINALITY AS e(name, ord)
    LEFT JOIN edition_names n ON n.name = lower(e.name)
),
picked AS (
    SELECT DISTINCT ON (id) id, key, label, rank FROM named
    WHERE key <> 'other:' ORDER BY id, rank, ord
)
UPDATE copies c
SET edition_key = p.key, edition_label = p.label, edition_marked = true, edition_rank = p.rank
FROM picked p
WHERE c.id = p.id;

UPDATE copies SET edition_rank = 33 WHERE NOT edition_marked;

DROP TABLE edition_names;

-- The editions of each movie: every edition held, wanted; the default the Theatrical when it
-- is held, else the held edition first in the list's order.
INSERT INTO title_editions (title_id, key, label, wanted, is_default, rank)
SELECT DISTINCT ON (c.title_id, c.edition_key) c.title_id, c.edition_key, c.edition_label, true, false, c.edition_rank
FROM copies c JOIN titles t ON t.id = c.title_id
WHERE t.kind = 'movie'
ORDER BY c.title_id, c.edition_key, c.id;

UPDATE title_editions te SET is_default = true
FROM (
    SELECT DISTINCT ON (title_id) title_id, key FROM title_editions
    ORDER BY title_id, (key <> 'theatrical'), rank, key
) d
WHERE te.title_id = d.title_id AND te.key = d.key;
