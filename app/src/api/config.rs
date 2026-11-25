//! The configuration routes: the settings the server describes, with their values and their
//! sources, and saving them; and the panels of the Settings page (integrations, libraries,
//! people, jobs, the system) in the shapes of `api::panels`. `routes::router` merges them with
//! one line and `routes::openapi` merges their document.
//!
//! Every route needs a caller with `read` (the layer in `routes::router`). Saving the
//! installation's settings needs the `settings` scope; a person's own values (their appearance)
//! need a person who signs in; what Singularr may do with an integration (`read`, or `act`) is
//! the owner's alone to change. Nothing here writes to an engine: saves go to Singularr's own
//! tables, each with a journal event.

use super::panels::{
    self, CacheView, CheckEntry, IntegrationDetail, IntegrationPanel, IntegrationPanels,
    IntegrationPatch, IntegrationTest, JobsPanel, LibraryFacts, LibraryPanels, PeoplePanel,
    PersonView, ServiceView, Started, SystemLinks, SystemPaths, SystemView, check,
    integration_panel, job_view, libraries_panel, provider_panel,
};
use super::routes::{ApiJson, ApiPath};
use super::state::AppState;
use crate::catalog::store as catalog_store;
use crate::core::auth::{self, Caller};
use crate::core::config::Integration;
use crate::core::db;
use crate::core::error::{AppError, AppResult};
use crate::core::jobs::{self, Check, RunNow};
use crate::core::model::{EventQuery, Paging, Scope};
use crate::core::outbound::Access;
use crate::core::schema::ProblemDoc;
use crate::core::store as core_store;
use crate::core::units;
use crate::engines::Protocol;
use crate::engines::access::{AccessError, AccessState};
use crate::engines::service::integration_view;
use crate::engines::store as engine_store;
use crate::settings::PatchError;
use crate::settings::model::{
    FieldView, OptionView, SettingsPatch, SettingsRefused, SettingsSaved, SettingsView,
};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use serde_json::json;
use singularr_sdk::manifest::{Kind, Manifest};
use utoipa::OpenApi;

type ApiResult<T> = Result<Json<T>, AppError>;

/// The installation's value in force of a setting, for any route that shows it:
/// `setting(&state, "general.name")`. `null` for a secret or an unknown key.
pub fn setting(state: &AppState, key: &str) -> serde_json::Value {
    state.settings.value(key)
}

/// Every setting: the sections of the Settings page, their groups and fields, each field with
/// its type, its value in force, its default, where the value comes from (`default`,
/// `environment` with the variable, `file` with the instance file, `saved`) and whether it can
/// be saved here. A secret is never sent: it says only whether it is set. Values each person sets
/// for themselves (`scope: user`) are the caller's own, with the installation's next to them.
#[utoipa::path(get, path = "/api/v1/settings", tag = "settings",
    responses((status = 200, body = SettingsView), (status = 401, body = ProblemDoc)))]
async fn settings_view(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> Json<SettingsView> {
    Json(state.settings.view(&caller))
}

/// Save settings: `{"values": {"general.name": "Home"}}`. `null` takes a saved value away.
/// With `"scope": "user"` the values are the caller's own (appearance). All or nothing: a value
/// that is refused stores none, and the answer is 422 with the reason per key. Every change is a
/// `setting_changed` event; it takes effect at once unless `restart` says otherwise.
#[utoipa::path(patch, path = "/api/v1/settings", tag = "settings", request_body = SettingsPatch,
    responses((status = 200, body = SettingsSaved), (status = 422, description = "Nothing was saved: why, by key", body = SettingsRefused),
        (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn settings_patch(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(patch): ApiJson<SettingsPatch>,
) -> Result<Response, AppError> {
    let saved = match state
        .settings
        .patch(&state.pool, &state.journal, &caller, &patch)
        .await
    {
        Ok(saved) => saved,
        Err(PatchError::Forbidden(why)) => return Err(AppError::Forbidden(why)),
        Err(PatchError::Refused(errors)) => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(SettingsRefused { errors }),
            )
                .into_response());
        }
        Err(PatchError::Db(e)) => return Err(AppError::Db(e)),
    };
    // The portal links the owner to the workspace's address from its own table.
    if saved.saved.iter().any(|k| k == "general.url") {
        let rules = &state.requests.rules;
        crate::requests::store::write_face_settings(
            &state.pool,
            i32::try_from(rules.max_seasons).unwrap_or(i32::MAX),
            &rules.gated_sites(),
            state.settings.text("general.url").as_deref(),
        )
        .await?;
    }
    // The policy's source puts the policy file or the saved model in force at once.
    if saved
        .saved
        .iter()
        .any(|k| k == crate::policy::saved::SOURCE)
    {
        super::profiles::apply_source(&state).await?;
    }
    // The location database's download, turned on or off: its loop passes now.
    if saved
        .saved
        .iter()
        .any(|k| k == crate::plays::locations::DOWNLOAD)
    {
        crate::plays::locations::wake();
    }
    // Addresses turned off: the kept ones go now, not at the next housekeeping (which never
    // runs with the sync off).
    if saved
        .saved
        .iter()
        .any(|k| k == crate::plays::model::KEEP_ADDRESSES)
        && !crate::plays::service::keep_addresses(&state.settings)
    {
        crate::plays::store::forget_addresses(&state.pool).await?;
    }
    Ok(Json(saved).into_response())
}

