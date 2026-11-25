# Quality profiles, lists and naming

This document describes how a person states what they want from a file in Singularr. It replaces the long tables of scored rules on the Rules page. The scoring engine is unchanged (`singularr-core`). Traits score a release. Each verdict lists the reasons for its score. The change is in what a person edits and how Singularr stores it.

## The old shape

A profile in the managers is a quality ladder plus a score for every custom format. Every release group is its own custom format. A policy built that way has 113 traits, most of them one release group each, each with points in four profiles. It is hard to read. Changing a preference means editing numbers in many rows.

## What a person edits

Three things: lists, quality profiles and naming conventions.

### 1. Lists

A **list** is a named set of release groups or streaming services that are treated alike. "Remux, tier 1" contains FraMeSToR, CiNEPHiLES, TRiToN and others of that standing. A change to a list applies to every profile that ranks it.

- Members are chips: type to add, click to remove, drag from one list to another.
- A list shows its effect on the library: how many files come from its members and which members have none.
- "Unlisted" shows the groups seen in the library and in the managers' history that are in no list. The most frequent come first. Lists are built from the groups that actually appear.
- Lists ship with defaults. They are data and can be edited or removed.

### 2. Quality profiles

A **quality profile** consists of the answers to five questions. Each question is one compact control. None of them asks for a number.

1. **Target.** Movies or shows; the resolution (720p, 1080p, 2160p); the sources in order of preference as a strip of chips (Remux, Blu-ray, WEB-DL, WEBRip, HDTV), reordered by dragging. Two markers sit on the strip: **Accept from** (nothing below it is taken) and **Stop at** (a file at or above it that also meets every preference marked "required" is final).
2. **Picture.** Dynamic range (Dolby Vision, HDR10+, HDR10, SDR) and codec (HEVC, AV1, H.264). Each has one of four stances: **Prefer**, **Allow**, **Avoid**, **Block**.
3. **Sound.** Formats (Atmos, TrueHD, DTS-HD MA, DTS, E-AC-3, AAC, Opus), channels and languages (original language, others). The same four stances apply.
4. **Release groups and services.** The lists, ranked by dragging. A higher list means a stronger preference. A list can also be set to Avoid or Block. Releases from no list rank between the last preferred list and the first avoided one.
5. **Size and upgrades.** A size range per hour of runtime on a two-handle slider; **Upgrade until** (the Stop at marker); **Upgrade when** (any better release, or only a clearly better one); and whether a smaller file wins a tie.

Every other property of a release (an edition, a repack, an AI upscale, a hardcoded subtitle, a sample) has a default stance. These properties are listed under **More** in the same four-stance form. More is closed by default.

A profile has a one-line summary made from these answers. Its card shows the summary: "1080p, Remux to WEB-DL, prefers Dolby Vision and Atmos, tier 1 groups first, 4 to 30 GB per hour".

### 3. Naming

A **naming convention** has a template for each named thing: movie folder, movie file, series folder, season folder and episode file. A template is built from tokens picked from a list (title, year, edition, quality, dynamic range, audio, codec, release group, the ids), with literal text between them.

- The preview is live and uses real files from the library. It shows the current name beside the template's name for a handful of titles. The titles can be changed.
- Presets cover the common conventions: Plex, Jellyfin, Emby, Kodi and the detailed convention the managers' guides recommend.
- "Does not match" lists the files whose names differ from the convention.

## Explanations

- **Live ranking.** A sample of releases sits beside the editor: the candidates the manager last saw for a chosen title, or the files in the library. The profile ranks the sample while it is edited. Each release shows its reasons in words. Moving a list or changing a stance reorders the sample at once.
- **Verdict traces.** A verdict names the answer that earned or cost a release its place ("Release groups: Remux, tier 1", "Picture: Dolby Vision preferred"). Trait ids and numbers appear on request.
- **Advanced rules.** A condition outside the five questions (the engine's full condition language) can be written as a rule with a stance under More. On import, the unusual traits of an existing policy become advanced rules. Most profiles have none.

## Compilation

The authoring model compiles to the engine's policy. Scoring, verdict traces and the shadow comparison with the managers are unchanged.

- A list becomes one trait whose condition names all its members.
- A stance on a built-in attribute becomes points on that attribute's trait. Prefer and Avoid get a weight from their position (earlier in a ranked group is stronger). Allow is zero. Block rejects.
- The ranked lists get descending weights. No combination of smaller preferences reorders two adjacent lists. The one exception is a profile that upgrades for a clearly better release.
- The source strip is the ladder. Accept from is the minimum and Stop at is the cutoff.
- The compiler is a pure function with its own tests. The compiled policy is shown read-only for anyone who wants the numbers.

## Storage

- Profiles, lists and naming conventions are Singularr's own data. Every change is in the journal. Earlier versions are kept for comparison and undo.
- An installation can still pin its policy in a file. The pages then show the policy read-only with its source and offer to import it. Import turns a file's traits into this model. Traits that only name groups become lists (groups with the same points in every profile go into the same list). Attribute traits become stances. The rest become advanced rules.
- Export returns the same file form.

## Effects

- Singularr's own verdicts and the "can be upgraded" marks change at once.
- The managers, while they still grab and name files: with write access to a manager, **Apply to the manager** writes the profile as the manager's quality profile and custom formats. It writes the naming convention as the manager's naming settings. It shows the difference before writing. Without write access the page shows what would change and sends nothing.
- Later, Singularr's own search and import (stage 2 of the roadmap) use the profiles with no manager in between.

## API

- `GET /api/v1/profiles`, `POST`, `GET /{id}`, `PUT /{id}`, `DELETE /{id}`, `POST /{id}/duplicate`. A profile's body is the five answers plus More and advanced rules.
- `GET /api/v1/lists`, `POST`, `PUT /{id}`, `DELETE /{id}`; `GET /api/v1/lists/unlisted` (groups and services seen and in no list, with counts).
- `GET /api/v1/naming`, `PUT /api/v1/naming/{kind}`; `POST /api/v1/naming/preview` (a template and titles; returns the current and resulting names); `GET /api/v1/naming/presets`.
- `POST /api/v1/profiles/preview`: a draft profile and a title (or a set of release names). Returns the ranking with reasons without saving.
- `GET /api/v1/policy` returns the compiled policy and its source (file, or saved with its version).
- `POST /api/v1/policy/import` (the pinned file into the model) and `GET /api/v1/policy/export`.
- `POST /api/v1/profiles/{id}/apply` and `POST /api/v1/naming/apply` with `dryRun`: the difference to a manager and, with write access, the write.

## Pages

Rules has four tabs in place of the long tables: **Quality profiles** (cards; a profile opens as the five-question editor with the live ranking beside it), **Lists** (cards of chips; Unlisted), **Naming** (templates with the token picker and the live preview) and **More** (editions, protections, request rules, the compiled policy). Retention, processing rules and download cleanup get their own tabs when they are built.
