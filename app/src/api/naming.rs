//! Naming conventions (`docs/profiles.md`, Naming): a template per kind of name, kept in the
//! saved model with its versions. `GET /api/v1/naming` answers the conventions in force with the
//! tokens and the presets; `PUT /api/v1/naming/{kind}` saves one (the owner's); `POST
//! /api/v1/naming/preview` writes a template's names for files of the library beside their
//! names now, and counts the files whose name differs.
//!
//! Naming is Singularr's own: it is saved whether or not a policy file pins the profiles, and a
//! save does not change the policy in force.
//!
//! Each manager is asked how it names files (`/config/naming` and `/customformat`, plain GETs);
//! its formats, in Singularr's tokens, are the presets "From Radarr" and "From Sonarr", and the
//! conventions of every kind a model names not. Names are written from what the file's manager
//! reports (its quality with Proper or Repack, media info, group, edition, custom formats), so a
//! file the manager named matches its own format.

use super::routes::{ApiJson, ApiPath};
use super::state::AppState;
use crate::catalog::model::TitleKind;
use crate::catalog::store::{self as catalog_store, HeldFile};
use crate::core::auth::Caller;
use crate::core::error::AppError;
use crate::core::schema::ProblemDoc;
use crate::policy::saved::{self, Change, SaveError};
use axum::extract::State;
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use singularr_core::convention::{self, Facts, ManagerFile, PRESETS, TOKENS};
use singularr_core::profile::{Convention, NameKind};
use singularr_core::release::ParsedRelease;
use std::collections::BTreeSet;
use std::time::Duration;
use utoipa::OpenApi;

fn save_failed(e: SaveError) -> AppError {
    match e {
        SaveError::Invalid(why) | SaveError::Conflict(why) => AppError::BadRequest(why),
        SaveError::NotFound(why) => AppError::NotFound(why),
        SaveError::Body(why) => AppError::Internal(why),
        SaveError::Db(e) => AppError::Db(e),
    }
}

fn kind_of(key: &str) -> Result<NameKind, AppError> {
    NameKind::from_key(key).ok_or_else(|| {
        AppError::not_found(format!(
            "There is no kind of name `{key}`: movieFolder, movieFile, seriesFolder, seasonFolder or episodeFile."
        ))
    })
}

fn convention_json(c: &Convention) -> Value {
    json!({
        "kind": c.kind.key(),
        "label": c.kind.label(),
        "template": c.template,
        "preset": c.preset,
        "rev": c.rev,
    })
}

/// A manager's naming in Singularr's tokens: the preset it offers ("From Radarr") and the
/// conventions a model that names none starts from.
struct FromManager {
    id: String,
    label: String,
    templates: Vec<(NameKind, String)>,
    /// The manager's tokens Singularr has none for, left out.
    dropped: Vec<String>,
    /// The custom formats it writes into file names.
    named: BTreeSet<String>,
}

impl FromManager {
    fn movies(&self) -> bool {
        self.templates.iter().any(|(k, _)| k.is_movie())
    }
}

/// How long a manager may take to say how it names files.
const MANAGER_WAIT: Duration = Duration::from_secs(5);

/// How each configured manager names files, asked now and translated into Singularr's tokens. A
/// manager that does not answer offers nothing.
async fn from_managers(state: &AppState) -> Vec<FromManager> {
    let asks = state.engines.arrs.iter().map(|arr| async move {
        let engine = arr.engine();
        let movie = matches!(engine.kind, TitleKind::Movie);
        let naming = tokio::time::timeout(MANAGER_WAIT, arr.naming()).await;
        (engine.id.clone(), movie, naming)
    });
    let mut out: Vec<FromManager> = Vec::new();
    for (id, movie, naming) in futures::future::join_all(asks).await {
        let Ok(Ok(Some(n))) = naming else {
            continue;
        };
        let formats = if movie {
            vec![
                (NameKind::MovieFolder, n.movie_folder),
                (NameKind::MovieFile, n.movie_file),
            ]
        } else {
            vec![
                (NameKind::SeriesFolder, n.series_folder),
                (NameKind::SeasonFolder, n.season_folder),
                (NameKind::EpisodeFile, n.episode_file),
            ]
        };
        let mut templates = Vec::new();
        let mut dropped = Vec::new();
        for (kind, format) in formats {
            if let Some(format) = format {
                let t = convention::from_manager(&format);
                templates.push((kind, t.template));
                dropped.extend(t.dropped);
            }
        }
        if templates.is_empty() {
            continue;
        }
        let app = if movie { "Radarr" } else { "Sonarr" };
        let first = !out.iter().any(|m| m.movies() == movie);
        out.push(FromManager {
            id: if first {
                format!("from-{}", app.to_ascii_lowercase())
            } else {
                format!("from-{id}")
            },
            label: if first {
                format!("From {app}")
            } else {
                format!("From {app} ({id})")
            },
            templates,
            dropped,
            named: n.named_formats.into_iter().collect(),
        });
    }
    out
}