// The panels ---------------------------------------------------------------------------------------

/// A problem document for a request that conflicts with how things are now.
fn conflict_of(title: &str, why: &str) -> Response {
    let mut p = crate::core::http::Problem::new(StatusCode::CONFLICT, "singularr.conflict", title);
    p.detail = Some(why.to_string());
    p.into_response()
}

/// A problem document for a request that cannot start anything now.
fn conflict(why: &str) -> Response {
    conflict_of("Not started", why)
}

/// Every integration's panel, the configured ones in the configuration's order, then the
/// metadata providers.
async fn integration_panels(state: &AppState) -> AppResult<Vec<IntegrationPanel>> {
    let rows = engine_store::list(&state.pool).await?;
    let ids = panels::job_ids(&jobs::registry().list());
    let mut out: Vec<IntegrationPanel> = state
        .config
        .integrations
        .iter()
        .map(|i| {
            let row = rows.iter().find(|r| r.engine == i.id);
            integration_panel(
                i,
                integration_view(i, row, &state.engines.access.of(&i.id)),
                row,
                &jobs::registry().checks(&i.id),
                ids.get(&i.id).cloned().unwrap_or_default(),
                state.config.instance_path.as_deref(),
            )
        })
        .collect();
    for p in state.metadata.list() {
        if matches!(p.id.as_str(), "tmdb" | "tvdb") && state.config.integration(&p.id).is_none() {
            let mut panel =
                provider_panel(&p.id, &p.name, p.configured, p.needs.as_deref(), &p.host);
            let checks = jobs::registry().checks(&p.id);
            if let Some(last) = checks.first() {
                panel.last_check = Some(last.at);
                panel.latency_ms = i32::try_from(last.ms).ok();
            }
            panel.latency = checks
                .iter()
                .rev()
                .filter_map(|c| i32::try_from(c.ms).ok())
                .collect();
            out.push(panel);
        }
    }
    Ok(out)
}

/// The configured integrations (and the metadata providers) with their group, their address
/// without credentials, where they are configured, their health, the last contact and sync,
/// latency, what their passes counted, what Singularr reads from each, and what it may do with
/// each (`access`: `read`, or `act` for the `actions` the type has); the integration types
/// this version knows and their groups, as data.
#[utoipa::path(get, path = "/api/v1/integrations", tag = "system",
    responses((status = 200, body = IntegrationPanels), (status = 401, body = ProblemDoc)))]
async fn integrations(State(state): State<AppState>) -> ApiResult<IntegrationPanels> {
    Ok(Json(IntegrationPanels {
        editable: false,
        file: state.config.instance_path.clone(),
        groups: panels::groups(),
        types: panels::kinds(),
        integrations: integration_panels(&state).await?,
    }))
}

/// A field of an integration's own configuration: read-only, with its source.
fn own_field(id: &str, name: &str, label: &str, kind: &str, part: &str) -> FieldView {
    FieldView {
        key: format!("integrations.{id}.{name}"),
        label: label.to_string(),
        kind: kind.to_string(),
        value: None,
        default: None,
        set: None,
        source: "default".to_string(),
        env: None,
        file: None,
        editable: false,
        restart: true,
        scope: crate::settings::INSTANCE.to_string(),
        help: String::new(),
        withheld: false,
        options: None,
        min: None,
        max: None,
        step: None,
        unit: None,
        instance: None,
        part: Some(part.to_string()),
        placeholder: None,
    }
}

/// Which kind a Test of an integration of this type asks: the main one of a type that is read.
/// `None`: the type is listed and not read.
pub fn test_route(m: &Manifest) -> Option<Kind> {
    m.kinds.first().copied().filter(|_| m.read())
}

/// An integration's configuration as fields: its name, address and secret (set or not) where
/// they are set, and how often it is read.
fn integration_fields(state: &AppState, caller: &Caller, i: &Integration) -> Vec<FieldView> {
    let from_file = i.source != "environment";
    let file = state.config.instance_path.clone().filter(|_| from_file);
    let fixed = |mut f: FieldView, set_here: bool, env: Option<String>| {
        if set_here {
            f.source = if from_file { "file" } else { "environment" }.to_string();
            f.env = if from_file { None } else { env };
            f.file = file.clone();
        }
        f
    };
    let mut name = own_field(&i.id, "name", "Name", "text", "connection");
    name.value = Some(json!(i.name));
    name.default = Some(json!(i.kind.label()));
    let named = from_file || i.name != i.kind.label();
    let name_var = format!(
        "{}_NAME",
        i.url_var.strip_suffix("_URL").unwrap_or(&i.url_var)
    );
    let mut address = own_field(&i.id, "address", "Address", "url", "connection");
    address.value = Some(json!(i.url.as_deref().map(panels::without_credentials)));
    address.help = "As reached from the server".to_string();
    address.placeholder = panels::kinds()
        .into_iter()
        .find(|k| k.kind == i.kind.as_str())
        .and_then(|k| k.fields.into_iter().find(|f| f.key == "address"))
        .and_then(|f| f.placeholder);
    let mut secret = own_field(
        &i.id,
        panels::secret_key(i.kind),
        panels::secret_label(i.kind),
        "secret",
        "connection",
    );
    secret.set = Some(i.secret.is_some());
    secret.env = Some(i.secret_var.clone());
    secret.source = if i.secret.is_some() {
        "environment"
    } else {
        "default"
    }
    .to_string();
    secret.help = format!("Read from {} in the environment", i.secret_var);
    let mut out = vec![
        fixed(name, named, (!from_file).then_some(name_var)),
        fixed(address, i.url.is_some(), Some(i.url_var.clone())),
    ];
    out.push(secret);
    if i.kind.takes_user() {
        let mut user = own_field(&i.id, "username", "Username", "text", "connection");
        user.value = Some(json!(i.user));
        user.help = "Empty for no sign-in".to_string();
        let user_var = format!(
            "{}_USER",
            i.url_var.strip_suffix("_URL").unwrap_or(&i.url_var)
        );
        out.insert(2, fixed(user, i.user.is_some(), Some(user_var)));
    }
    // How often it is read: its passes, the main one first, with the setting that holds each
    // interval.
    for pass in i.kind.manifest().passes {
        if let Some(mut f) = state.settings.field(pass.setting, caller) {
            f.key = format!("integrations.{}.{}", i.id, pass.field);
            f.label = pass.label.to_string();
            f.part = Some("schedule".to_string());
            out.push(f);
        }
    }
    // What Singularr may do with it, for a type that has an action.
    let access = state.engines.access.of(&i.id);
    if !access.actions.is_empty() {
        out.push(access_field(
            &i.id,
            &access,
            matches!(caller, Caller::Owner { .. }),
        ));
    }
    out
}

