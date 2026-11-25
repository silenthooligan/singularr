# singularr-core working notes

Notes from the parser, policy and corpus work on branch `core` (22 Nov 2026). They record what was found and decided, so the next change starts from facts.

## How to check

```
cargo test -p singularr-core
cargo clippy -p singularr-core --all-targets -- -D warnings
cargo fmt --all -- --check
cargo build -p singularr-core --target wasm32-unknown-unknown
cargo run -p singularr-core --example corpus_report        # rewrites docs/corpus-report.md, a few minutes in a debug build
cargo run -p singularr-core --example parse_name -- "Cam.2018.1080p.NF.WEB-DL.DDP5.1.x264-NTG"
```

`corpus_report --dump <dir>` also writes every disagreement as TSV (`parse.tsv`, `scores.tsv`, `replay.tsv`).

## Results on 22 Nov 2026 (docs/corpus-report.md has the detail)

- Parse, raw agreement with the arr record: movies quality 96.4%, group 74.1%, edition 97.8%; episodes quality 81.0%, group 100%, season and episode 100%. Every disagreement is in a bucket where Singularr is right or the name is ambiguous. None is left as a parser miss.
- Scene names only (Radarr): quality 99.5%, group 93.3%. The low movie group rate comes from the pipeline's relabeled files.
- Episode scores against Sonarr's own: 87.1% exact with media info, 99.8% within 100. 300 files were scored by Sonarr from a name the export lacks. Without them: 2,173 of 2,194, 99.0%.
- The replay of the arrs' custom formats gives Sonarr's real score for all 2,224 episode files whose scored name the export carries.
- Movie scores against that replay, as recorded: movies-hd 97.0% exact (99.7% within 100), movies-uhd 97.6%, jellyfin-movies 98.8%. All 131 differing files are explained: 123 by faults or gaps in the arr's patterns, 8 by a name that disagrees with the file or has no anchor.

## Corpus facts (fixtures/)

- `radarr_movies.jsonl`: 4,470 movies, 4,328 with a file. 2,480 have `sceneName`, 1,750 only `originalFilePath`, 98 neither.
- Every Radarr and Radarr (Jellyfin) movie file has `customFormats: []` and `customFormatScore: null`. Movie scores cannot be compared with Radarr's own numbers. The report replays Radarr's custom formats instead (see below).
- `sonarr_episodefiles_sample.jsonl`: 2,494 files, 966 with `sceneName`, none with `originalFilePath` (the Sonarr API does not return it). All carry scores.
- The arr scores a held file's title rules against the scene name, else the original file name, else the current file name, and also against the current file name. The current file name carries media info tokens (`[EAC3 5.1][x265]`), which is how a silent scene name still earns "5.1 Surround".
- Source, resolution and remux rules use the file's recorded quality, not the title. Group rules use the recorded group.
- Custom format logic: specs are grouped by type. In each group every required spec must pass and at least one spec must pass. All groups must pass. So "title X" plus "group X" (both optional) means title AND group.
- 300 Sonarr files with no scene name match a streaming service that their library name does not show: Sonarr scored them from the original file name, which the export lacks. No reader of the export can reproduce those scores.
- About 930 movie files have a pipeline name (`-[SHB]`, `-[SHR]`, `-[SHT]`) whose group the record no longer has (relabels of 12 Oct and 1 Nov 2026). 128 pipeline names state `BluRay` for a file the record has as WEB or Remux.
- 105 scene names end `x265 RARBG ORARBG` (an indexer rewrote `x265-RARBG`). `ORARBG` is treated as an uploader tag.

## Faults found in the arrs' own rules (worth telling the owner)

- **Unanchored group patterns.** `ImE` (a QxR member) is a bare pattern, so the QxR format also matches the groups SHOWTiME, OLDTiME and Anime Land. Six Warehouse 13 episodes score 1550 in Sonarr instead of 50, and movies from those groups get QxR's 1500 in Radarr.
- **The low channel count exclusion** of the surround formats, `[^0-9][1-4][ .][0-1]`, matches `AV1 10bit`. Every AV1 pipeline file loses its 5.1 or 7.1 points in Radarr (93 files). It also hits a sequel number in front of a resolution (`Taken 3 1080p`).
- **Title words.** "Special Edition" matches `Special` in Midnight Special, "IMAX" matches Climax.
- Radarr stores the DirtyHippie group as `60fps-DirtyHippie`. The policy lists both spellings in the trait and in the protection.

## Parser decisions

