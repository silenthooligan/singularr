-- The descriptive facts the arrs give in their title lists, and what each engine says about
-- a show's seasons. The sync already reads those lists; this stores more of what they say.

-- catalog ------------------------------------------------------------------------------------

ALTER TABLE titles
    ADD COLUMN overview        text,
    ADD COLUMN genres          text[] NOT NULL DEFAULT '{}',
    ADD COLUMN runtime_minutes integer,
    ADD COLUMN certification   text,
    -- Movies have a studio, shows have a network.
    ADD COLUMN studio          text,
    ADD COLUMN network         text,
    -- Movies: the collection the movie belongs to, by name.
    ADD COLUMN collection      text,
    -- 0 to 10. rating is TMDB's for a movie and the one Sonarr gives for a show;
    -- imdb_rating is IMDb's, which only Radarr gives.
    ADD COLUMN rating          double precision,
    ADD COLUMN imdb_rating     double precision,
    ADD COLUMN fanart_url      text,
    ADD COLUMN banner_url      text,
    -- Shows.
    ADD COLUMN first_aired     timestamptz,
    ADD COLUMN next_airing     timestamptz,
    -- Movies.
    ADD COLUMN in_cinemas      timestamptz,
    ADD COLUMN digital_release timestamptz;

-- What one engine says about a show: its episode counts, and the same per season as a list of
-- {season_number, monitored, episode_count, total_episode_count, episode_file_count,
-- size_bytes}. episode_count is the engine's own count toward completion (monitored and
-- aired, or held); total_episode_count is every episode it knows of. file_count, which was
-- already here, is the number of episodes with a file. Null and empty for a movie.
ALTER TABLE title_engines
    ADD COLUMN episode_count       integer,
    ADD COLUMN total_episode_count integer,
    ADD COLUMN seasons             jsonb NOT NULL DEFAULT '[]';

-- A season of a title over all engines: each count is the largest any engine reports. The
-- rows are rebuilt from title_engines.seasons whenever an engine's word on the title changes.
ALTER TABLE seasons
    ADD COLUMN total_episode_count integer NOT NULL DEFAULT 0,
    ADD COLUMN episode_file_count  integer NOT NULL DEFAULT 0;