/// What Singularr may do with an integration, as a field of its own part (`access`): `read` or
/// `act`, with where the value comes from and what fixes it. It is saved with
/// `PATCH /api/v1/integrations/{id}`, not with the settings route, and only by the owner.
fn access_field(id: &str, access: &AccessState, owner: bool) -> FieldView {
    let mut f = own_field(id, "access", "Access", "select", "access");
    f.value = Some(json!(access.access.as_str()));
    f.default = Some(json!(Access::Read.as_str()));
    f.source = access.source.to_string();
    f.env = access.env.clone();
    f.file = access.file.clone();
    f.editable = owner && access.editable();
    f.restart = false;
    f.options = Some(
        [(Access::Read, "Read-only"), (Access::Act, "Write access")]
            .into_iter()
            .map(|(value, label)| OptionView {
                value: value.as_str().to_string(),
                label: label.to_string(),
            })
            .collect(),
    );
    let actions: Vec<&str> = access.actions.iter().map(|a| a.label).collect();
    f.help = format!("Write access allows: {}", actions.join("; "));
    f
}

/// One integration: its panel, its configuration as fields (each with its source; the secret
/// only as set or not; how often it is read; what Singularr may do with it, for a type that has
/// an action) and its recent checks, newest first.
#[utoipa::path(get, path = "/api/v1/integrations/{id}", tag = "system", params(("id" = String, Path, description = "The integration's id")),
    responses((status = 200, body = IntegrationDetail), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn integration_one(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<IntegrationDetail> {
    Ok(Json(integration_detail(&state, &caller, &id).await?))
}

/// One integration's record: what `GET /api/v1/integrations/{id}` answers.
async fn integration_detail(
    state: &AppState,
    caller: &Caller,
    id: &str,
) -> AppResult<IntegrationDetail> {
    let panel = integration_panels(state)
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| AppError::not_found(format!("There is no integration `{id}`.")))?;
    let settings = match state.config.integration(id) {
        Some(i) => integration_fields(state, caller, i),
        None => {
            // A metadata provider: its key, from its variable.
            let mut f = own_field(
                id,
                if id == "tmdb" { "token" } else { "key" },
                if id == "tmdb" {
                    "Read access token"
                } else {
                    "API key"
                },
                "secret",
                "connection",
            );
            f.set = Some(panel.configured);
            f.env = panel.env.clone();
            f.source = if panel.configured {
                "environment"
            } else {
                "default"
            }
            .to_string();
            f.help = "Read from the environment".to_string();
            vec![f]
        }
    };
    // The checks this process made, then the journal's changes between up and down from
    // before them.
    let mut log: Vec<CheckEntry> = jobs::registry()
        .checks(id)
        .into_iter()
        .map(|c| CheckEntry {
            at: c.at,
            state: if c.ok { "ok" } else { "down" }.to_string(),
            detail: c.detail,
            via: c.via,
            ms: Some(c.ms),
        })
        .collect();
    let oldest = log.last().map(|c| c.at);
    let (changes, _) = core_store::list_events(
        &state.pool,
        &EventQuery {
            kind: Some("engine_up,engine_down".to_string()),
            subject_type: Some("engine".to_string()),
            subject_id: Some(id.to_string()),
            ..EventQuery::default()
        },
        Paging::new(None, Some(20)),
    )
    .await?;
    log.extend(
        changes
            .into_iter()
            .filter(|e| oldest.is_none_or(|o| e.at < o))
            .map(|e| CheckEntry {
                at: e.at,
                state: if e.kind == "engine_up" { "ok" } else { "down" }.to_string(),
                detail: if e.kind == "engine_up" {
                    "Online".to_string()
                } else {
                    e.body["error"].as_str().unwrap_or("Offline").to_string()
                },
                via: "journal".to_string(),
                ms: None,
            }),
    );
    log.truncate(30);
    Ok(IntegrationDetail {
        integration: panel,
        settings,
        log,
    })
}

/// Say what Singularr may do with an integration: `{"access": "read"}` or `{"access": "act"}`.
/// The owner alone may. It is in force at once, without a restart, and each change is an
/// `integration_access_changed` event. 400 for a type Singularr has no action for; 409 when the
/// environment or the instance file fixes the access, naming the variable or the file.
#[utoipa::path(patch, path = "/api/v1/integrations/{id}", tag = "system", request_body = IntegrationPatch,
    params(("id" = String, Path, description = "The integration's id")),
    responses((status = 200, body = IntegrationDetail), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 409, description = "The environment or the instance file fixes it", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn integration_patch(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
    ApiJson(patch): ApiJson<IntegrationPatch>,
) -> Result<Response, AppError> {
    if !matches!(caller, Caller::Owner { .. }) {
        return Err(AppError::Forbidden(
            "Only the owner can change integration access.".to_string(),
        ));
    }
    let access = Access::parse(&patch.access).ok_or_else(|| {
        AppError::bad_request(format!(
            "`access` is `read` or `act`, not `{}`.",
            patch.access.trim()
        ))
    })?;
    let missing = || AppError::not_found(format!("There is no integration `{id}`."));
    if state.config.integration(&id).is_none() {
        // A metadata provider is listed with the integrations: it is asked for records and
        // nothing else.
        let provider = state
            .metadata
            .list()
            .into_iter()
            .find(|p| p.id == id && matches!(p.id.as_str(), "tmdb" | "tvdb"));
        return Err(match provider {
            Some(p) => AppError::bad_request(format!("{} is read-only.", p.name)),
            None => missing(),
        });
    }
    let set = state
        .engines
        .access
        .set(&state.pool, &state.journal, &caller.actor(), &id, access)
        .await;
    match set {
        Ok(_) => {}
        Err(AccessError::Unknown(_)) => return Err(missing()),
        Err(e @ AccessError::NoActions { .. }) => return Err(AppError::bad_request(e.to_string())),
        Err(e @ AccessError::Fixed { .. }) => {
            return Ok(conflict_of("Set in configuration", &e.to_string()));
        }
        Err(AccessError::Db(e)) => return Err(AppError::Db(e)),
    }
    Ok(Json(integration_detail(&state, &caller, &id).await?).into_response())
}

/// One read-only call to an integration now, with the time it took: its status, its identity,
/// its list of downloads, or TMDB's configuration. The answer says why when it cannot be asked.
#[utoipa::path(post, path = "/api/v1/integrations/{id}/test", tag = "system", params(("id" = String, Path, description = "The integration's id")),
    responses((status = 200, body = IntegrationTest), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn integration_test(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<IntegrationTest> {
    let refused = |detail: &str| {
        Json(IntegrationTest {
            ok: false,
            ms: 0,
            version: None,
            detail: detail.to_string(),
        })
    };
    let started = std::time::Instant::now();
    let outcome: Result<(Option<String>, String), String> = match state.config.integration(&id) {
        Some(i) => {
            if let Some(why) = i.missing() {
                return Ok(refused(&format!("Not configured: {why}")));
            }
            let label = i.kind.label();
            match test_route(i.kind.manifest()) {
                Some(Kind::Manager) => match state.engines.arr(&id) {
                    Some(engine) => engine
                        .status()
                        .await
                        .map(|info| {
                            let app = info.app.clone().unwrap_or_else(|| label.to_string());
                            let detail = match &info.version {
                                Some(v) => format!("{app} {v}"),
                                None => app,
                            };
                            (info.version, detail)
                        })
                        .map_err(|e| e.to_string()),
                    None => Err("Not configured".to_string()),
                },
                Some(Kind::DownloadClient) => match state.engines.download_client(&id) {
                    Some(client) => client
                        .downloads()
                        .await
                        .map(|listed| {
                            let n = listed.len() as i64;
                            let words = match client.protocol() {
                                Protocol::Torrent => units::counted(n, "torrent", "torrents"),
                                Protocol::Usenet => units::counted(n, "download", "downloads"),
                            };
                            (None, words)
                        })
                        .map_err(|e| e.to_string()),
                    None => Err("Not configured".to_string()),
                },
                Some(Kind::RequestSource) => {
                    match state.engines.request_sites.iter().find(|s| s.id() == id) {
                        Some(site) => site
                            .status()
                            .await
                            .map(|info| {
                                let detail = match &info.version {
                                    Some(v) => format!("{label} {v}"),
                                    None => label.to_string(),
                                };
                                (info.version, detail)
                            })
                            .map_err(|e| e.to_string()),
                        None => Err("Only the first request source is used".to_string()),
                    }
                }
                Some(Kind::MediaServer) => match state.engines.media_server_of(&id) {
                    Some(server) => server
                        .identity()
                        .await
                        .map(|s| (s.version, s.name))
                        .map_err(|e| e.to_string()),
                    None => Err("Not read: first of its type only".to_string()),
                },
                Some(Kind::Processor) => match state.engines.processor(&id) {
                    Some(processor) => processor
                        .status()
                        .await
                        .map(|s| {
                            let detail = match &s.version {
                                Some(v) => format!("{label} {v}: {}", s.detail),
                                None => format!("{label}: {}", s.detail),
                            };
                            (s.version, detail)
                        })
                        .map_err(|e| e.to_string()),
                    None => Err("Not configured".to_string()),
                },
                Some(Kind::PlayHistory) => match &state.engines.play_history {
                    Some(history) if history.id() == id => history
                        .status()
                        .await
                        .map(|info| {
                            let detail = match &info.version {
                                Some(v) => format!("{label} {v}"),
                                None => label.to_string(),
                            };
                            (info.version, detail)
                        })
                        .map_err(|e| e.to_string()),
                    _ => Err(format!("Only the first {label} is used")),
                },
                // A type that is listed and not read, and a kind no type of this version fills.
                _ => {
                    return Ok(refused("Not supported"));
                }
            }
        }
        None if id == "tmdb" => {
            if !state
                .metadata
                .list()
                .iter()
                .any(|p| p.id == "tmdb" && p.configured)
            {
                return Ok(refused("Not configured: SINGULARR_TMDB_TOKEN is not set"));
            }
            state
                .metadata
                .tmdb
                .get_json("/configuration")
                .await
                .map(|_| (None, "TMDB".to_string()))
                .map_err(|e| e.to_string())
        }
        None if id == "tvdb" => {
            return Ok(refused("Not tested"));
        }
        None => {
            return Err(AppError::not_found(format!(
                "There is no integration `{id}`."
            )));
        }
    };
    let ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let (ok, version, detail) = match outcome {
        Ok((version, detail)) => (true, version, detail),
        Err(why) => (false, None, why),
    };
    jobs::registry().record_check(
        &id,
        Check {
            at: Utc::now(),
            ok,
            via: "test".to_string(),
            ms,
            detail: detail.clone(),
        },
    );
    Ok(Json(IntegrationTest {
        ok,
        ms,
        version,
        detail,
    }))
}

/// Read an integration now: every job that reads it is woken. Needs `library`, like a folder
/// scan: it asks the integration more.
#[utoipa::path(post, path = "/api/v1/integrations/{id}/sync", tag = "system", params(("id" = String, Path, description = "The integration's id")),
    responses((status = 202, body = Started), (status = 404, body = ProblemDoc), (status = 409, description = "Nothing to start: the sync is off, nothing reads it, or its jobs are running", body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn integration_sync(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
) -> Result<Response, AppError> {
    caller.require(Scope::Library)?;
    if state.config.integration(&id).is_none() {
        return Err(AppError::not_found(format!(
            "There is no integration `{id}`."
        )));
    }
    let jobs = jobs::registry().of_integration(&id);
    if jobs.is_empty() {
        return Ok(conflict(if state.config.sync.enabled {
            "No job reads this integration."
        } else {
            "Integration sync is off (SINGULARR_SYNC_ENABLED)."
        }));
    }
    let started: Vec<String> = jobs
        .iter()
        .filter(|j| j.run_now() == RunNow::Started)
        .map(|j| j.spec.id.clone())
        .collect();
    if started.is_empty() {
        return Ok(conflict(
            "The jobs of this integration are already running.",
        ));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(Started {
            started: true,
            jobs: started,
        }),
    )
        .into_response())
}

/// The libraries: the media servers' sections with what each section's manager holds (titles,
/// files, size, the profiles the policy gives it), the servers' music, and a manager's own
/// library that no section stands for.
#[utoipa::path(get, path = "/api/v1/libraries", tag = "catalog",
    responses((status = 200, body = LibraryPanels), (status = 401, body = ProblemDoc)))]
async fn libraries(State(state): State<AppState>) -> ApiResult<LibraryPanels> {
    let rows = crate::catalog::service::libraries(&state.pool).await?;
    let holdings = catalog_store::library_holdings(&state.pool).await?;
    let albums = crate::music::store::server_albums(&state.pool).await?;
    let music = (!albums.is_empty()).then(|| {
        (
            albums.len() as i64,
            albums.iter().map(|a| a.tracks).sum::<i64>(),
            albums.iter().map(|a| a.size_bytes).sum::<i64>(),
        )
    });
    let last_ok = engine_store::list(&state.pool)
        .await?
        .into_iter()
        .filter_map(|r| r.last_ok_at.map(|at| (r.engine, at)))
        .collect();
    let libraries = libraries_panel(
        &rows,
        &LibraryFacts {
            holdings: &holdings,
            policy: &state.policy.current().policy,
            albums: music,
            last_ok: &last_ok,
        },
    );
    Ok(Json(LibraryPanels {
        total: libraries.len() as i64,
        libraries,
    }))
}

/// Who signs in: the owner (by address, to callers with `settings`), the people the server is
/// shared with (their Seerr accounts, with their requests and their last sign-in on the
/// portal), and the programs with API keys; the roles and their permissions, as data.
#[utoipa::path(get, path = "/api/v1/people", tag = "system",
    responses((status = 200, body = PeoplePanel), (status = 401, body = ProblemDoc)))]
async fn people(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<PeoplePanel> {
    let may_see = caller.has(Scope::Settings);
    let blank = |id: String, name: String, kind: &str, source: &str| PersonView {
        id,
        name,
        kind: kind.to_string(),
        role: None,
        sign_in: None,
        last_seen: None,
        last_request: None,
        requests: None,
        limits: None,
        season_gate: None,
        auto_approve: None,
        target: None,
        since: None,
        scopes: None,
        source: source.to_string(),
    };
    let mut people: Vec<PersonView> = state
        .config
        .owner_emails
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .enumerate()
        .map(|(n, address)| {
            let (id, name) = if may_see {
                (format!("user:{address}"), address.to_string())
            } else {
                (format!("owner-{}", n + 1), "Owner".to_string())
            };
            PersonView {
                role: Some("owner".to_string()),
                sign_in: Some("proxy".to_string()),
                ..blank(id, name, "person", "configuration")
            }
        })
        .collect();
    let rules = &state.requests.rules;
    if !state.config.seerr.is_empty() {
        for a in crate::requests::store::accounts(&state.pool, crate::engines::seerr::SITE).await? {
            let member = !a.is_owner;
            people.push(PersonView {
                role: Some(if member { "member" } else { "owner" }.to_string()),
                sign_in: a.plex_id.map(|_| "plex".to_string()),
                last_seen: a.last_sign_in,
                last_request: a.last_request,
                requests: Some(a.requests),
                season_gate: (member && rules.season_gate)
                    .then(|| i64::try_from(rules.max_seasons).unwrap_or(i64::MAX)),
                target: Some("HD".to_string()),
                ..blank(
                    format!("seerr-{}", a.seerr_user_id),
                    a.display_name,
                    "person",
                    "seerr",
                )
            });
        }
    }
    for k in auth::list_keys(&state.pool)
        .await?
        .into_iter()
        .filter(|k| k.revoked_at.is_none())
    {
        people.push(PersonView {
            role: k
                .scopes
                .contains(&Scope::Admin)
                .then(|| "admin".to_string()),
            sign_in: Some("key".to_string()),
            last_seen: k.last_used,
            since: Some(k.created_at),
            scopes: Some(k.scopes),
            ..blank(format!("key-{}", k.id), k.name, "program", "keys")
        });
    }
    Ok(Json(PeoplePanel {
        people,
        roles: panels::roles(),
        permissions: panels::permissions(),
    }))
}

/// Every loop this process runs, the reads of each integration and the folder scans, each
/// with how often it runs, its last run (since this start), the result, how long it took and
/// when it runs next; and what Singularr keeps to answer faster.
#[utoipa::path(get, path = "/api/v1/jobs", tag = "system",
    responses((status = 200, body = JobsPanel), (status = 401, body = ProblemDoc)))]