/// The conventions the managers name files by: for each kind, the first manager's.
fn managers_first(managers: &[FromManager]) -> Vec<Convention> {
    managers
        .iter()
        .flat_map(|m| {
            m.templates.iter().map(|(kind, template)| Convention {
                kind: *kind,
                template: template.clone(),
                preset: Some(m.id.clone()),
                rev: 0,
            })
        })
        .collect()
}

/// The managers' presets first, then Singularr's.
fn presets_json(managers: &[FromManager]) -> Vec<Value> {
    let mut out: Vec<Value> = managers
        .iter()
        .map(|m| {
            let templates: serde_json::Map<String, Value> = m
                .templates
                .iter()
                .map(|(k, t)| (k.key().to_string(), json!(t)))
                .collect();
            json!({"id": m.id, "label": m.label, "templates": templates, "dropped": m.dropped})
        })
        .collect();
    out.extend(PRESETS.iter().map(|p| {
        let templates: serde_json::Map<String, Value> = NameKind::ALL
            .iter()
            .map(|k| (k.key().to_string(), json!(p.template(*k))))
            .collect();
        json!({"id": p.id, "label": p.label, "templates": templates})
    }));
    out
}

/// The naming conventions in force, the tokens a template can hold and the presets. A kind the
/// saved model names not is named as its manager names it, else by the detailed preset.
#[utoipa::path(get, path = "/api/v1/naming", tag = "policy",
    responses((status = 200, description = "`{editable, version, conventions: [{kind, label, template, preset, rev}], tokens: [{token, label, kinds}], presets: [{id, label, templates, dropped}]}`", body = Object),
        (status = 401, body = ProblemDoc)))]
async fn naming_get(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let latest = saved::latest(&state.pool).await.map_err(save_failed)?;
    let managers = from_managers(&state).await;
    let conventions = convention::in_force_with(
        latest.as_ref().map_or(&[][..], |s| &s.model.naming[..]),
        &managers_first(&managers),
    );
    let tokens: Vec<Value> = TOKENS
        .iter()
        .map(|t| {
            json!({
                "token": t.name,
                "label": t.label,
                "kinds": t.kinds.iter().map(|k| k.key()).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(json!({
        "editable": true,
        "version": latest.as_ref().map(|s| s.version),
        "conventions": conventions.iter().map(convention_json).collect::<Vec<_>>(),
        "tokens": tokens,
        "presets": presets_json(&managers),
    })))
}

/// The presets: the managers' own ("From Radarr", "From Sonarr", for their kinds of name, with
/// the tokens left out), then Singularr's, each a template for every kind of name.
#[utoipa::path(get, path = "/api/v1/naming/presets", tag = "policy",
    responses((status = 200, description = "`{presets: [{id, label, templates: {movieFolder, movieFile, seriesFolder, seasonFolder, episodeFile}, dropped}]}`", body = Object),
        (status = 401, body = ProblemDoc)))]
async fn naming_presets(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "presets": presets_json(&from_managers(&state).await) }))
}

/// `PUT /api/v1/naming/{kind}`.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct NamingPut {
    pub template: String,
    /// The preset the template was taken from, when it was.
    #[serde(default)]
    pub preset: Option<String>,
}

