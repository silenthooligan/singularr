# Quality profiles, lists and naming: the design

How a person says what they want from a file in Singularr, without a table of a hundred scored rules. This replaces the Rules page's long lists. The scoring engine stays (`singularr-core`: a release is scored by traits and a verdict explains itself); what changes is what a person edits, and how it is kept.

## The problem with the old shape

A profile in the managers is a quality ladder plus a score for every custom format, and every release group is its own custom format. This installation's policy mirrors that: 113 traits, most of them one release group each, each with points in four profiles. Nobody can read it, and changing a preference means editing numbers in many rows.

## What a person edits

Three things, each small.

### 1. Lists

A **list** is a named set of things that are treated alike: release groups, or streaming services. "Remux, tier 1" holds FraMeSToR, CiNEPHiLES, TRiToN and the others of that standing. A list is edited once and every profile that ranks it follows.

- Members are chips: type to add, click to remove, drag from one list to another.
- A list shows what it means in the library: how many files come from its members, and which members have none.
- "Unlisted" shows the groups seen in the library and in the managers' history that are in no list, most frequent first, so a list is grown from what really arrives.
- Lists ship with defaults a person can take or leave; they are data, not code.

### 2. Quality profiles

A **quality profile** answers five questions. Each is one compact control, and none of them is a number to type.

1. **Target.** Movies or shows, the resolution (720p, 1080p, 2160p), and the sources in order of preference as a strip of chips (Remux, Blu-ray, WEB-DL, WEBRip, HDTV) that is reordered by dragging. Two markers sit on the strip: **Accept from** (nothing below it is taken) and **Stop at** (a file at or above it, that also satisfies the preferences marked "required", is final).
2. **Picture.** Dynamic range (Dolby Vision, HDR10+, HDR10, SDR) and codec (HEVC, AV1, H.264), each with one of four stances: **Prefer**, **Allow**, **Avoid**, **Block**.
3. **Sound.** Formats (Atmos, TrueHD, DTS-HD MA, DTS, E-AC-3, AAC, Opus), channels, and languages (original language, others), with the same four stances.
4. **Release groups and services.** The lists, ranked by dragging: the higher a list, the more its releases are preferred. A list can also be set to Avoid or Block. Releases from no list rank between the last preferred list and the first avoided one.
5. **Size and upgrades.** A size range per hour of runtime as a two-handle slider; **Upgrade until** (the Stop at marker, restated); **Upgrade when** (any better release, or only a clearly better one); and whether a smaller file wins a tie.

Everything else a release can be (an edition, a repack, an AI upscale, a hardcoded subtitle, a sample) has a sensible default stance and is listed under **More** in the same four-stance form, closed unless opened.

A profile has a one-line summary made from these answers, which is what its card shows: "1080p, Remux to WEB-DL, prefers Dolby Vision and Atmos, tier 1 groups first, 4 to 30 GB per hour".

### 3. Naming

A **naming convention** is a template per thing that is named: movie folder, movie file, series folder, season folder, episode file. A template is built from tokens picked from a list (title, year, edition, quality, dynamic range, audio, codec, release group, the ids), with literal text between them.

- The preview is live and uses real files of the library: the current name beside the name the template gives, for a handful of titles the person can change.
- Presets for the common conventions (Plex, Jellyfin, Emby, Kodi, and the detailed one the managers' guides recommend).
- "Does not match" lists the files whose name differs from the convention.

## How it stays explainable

- **Live ranking.** Beside the editor, a sample of releases (the candidates the manager last saw for a chosen title, or the files the library holds) is ranked by the profile as it is being edited, each with the reasons in words. Moving a list or changing a stance reorders the sample at once.
- **Verdicts keep their trace.** A verdict names the answer that earned or cost a release its place ("Release groups: Remux, tier 1", "Picture: Dolby Vision preferred"), not a trait id and a number. The numbers exist and are shown on request.
- **Advanced rules.** A condition the five questions cannot express (the engine's full condition language) can still be written as a rule with a stance, under More. The existing policy's exotic traits land there on import. Most profiles have none.

## How it compiles

The authoring model compiles to the engine's policy, so scoring, verdict traces and the shadow comparison with the managers do not change.

- A list becomes one trait whose condition names all its members.
- A stance on a built-in attribute becomes points on that attribute's trait: Prefer and Avoid get a weight from their position (earlier in a ranked group is stronger), Allow is zero, Block rejects.
- The ranked lists get descending weights large enough that no combination of smaller preferences reorders two adjacent lists unless the profile says "a clearly better release may".
- The source strip is the ladder; Accept from and Stop at are the minimum and the cutoff.
- The compiler is a pure function with tests of its own, and the compiled policy is shown read-only for anyone who wants the numbers.

## Where it is kept

- Profiles, lists and naming conventions are Singularr's own data, with every change in the journal and earlier versions kept, so a change can be compared and undone.
- An installation can still pin its policy in a file. Then the pages show it read-only with its source, and offer to import it. Import turns a file's traits into this model: traits that only name groups become lists (groups with the same points in every profile fall into the same list), attribute traits become stances, and what is left becomes advanced rules.
- Export gives the same file form back.

## What it drives

- Singularr's own verdicts and the "can be upgraded" marks, at once.
- The managers, while they still do the grabbing and the naming: with write access to a manager, **Apply to the manager** writes the profile as that manager's quality profile and custom formats, and the convention as its naming settings, and shows the difference before it writes. Without write access the pages say what would change and nothing is sent.
- Later, Singularr's own search and import (stage 2 of DESIGN.md), with no manager in between.

## API (for the build)

- `GET /api/v1/profiles`, `POST`, `GET /{id}`, `PUT /{id}`, `DELETE /{id}`, `POST /{id}/duplicate`; a profile's body is the five answers plus More and advanced rules.
- `GET /api/v1/lists`, `POST`, `PUT /{id}`, `DELETE /{id}`; `GET /api/v1/lists/unlisted` (groups and services seen and in no list, with counts).
- `GET /api/v1/naming`, `PUT /api/v1/naming/{kind}`; `POST /api/v1/naming/preview` (a template and titles, answers current and resulting names); `GET /api/v1/naming/presets`.
- `POST /api/v1/profiles/preview`: a draft profile and a title (or a set of release names), answers the ranking with reasons, without saving.
- `GET /api/v1/policy` keeps answering the compiled policy and says where it comes from (file, or saved with its version).
- `POST /api/v1/policy/import` (the pinned file into the model) and `GET /api/v1/policy/export`.
- `POST /api/v1/profiles/{id}/apply` and `POST /api/v1/naming/apply` with `dryRun`: the difference to a manager, and, with write access, the write.

## Pages

Rules becomes four tabs in place of the long tables: **Quality profiles** (cards; a profile opens as the five-question editor with the live ranking beside it), **Lists** (cards of chips; Unlisted), **Naming** (templates with the token picker and the live preview), and **More** (editions, protections, request rules, the compiled policy). Retention, processing rules and download cleanup get their own tabs when they are built.