async fn jobs_list(State(state): State<AppState>) -> ApiResult<JobsPanel> {
    let mut cache = Vec::new();
    if let Some((entries, size)) =
        crate::settings::store::table_facts(&state.pool, "metadata_cache").await?
    {
        cache.push(CacheView {
            id: "metadata".to_string(),
            label: "Metadata".to_string(),
            entries,
            size,
            detail: format!(
                "Records {} d, lists and searches {} d",
                crate::metadata::records::RECORD_TTL,
                crate::metadata::records::LIST_TTL
            ),
        });
    }
    if let Some((entries, size)) =
        crate::settings::store::table_facts(&state.pool, "request_media").await?
    {
        cache.push(CacheView {
            id: "lookups".to_string(),
            label: "Title lookups".to_string(),
            entries,
            size,
            detail: "Request titles from Seerr".to_string(),
        });
    }
    cache.push(CacheView {
        id: "images".to_string(),
        label: "Artwork".to_string(),
        entries: 0,
        size: 0,
        detail: "Not cached".to_string(),
    });
    Ok(Json(JobsPanel {
        jobs: jobs::registry()
            .list()
            .iter()
            .map(|j| job_view(j, &state.config))
            .collect(),
        cache,
        sync_enabled: state.config.sync.enabled,
        since: state.settings.started(),
    }))
}