/// Save the template of one kind of name. A new version of the saved model; the policy in force
/// does not change.
#[utoipa::path(put, path = "/api/v1/naming/{kind}", tag = "policy", request_body = NamingPut,
    params(("kind" = String, Path, description = "movieFolder, movieFile, seriesFolder, seasonFolder or episodeFile")),
    responses((status = 200, description = "The convention as saved, with `version`", body = Object), (status = 400, body = ProblemDoc),
        (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn naming_put(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(kind): ApiPath<String>,
    ApiJson(body): ApiJson<NamingPut>,
) -> Result<Json<Value>, AppError> {
    if !matches!(caller, Caller::Owner { .. }) {
        return Err(AppError::Forbidden(
            "Only the owner may change naming.".to_string(),
        ));
    }
    let kind = kind_of(&kind)?;
    let template = body.template.trim().to_string();
    convention::check(&template).map_err(AppError::BadRequest)?;
    let managers = from_managers(&state).await;
    let first = managers_first(&managers);
    let preset = body.preset.filter(|p| {
        convention::preset(p).is_some_and(|x| x.template(kind) == template)
            || first
                .iter()
                .any(|c| c.preset.as_ref() == Some(p) && c.kind == kind && c.template == template)
    });
    let actor = caller.actor();
    let subject = format!("naming:{}", kind.key());
    let in_force = state.policy.current();
    let config = &state.config;
    let ((), saved, _) = saved::change(
        &state.pool,
        &state.journal,
        Change {
            action: "naming_saved",
            subject: &subject,
            actor: &actor,
        },
        || saved::imported(&in_force.policy, config, None),
        |m| {
            let mut all = convention::in_force_with(&m.naming, &first);
            if let Some(c) = all.iter_mut().find(|c| c.kind == kind) {
                c.rev += 1;
                c.template = template.clone();
                c.preset = preset.clone();
            }
            m.naming = all;
            Ok(())
        },
    )
    .await
    .map_err(save_failed)?;
    let c = saved
        .model
        .naming
        .iter()
        .find(|c| c.kind == kind)
        .cloned()
        .ok_or_else(|| AppError::Internal("the convention was not saved".to_string()))?;
    let mut v = convention_json(&c);
    v["version"] = json!(saved.version);
    Ok(Json(v))
}

/// `POST /api/v1/naming/preview`.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamingPreview {
    /// movieFolder, movieFile, seriesFolder, seasonFolder or episodeFile.
    pub kind: String,
    pub template: String,
    /// Titles to show: `movie-603`, `show-1396`. Left out: the first ones of the library.
    #[serde(default)]
    pub titles: Vec<String>,
    /// How many names to show (default 8, at most 50).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One name the template writes for the library: what it is now and what it would be.
struct Named {
    title: String,
    title_id: String,
    current: String,
    result: String,
}

fn last_segment(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
}

fn title_id(f: &HeldFile) -> String {
    match f.tmdb_id {
        Some(tmdb) => format!("{}-{tmdb}", f.kind),
        None => format!("title-{}", f.title_id),
    }
}

/// What is known of a file, as its manager names it: the title's facts, the release's, and
/// over them the manager's (`named`: the custom formats it writes into names).
fn facts_of(f: &HeldFile, named: &BTreeSet<String>) -> Facts {
    let release: ParsedRelease = serde_json::from_value(f.held.clone()).unwrap_or_default();
    let formats: Vec<String> = f
        .arr_custom_formats
        .iter()
        .filter(|c| named.contains(*c))
        .cloned()
        .collect();
    Facts {
        title: Some(f.title.clone()),
        year: f.year.map(i64::from),
        tmdb_id: f.tmdb_id,
        imdb_id: f.imdb_id.clone(),
        tvdb_id: f.tvdb_id,
        edition: f.edition_marked.then(|| f.edition_label.clone()),
        season: f.season_number.map(i64::from),
        episode: f.episode_numbers.first().copied().map(i64::from),
        episode_title: f.episode_title.clone(),
        ..Facts::of_release(&release)
    }
    .with_manager(&ManagerFile {
        quality: f.arr_quality.as_deref(),
        proper: f.arr_proper,
        repack: f.arr_repack,
        group: f.arr_group.as_deref(),
        edition: f.arr_edition.as_deref(),
        media_info: f.media_info.as_ref(),
        custom_formats: &formats,
    })
}

/// The names of one kind the library has, each with the name the template gives it. A folder
/// is named once for its title (a season folder once for its season).
fn names(
    kind: NameKind,
    template: &str,
    files: &[HeldFile],
    named: &BTreeSet<String>,
) -> Vec<Named> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for f in files {
        let current = match kind {
            NameKind::MovieFile | NameKind::EpisodeFile => {
                convention::stem(last_segment(&f.relative_path)).to_string()
            }
            NameKind::MovieFolder | NameKind::SeriesFolder => match &f.folder {
                Some(folder) if seen.insert(f.title_id.to_string()) => {
                    last_segment(folder).to_string()
                }
                _ => continue,
            },
            NameKind::SeasonFolder => {
                let segments: Vec<&str> = f.relative_path.split(['/', '\\']).collect();
                let key = format!("{}:{:?}", f.title_id, f.season_number);
                if segments.len() < 2 || !seen.insert(key) {
                    continue;
                }
                segments[segments.len() - 2].to_string()
            }
        };
        out.push(Named {
            title: f.title.clone(),
            title_id: title_id(f),
            result: convention::render(template, &facts_of(f, named)),
            current,
        });
    }
    out
}

fn named_json(n: &Named) -> Value {
    json!({
        "title": n.title,
        "titleId": n.title_id,
        "current": n.current,
        "result": n.result,
        "matches": n.current == n.result,
    })
}

/// A template's names for files of the library beside their names now, and how many of the
/// library's names differ from it.
#[utoipa::path(post, path = "/api/v1/naming/preview", tag = "policy", request_body = NamingPreview,
    responses((status = 200, description = "`{kind, unknown, items: [{title, titleId, current, result, matches}], mismatches: {count, total, items}}`", body = Object),
        (status = 400, body = ProblemDoc), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn naming_preview(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<NamingPreview>,
) -> Result<Json<Value>, AppError> {
    let kind = kind_of(&req.kind)?;
    let template = req.template.trim().to_string();
    let files = catalog_store::held_files_of_kind(
        &state.pool,
        if kind.is_movie() { "movie" } else { "show" },
        50_000,
    )
    .await?;
    // The custom formats a manager writes into names, asked of it only when the template
    // names custom formats.
    let named: BTreeSet<String> = if template.contains("Custom Formats") {
        from_managers(&state)
            .await
            .into_iter()
            .filter(|m| m.movies() == kind.is_movie())
            .flat_map(|m| m.named)
            .collect()
    } else {
        BTreeSet::new()
    };
    let all = names(kind, &template, &files, &named);
    let limit = req.limit.unwrap_or(8).clamp(1, 50);
    let wanted: Vec<&str> = req.titles.iter().map(|t| t.trim()).collect();
    let items: Vec<Value> = all
        .iter()
        .filter(|n| wanted.is_empty() || wanted.contains(&n.title_id.as_str()))
        .take(limit)
        .map(named_json)
        .collect();
    let differ: Vec<&Named> = all.iter().filter(|n| n.current != n.result).collect();
    Ok(Json(json!({
        "kind": kind.key(),
        "template": template,
        "unknown": convention::unknown_tokens(&template),
        "items": items,
        "mismatches": {
            "count": differ.len(),
            "total": all.len(),
            "items": differ.iter().take(50).map(|n| named_json(n)).collect::<Vec<_>>(),
        },
    })))
}

/// The routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/naming", get(naming_get))
        .route("/naming/presets", get(naming_presets))
        .route("/naming/preview", post(naming_preview))
        .route("/naming/{kind}", put(naming_put))
}

#[derive(OpenApi)]
#[openapi(
    paths(naming_get, naming_presets, naming_put, naming_preview),
    components(schemas(NamingPut, NamingPreview))
)]
struct NamingDoc;