- The title ends at the last year before the first strong technical token, or at an episode marker, or at the first strong technical token. Strong tokens are resolutions, unambiguous sources and video codecs. Words such as Cam, TS, Screener, Proper, Extended, Web are never strong.
- Offsets in `ParseNote.at` index `ParsedRelease.raw` (the trimmed input). The token is the text as written at that offset. A test checks this for every note of every test name.
- `HDR` alone reads as `HdrFormat::Hdr` (new variant), `HDR10` as `Hdr10`. The arr scores them differently (100 against 500 in the 1080p movie lane).
- DVD and SDTV imply 480p, a service tag with no source implies WEBDL. Each carries a note that says "implied".
- A name with a resolution and no source keeps `source = None`. Sonarr and Radarr assume HDTV there.
- RM4K and "4K Remaster" give editions `Remastered` and `4K Remaster`, never 2160p. `UHD` and `4K` give 2160p only when no `NNNNp` token is present.
- Group rules, in order: `-[GROUP]`, the text after the last dash (also spaced and long dashes), the last word of the last technical `( .. )` block, a trailing word after technical tokens, a trailing `[TAG]`, a leading `[TAG]`. A trailing `[TAG]` after a group is the collective. Two trailing bare words are group and collective (`Tigole QxR`).
- Not taken for a group: technical words, compound tails (`DL`, `HD`, `Ray`), a single letter, a word cut by `...`, the beginning of a technical word (`Com`, `10bi`), a site suffix (`.nu`), `RG`.
- Known hyphenated groups are a list (`D-Z0N3`, `UTR-HD`, `Fight-BB`, `Blu-Bits`), as are two-word groups (`Anime Land`).
- Before the year every word is a title word, so `Doctor Sleep Directors Cut 2019` has no edition. A name with no year and no technical token (`Skyfall IMAX Open Matte.mkv`) is all title.
- Anime absolute numbering is out of scope: `Show - 12 [1080p]` stays kind Unknown.

## Policy decisions

- `rules/policy.toml` was generated once from `fixtures/arr_config.json` by a script that is not in the repository. The file is now edited by hand. `tests/policy_file.rs` checks every lane's points against the arr export, so a drift from the arrs shows as a test failure until the arrs are retired.
- One trait per custom format, shared by the arrs where the meaning is the same. Where the arrs differ, the traits are split so each lane can reproduce its arr: `group.afm72` (in Radarr's QxR, Sonarr's own format, the Jellyfin UTR), the TRaSH tiers for movies and for TV, `video.hdr` (Radarr, HDR with no format named) and `video.hdr-any` (Sonarr).
- The quality groups `HQ-1080p` and `2160p` are exported by name only. Their members in the ladders are an assumption: the 1080p (or 2160p) Remux, Bluray, WEBDL, WEBRip and HDTV qualities.
- `Lane.cutoff_quality` carries the arr's cutoff quality for the two Jellyfin lanes. A held copy at that step or better whose score reached `cutoff_score` is final.
- Media info: conditions read the name first and the probe where the name is silent. For HDR each format is a fact of its own, so the probe adds formats the name does not mention. For the audio codec and channels the name wins. The arrs in effect take the union for every rule, so a held file whose name and probe disagree about the audio scores differently here.
- `language` matches only what the name states. `original_language` falls back to `EvalContext::languages`.
- Protections apply only when something is held. Radarr's required-term release profiles (DirtyHippie, open matte) also restrict a first grab for a tagged title with no file. The two ignored-term profiles are traits with -10000, which do stop a first grab.

## Editions (branch `next`, 22 Nov 2026)

- `rules/editions.toml` is the closed list: key, label, aliases, one default (`theatrical`), and `others_before`. `edition::EditionList::shipped()` is that file compiled in; the server reads the file at run time. A test checks that every name the parser can give (`release::EDITIONS`) has a key, so the two cannot drift.
- The parser still finds the edition words (after the title only, which is what keeps "Midnight Special", "The Final Cut (2004)", "Directors Cut (2016)" and "DC League of Super-Pets" out). The list decides which edition they are. `read_release` for a parsed name, `read_text` for Radarr's edition field and `{edition-..}` labels, `read_copy` for a held copy (the name with the arr's edition, the arr's text where the name reads to nothing).
- One edition per copy. With several names the list's order decides: cuts, then an unknown named cut (`other:<slug>`, label as written), then presentations, transfers, labels, the default last. `DC RM4K` is a Director's Cut that is also `remastered`; `Ulysses Cut RM4K` is `other:ulysses-cut`. Remastered and Criterion are editions of their own, as Radarr and Plex treat them, so a plain copy does not compete with a held remaster.
- New parser case: `Black and Chrome`, `Black & Chrome Edition`.
- `policy::decide_edition`: a lane holds one copy per wanted edition; the candidate is compared with the held copy of its own edition through `decide`, so protections still apply. A candidate of an edition the title does not want is held with that reason. `wanted = None` means the editions held. The open matte protection in `rules/policy.toml` stays, and `decide` keeps honoring it; `open_matte_is_kept_by_the_edition_rule_alone` shows the edition rule gives the same answer without it.
- `naming::LibraryNaming`: Plex `{edition-Label}` after the ids, Jellyfin ` - Label` after the year. `parse_library_name` reads the Jellyfin form back when the label reads as an edition.
- Corpus (docs/corpus-report.md, Editions): 600 of 4,409 movie files carry an edition (596 Radarr, 4 Radarr Jellyfin); Singularr's reading of the name alone agrees with Radarr's field on 4,324 of 4,409 files (98.1%).

## Open items

- Decide whether a held copy should be scored probe first (the file is the truth) instead of name first. The report's "name and file disagree" buckets are the cost of name first.
- The server must take a held copy's group, quality, revision and edition from the arr's record, not from the pipeline's file name. The report's "as recorded" column does this.
- Radarr's scores were not exported. Export `customFormats` and `customFormatScore` for movie files and the movie comparison becomes direct.
- Sonarr's original file names are not exported, which leaves 300 scores unexplained by any name in the fixtures.