/// Run a job now: its loop wakes and runs once, then waits its interval again. Needs `library`.
#[utoipa::path(post, path = "/api/v1/jobs/{id}/run", tag = "system", params(("id" = String, Path, description = "The job's id: radarr:titles, folder:music")),
    responses((status = 202, body = Started), (status = 404, body = ProblemDoc), (status = 409, description = "It is running now", body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn job_run(
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
) -> Result<Response, AppError> {
    caller.require(Scope::Library)?;
    let job = jobs::registry()
        .get(&id)
        .ok_or_else(|| AppError::not_found(format!("There is no job `{id}`.")))?;
    Ok(match job.run_now() {
        RunNow::Started => (
            StatusCode::ACCEPTED,
            Json(Started {
                started: true,
                jobs: vec![id],
            }),
        )
            .into_response(),
        RunNow::Running => conflict("The job is already running."),
    })
}

/// This build and this process: version, build, when it started, the mode; the platform
/// services (database, search, cache, artwork) with what each runs on; what `singularrd doctor`
/// checks, as data, from what the process knows (it asks no integration); paths and links.
#[utoipa::path(get, path = "/api/v1/system", tag = "system",
    responses((status = 200, body = SystemView), (status = 401, body = ProblemDoc)))]
async fn system(State(state): State<AppState>) -> ApiResult<SystemView> {
    let config = &state.config;
    let asked = std::time::Instant::now();
    let answered = core_store::ping(&state.pool).await;
    let ping_ms = asked.elapsed().as_millis();
    let facts = crate::settings::store::database_facts(&state.pool).await;
    let in_use = (state.pool.size() as usize).saturating_sub(state.pool.num_idle());
    let database = ServiceView {
        id: "database".to_string(),
        label: "Database".to_string(),
        provider: "postgresql".to_string(),
        choices: vec![panels::choice(
            "postgresql",
            "PostgreSQL",
            "PostgreSQL 16 or later (DATABASE_URL)",
        )],
        state: if facts.is_ok() { "ok" } else { "down" }.to_string(),
        detail: match &facts {
            Ok((version, bytes)) => format!(
                "PostgreSQL {version}, {}, {in_use} of {} connections",
                units::size(*bytes),
                config.db_max_connections
            ),
            Err(e) => format!("No answer: {e}"),
        },
    };
    let cache_bytes = crate::settings::store::table_facts(&state.pool, "metadata_cache")
        .await?
        .map(|(_, bytes)| bytes)
        .unwrap_or(0);
    // The search service says itself which provider answers, how much it holds and whether a
    // configured Meilisearch is failing (the built-in search answers then).
    let (provider, search_state, search_detail) = match state.search.status().await {
        Ok(s) => {
            let detail = s
                .detail
                .clone()
                .unwrap_or_else(|| match s.provider.as_str() {
                    "meilisearch" => format!("Meilisearch, {} entries", units::count(s.documents)),
                    _ if s.typo_tolerant => format!(
                        "Built in, {} entries, typo tolerant",
                        units::count(s.documents)
                    ),
                    _ => format!("Built in, {} entries", units::count(s.documents)),
                });
            (s.provider, s.state, detail)
        }
        Err(_) => (
            "postgresql".to_string(),
            "warn".to_string(),
            "Status unavailable".to_string(),
        ),
    };
    let services = vec![
        database,
        ServiceView {
            id: "search".to_string(),
            label: "Search".to_string(),
            provider,
            choices: vec![
                panels::choice("postgresql", "Built in", "None"),
                panels::choice(
                    "meilisearch",
                    "Meilisearch",
                    "Meilisearch address and API key",
                ),
            ],
            state: search_state,
            detail: search_detail,
        },
        ServiceView {
            id: "cache".to_string(),
            label: "Cache".to_string(),
            provider: "postgresql".to_string(),
            choices: vec![panels::choice("postgresql", "Database", "None")],
            state: "ok".to_string(),
            detail: format!("Database, {}", units::size(cache_bytes)),
        },
        ServiceView {
            id: "artwork".to_string(),
            label: "Artwork".to_string(),
            provider: "hotlink".to_string(),
            choices: vec![panels::choice("hotlink", "Provider image hosts", "None")],
            state: "ok".to_string(),
            detail: "Provider image hosts; music artwork from the media server".to_string(),
        },
    ];

    let mut checks = vec![match &answered {
        Ok(()) => check("database", "Database", "ok", format!("{ping_ms} ms")),
        Err(e) => check("database", "Database", "down", e.to_string()),
    }];
    checks.push(match db::pending_migrations(&state.pool).await {
        Ok(p) if p.is_empty() => check("migrations", "Migrations", "ok", "All applied"),
        Ok(p) => check(
            "migrations",
            "Migrations",
            "warn",
            format!("{} pending: {}", p.len(), p.join(", ")),
        ),
        Err(e) => check("migrations", "Migrations", "down", e.to_string()),
    });
    let policy = state.policy.current();
    checks.push(match &policy.source {
        crate::policy::PolicySource::File(path) => check(
            "policy",
            "Policy",
            "ok",
            format!("{} {}, {path}", policy.policy.name, policy.policy.version),
        ),
        crate::policy::PolicySource::Saved(version) => check(
            "policy",
            "Policy",
            "ok",
            format!("Saved, version {version}"),
        ),
        crate::policy::PolicySource::Builtin => check(
            "policy",
            "Policy",
            "warn",
            format!("{} missing, built-in policy in use", config.policy_path),
        ),
    });
    checks.push(match crate::policy::load_editions(&config.editions_path) {
        Ok((_, None)) => check(
            "editions",
            "Edition list",
            "ok",
            config.editions_path.clone(),
        ),
        Ok((_, Some(_))) => check(
            "editions",
            "Edition list",
            "warn",
            format!("{} missing, built-in list in use", config.editions_path),
        ),
        Err(e) => check("editions", "Edition list", "down", e.to_string()),
    });
    let web = state
        .settings
        .text("system.webDir")
        .unwrap_or_else(|| "/app/web".to_string());
    checks.push(if std::path::Path::new(&web).join("index.html").is_file() {
        check("web", "Web client", "ok", web.clone())
    } else {
        check(
            "web",
            "Web client",
            "warn",
            format!("{web} has no index.html"),
        )
    });
    let integrations = integration_panels(&state).await?;
    // The processing services have a check each, below: one that is silent leaves files as
    // they were imported, and that is said by name.
    let asked_ones: Vec<&IntegrationPanel> = integrations
        .iter()
        .filter(|p| {
            p.configured && p.group != "metadata" && p.group != "processing" && !p.reads.is_empty()
        })
        .collect();
    let not_ok: Vec<String> = asked_ones
        .iter()
        .filter(|p| p.state != "ok")
        .map(|p| format!("{}: {}", p.name, p.detail))
        .collect();
    checks.push(check(
        "integrations",
        "Integrations",
        panels::worst(asked_ones.iter().map(|p| p.state.as_str())),
        if asked_ones.is_empty() {
            "None configured".to_string()
        } else if not_ok.is_empty() {
            format!("{} healthy", asked_ones.len())
        } else {
            not_ok.join("; ")
        },
    ));
    checks.extend(
        integrations
            .iter()
            .filter(|p| p.configured && p.group == "processing")
            .map(|p| {
                check(
                    &format!("processing-{}", p.id),
                    "Processing service",
                    &p.state,
                    format!("{}: {}", p.name, p.detail),
                )
            }),
    );
    let mut folders = vec![
        ("music", "Music folder", config.music.root.clone()),
        (
            "home-movies",
            "Home movies folder",
            config.home_movies.root.clone(),
        ),
    ];
    folders.extend(
        config
            .movie_folders
            .roots
            .iter()
            .map(|r| ("movie-folder", "Movie root folder", r.clone())),
    );
    for (n, (id, label, root)) in folders.into_iter().enumerate() {
        let id = if id == "movie-folder" {
            format!("folder-{id}-{n}")
        } else {
            format!("folder-{id}")
        };
        checks.push(match std::fs::read_dir(&root) {
            Ok(entries) => check(
                &id,
                label,
                "ok",
                format!(
                    "{root}, {}",
                    units::counted(entries.count() as i64, "entry", "entries")
                ),
            ),
            Err(e) => check(&id, label, "warn", format!("{root} unreadable: {e}")),
        });
    }
    checks.extend(panels::config_checks(config));
    Ok(Json(SystemView {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: env!("SINGULARR_BUILD_ID").to_string(),
        started: state.settings.started(),
        mode: state.engines.access.mode().to_string(),
        services,
        checks,
        paths: SystemPaths {
            web,
            policy: config.policy_path.clone(),
            editions: config.editions_path.clone(),
            instance: config.instance_path.clone(),
        },
        links: SystemLinks {
            reference: "/api/docs".to_string(),
            openapi: "/api/v1/openapi.json".to_string(),
        },
    }))
}