/// These routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    NamingDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(kind: &str, title: &str, path: &str, held: &str) -> HeldFile {
        HeldFile {
            copy_id: 1,
            title_id: 7,
            title: title.to_string(),
            year: Some(2016),
            kind: kind.to_string(),
            tmdb_id: Some(333371),
            tvdb_id: None,
            imdb_id: Some("tt1179933".to_string()),
            season_number: None,
            episode_numbers: Vec::new(),
            relative_path: path.to_string(),
            folder: Some(format!("/movies/{title} (2016) {{tmdb-333371}}")),
            episode_title: None,
            size_bytes: 1,
            edition_label: "Theatrical".to_string(),
            edition_marked: false,
            held: serde_json::to_value(
                singularr_core::naming::parse_library_name(held).to_release(),
            )
            .unwrap(),
            media_info: None,
            languages: Vec::new(),
            original_language: None,
            tags: Vec::new(),
            arr_quality: None,
            arr_proper: false,
            arr_repack: false,
            arr_group: None,
            arr_edition: None,
            arr_custom_formats: Vec::new(),
        }
    }

    #[test]
    fn the_preview_writes_names_beside_the_library_s_and_counts_those_that_differ() {
        let name = "10 Cloverfield Lane (2016) {imdb-tt1179933} [Bluray-1080p][DV HDR10][EAC3 Atmos 5.1][x265]-SM737.mkv";
        let files = vec![
            file("movie", "10 Cloverfield Lane", name, name),
            file(
                "movie",
                "10 Cloverfield Lane",
                "10.Cloverfield.Lane.2016.1080p.BluRay.x265-SM737.mkv",
                name,
            ),
        ];
        let detailed = PRESETS[0];
        let named = names(
            NameKind::MovieFile,
            detailed.template(NameKind::MovieFile),
            &files,
            &BTreeSet::new(),
        );
        assert_eq!(named.len(), 2);
        assert_eq!(
            named[0].current, named[0].result,
            "the detailed name matches"
        );
        assert_ne!(named[1].current, named[1].result);
        assert_eq!(named[0].title_id, "movie-333371");
        // One folder per title, its name the folder's last part.
        let folders = names(
            NameKind::MovieFolder,
            detailed.template(NameKind::MovieFolder),
            &files,
            &BTreeSet::new(),
        );
        assert_eq!(folders.len(), 1);
        assert_eq!(
            folders[0].current,
            "10 Cloverfield Lane (2016) {tmdb-333371}"
        );
        assert_eq!(folders[0].result, folders[0].current);
        let v = named_json(&named[1]);
        assert_eq!(v["matches"], false);
    }

    #[test]
    fn season_folders_are_named_once_a_season() {
        let mut a = file(
            "show",
            "Shoresy",
            "Season 01/Shoresy (2022) - S01E01 - Pilot.mkv",
            "Shoresy (2022) - S01E01 - Pilot [HDTV-1080p]-NTb.mkv",
        );
        a.season_number = Some(1);
        a.episode_numbers = vec![1];
        let mut b = a.clone();
        b.relative_path = "Season 01/Shoresy (2022) - S01E02 - Next.mkv".to_string();
        b.episode_numbers = vec![2];
        let named = names(
            NameKind::SeasonFolder,
            "Season {Season:00}",
            &[a, b],
            &BTreeSet::new(),
        );
        assert_eq!(named.len(), 1);
        assert_eq!(
            (named[0].current.as_str(), named[0].result.as_str()),
            ("Season 01", "Season 01")
        );
    }

    /// A file a manager named, as the catalog keeps it: its title's facts from the title's
    /// record, the manager's from the file's.
    fn recorded(kind: &str, title: &Value, file: &Value, episode: Option<&Value>) -> HeldFile {
        let text = |v: &Value| v.as_str().map(str::to_string);
        let number = |v: &Value| v.as_i64().and_then(|n| i32::try_from(n).ok());
        let path = file["relativePath"].as_str().unwrap();
        let revision = &file["quality"]["revision"];
        HeldFile {
            copy_id: file["id"].as_i64().unwrap(),
            title_id: title["id"].as_i64().unwrap(),
            title: title["title"].as_str().unwrap().to_string(),
            year: number(&title["year"]),
            kind: kind.to_string(),
            tmdb_id: title["tmdbId"].as_i64(),
            tvdb_id: title["tvdbId"].as_i64(),
            imdb_id: text(&title["imdbId"]),
            season_number: number(&file["seasonNumber"]),
            episode_numbers: episode
                .and_then(|e| number(&e["episodeNumber"]))
                .into_iter()
                .collect(),
            relative_path: path.to_string(),
            folder: text(&title["path"]),
            episode_title: episode.and_then(|e| text(&e["title"])),
            size_bytes: 1,
            edition_label: String::new(),
            edition_marked: false,
            held: serde_json::to_value(
                singularr_core::naming::parse_library_name(last_segment(path)).to_release(),
            )
            .unwrap(),
            media_info: Some(file["mediaInfo"].clone()),
            languages: Vec::new(),
            original_language: None,
            tags: Vec::new(),
            arr_quality: text(&file["quality"]["quality"]["name"]),
            arr_proper: revision["version"].as_i64().unwrap_or(1) > 1,
            arr_repack: revision["isRepack"].as_bool().unwrap_or(false),
            arr_group: text(&file["releaseGroup"]),
            arr_edition: text(&file["edition"]),
            arr_custom_formats: file["customFormats"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| text(&f["name"]))
                .collect(),
        }
    }

    /// The custom formats a recorded `/customformat` answer writes into names.
    fn named_in(answer: &str) -> BTreeSet<String> {
        let all: Vec<Value> = serde_json::from_str(answer).unwrap();
        all.iter()
            .filter(|f| f["includeCustomFormatWhenRenaming"] == true)
            .filter_map(|f| f["name"].as_str().map(str::to_string))
            .collect()
    }

    /// The names a manager's format, translated, gives that differ from the files' names now.
    fn differences(
        kind: NameKind,
        format: &str,
        files: &[HeldFile],
        named: &BTreeSet<String>,
    ) -> Vec<(String, String)> {
        let translated = convention::from_manager(format);
        let all = names(kind, &translated.template, files, named);
        assert!(!all.is_empty(), "{kind:?}");
        all.into_iter()
            .filter(|n| n.current != n.result)
            .map(|n| (n.current, n.result))
            .collect()
    }

    #[test]
    fn radarr_s_own_format_names_the_files_radarr_named_as_they_are() {
        let naming: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/radarr_naming.json")).unwrap();
        let named = named_in(include_str!(
            "../../tests/fixtures/radarr_customformat.json"
        ));
        let movies: Vec<Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/radarr_named.json")).unwrap();
        let files: Vec<HeldFile> = movies
            .iter()
            .map(|m| recorded("movie", m, &m["movieFile"], None))
            .collect();
        for (kind, key) in [
            (NameKind::MovieFile, "standardMovieFormat"),
            (NameKind::MovieFolder, "movieFolderFormat"),
        ] {
            let wrong = differences(kind, naming[key].as_str().unwrap(), &files, &named);
            assert!(wrong.is_empty(), "{kind:?}: {wrong:#?}");
        }
        // The detailed preset writes what Radarr writes (Proper, HEVC, EAC3 Atmos 5.1) and
        // differs only where Radarr's format does: the title cleaned, custom formats named.
        let detailed = PRESETS[0].template(NameKind::MovieFile);
        let differ: Vec<String> = names(NameKind::MovieFile, detailed, &files, &named)
            .into_iter()
            .filter(|n| n.current != n.result)
            .map(|n| n.title)
            .collect();
        assert_eq!(
            differ,
            [
                "Mission: Impossible - Dead Reckoning Part One",
                "Am\u{e9}lie",
                "Ocean's Eleven"
            ]
        );
    }

    #[test]
    fn sonarr_s_own_format_names_the_files_sonarr_named_as_they_are() {
        let naming: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/sonarr_naming.json")).unwrap();
        let named = named_in(include_str!(
            "../../tests/fixtures/sonarr_customformat.json"
        ));
        let answer: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/sonarr_named.json")).unwrap();
        let files: Vec<HeldFile> = answer["episodefile"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                let series = answer["series"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|s| s["id"] == f["seriesId"])
                    .unwrap();
                let episode = answer["episode"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|e| e["episodeFileId"] == f["id"]);
                recorded("show", series, f, episode)
            })
            .collect();
        for (kind, key) in [
            (NameKind::EpisodeFile, "standardEpisodeFormat"),
            (NameKind::SeriesFolder, "seriesFolderFormat"),
            (NameKind::SeasonFolder, "seasonFolderFormat"),
        ] {
            let wrong = differences(kind, naming[key].as_str().unwrap(), &files, &named);
            assert!(wrong.is_empty(), "{kind:?}: {wrong:#?}");
        }
    }

    #[test]
    fn the_openapi_document_lists_the_naming_routes() {
        let doc = serde_json::to_value(super::super::routes::openapi()).unwrap();
        for (path, method) in [
            ("/api/v1/naming", "get"),
            ("/api/v1/naming/presets", "get"),
            ("/api/v1/naming/{kind}", "put"),
            ("/api/v1/naming/preview", "post"),
        ] {
            assert!(doc["paths"][path][method].is_object(), "{method} {path}");
        }
    }
}