/// The configuration routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/settings", get(settings_view).patch(settings_patch))
        .route("/integrations", get(integrations))
        .route(
            "/integrations/{id}",
            get(integration_one).patch(integration_patch),
        )
        .route("/integrations/{id}/test", post(integration_test))
        .route("/integrations/{id}/sync", post(integration_sync))
        .route("/libraries", get(libraries))
        .route("/people", get(people))
        .route("/jobs", get(jobs_list))
        .route("/jobs/{id}/run", post(job_run))
        .route("/system", get(system))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        settings_view, settings_patch, integrations, integration_one, integration_patch,
        integration_test, integration_sync, libraries, people, jobs_list, job_run, system
    ),
    tags((name = "settings", description = "Every setting with its value and its source, and saving them"))
)]
struct ConfigDoc;

/// The configuration routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    ConfigDoc::openapi()
}

#[cfg(test)]
mod tests {
    /// The Test of an integration asks the main kind of its type, as its manifest states it: a
    /// new type needs no other edit. A type that is listed and not read is not asked.
    #[test]
    fn the_test_asks_the_kind_a_manifest_states() {
        use super::{Kind, Manifest, test_route};
        use crate::engines::manifests::{PLEX, PROWLARR, TAUTULLI, tests::SAMPLE};
        assert_eq!(test_route(&SAMPLE), Some(Kind::PlayHistory));
        assert_eq!(test_route(&TAUTULLI), Some(Kind::PlayHistory));
        assert_eq!(test_route(&PLEX), Some(Kind::MediaServer));
        assert_eq!(test_route(&PROWLARR), None);
        let unread = Manifest {
            reads: &[],
            passes: &[],
            ..SAMPLE
        };
        assert_eq!(test_route(&unread), None);
    }

    #[test]
    fn the_openapi_document_lists_the_configuration_routes() {
        let doc = serde_json::to_value(super::super::routes::openapi()).unwrap();
        let paths = doc["paths"].as_object().unwrap();
        for (path, method) in [
            ("/api/v1/settings", "get"),
            ("/api/v1/settings", "patch"),
            ("/api/v1/integrations", "get"),
            ("/api/v1/integrations/{id}", "get"),
            ("/api/v1/integrations/{id}", "patch"),
            ("/api/v1/integrations/{id}/test", "post"),
            ("/api/v1/integrations/{id}/sync", "post"),
            ("/api/v1/libraries", "get"),
            ("/api/v1/people", "get"),
            ("/api/v1/jobs", "get"),
            ("/api/v1/jobs/{id}/run", "post"),
            ("/api/v1/system", "get"),
        ] {
            assert!(
                paths.get(path).and_then(|p| p.get(method)).is_some(),
                "{method} {path} is missing from the OpenAPI document"
            );
        }
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for name in [
            "SettingsView",
            "FieldView",
            "SettingsPatch",
            "SettingsSaved",
            "SettingsRefused",
            "IntegrationPanels",
            "IntegrationDetail",
            "IntegrationPatch",
            "LibraryPanels",
            "PeoplePanel",
            "JobsPanel",
            "SystemView",
        ] {
            assert!(schemas.contains_key(name), "schema {name} is missing");
        }
    }
}
