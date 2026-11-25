//! The notification routes: the types of channel with their fields, the events with their
//! groups, the channels and what was sent to them; adding, changing, removing and testing a
//! channel. `routes::router` merges them with one line.
//!
//! Every route needs a caller with `read` (the layer in `routes::router`); whatever changes or
//! sends is the owner's alone. A secret (a webhook address, a token, a password) is
//! write-only: an answer says whether it is set, never what it is.
//!
//! The dispatcher in `notify` is handed a [`Describer`] from here: it turns journal events into
//! notices with the journal route's words and the title cards.

use super::journal::{self, JournalEvent, Names};
use super::pages::labels;
use super::routes::{ApiJson, ApiPath, ApiQuery};
use super::state::AppState;
use crate::catalog::store as catalog_store;
use crate::core::auth::Caller;
use crate::core::config::Config;
use crate::core::error::{AppError, AppResult};
use crate::core::model::Event;
use crate::core::schema::ProblemDoc;
use crate::notify::message;
use crate::notify::model::{
    self, Card, ChannelType, EVENTS, Fact, GROUPS, KINDS, Notice, label_of,
};
use crate::notify::service::{
    ChangeError, ChannelInput, ChannelPatch, Describe, TestInput, TestOutcome, checked_of,
};
use crate::notify::store::{ChannelRow, DeliveryRow};
use crate::settings::Settings;
use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use utoipa::OpenApi;

/// Deliveries the page lists at first.
const NEWEST: i64 = 50;
/// Deliveries one page of `GET /api/v1/notifications/deliveries` holds at most.
const MAX_LIMIT: u32 = 500;

/// One field of a type of channel, as the settings kit draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FieldView {
    pub key: String,
    pub label: String,
    /// How it is entered: `url`, `text`, `secret`, `number`, `select` or `list`.
    #[serde(rename = "type")]
    pub kind: String,
    pub required: bool,
    /// Write-only: an answer says whether it is set, never what it is.
    pub secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<OptionView>,
    /// The value it holds when nothing is given.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    #[schema(value_type = Object)]
    pub default: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OptionView {
    pub value: String,
    pub label: String,
}

/// A type of channel with the fields its form is drawn from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TypeView {
    /// `slack`, `discord`, `email`, `webhook`, `ntfy`, `telegram`, `pushover`, `gotify`.
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    pub fields: Vec<FieldView>,
}

/// A group of events, or a kind a channel may narrow to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Named {
    pub id: String,
    pub label: String,
}

/// An event a channel can subscribe to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EventView {
    /// `request.created`, `file.upgraded`.
    pub id: String,
    /// The group's id: `requests`, `downloads`, `library`, `processing`, `system`.
    pub group: String,
    pub label: String,
}

/// One message to one channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryView {
    pub id: i64,
    pub channel: i64,
    pub channel_name: Option<String>,
    /// The event (`file.upgraded`), or `test`.
    pub event: String,
    pub label: String,
    /// The first line of the message.
    pub headline: String,
    /// The journal event it is for (`e-123`); none for a test.
    pub journal_event: Option<String>,
    /// `pending` (waiting for its next attempt), `sending`, `sent` or `failed`.
    pub state: String,
    pub attempts: i32,
    pub at: DateTime<Utc>,
    /// When the next attempt is made, while it waits.
    pub next_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// The destination's status (HTTP), when it answered.
    pub status: Option<i32>,
    /// The destination's own words, or why nothing reached it.
    pub detail: Option<String>,
    pub test: bool,
}

/// A channel as an answer shows it: its settings without its secrets, and which secrets are
/// set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub type_label: String,
    pub name: String,
    pub enabled: bool,
    /// The values of its fields that are no secret.
    #[schema(value_type = Object)]
    pub settings: Map<String, Value>,
    /// Each secret field, and whether it is set.
    pub secrets: BTreeMap<String, bool>,
    pub events: Vec<String>,
    /// `movie`, `show`; none: both.
    pub kinds: Vec<String>,
    /// Where it sends, in words that hold no secret.
    pub destination: String,
    /// `ok` (its last delivery was sent), `failing` (its last delivery failed), `off`
    /// (disabled) or `new` (nothing sent yet).
    pub state: String,
    pub last_delivery: Option<DeliveryView>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

/// `GET /api/v1/notifications`: the types, the events, the channels and the newest
/// deliveries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NotificationsView {
    pub types: Vec<TypeView>,
    pub groups: Vec<Named>,
    pub events: Vec<EventView>,
    /// The kinds a channel may narrow to.
    pub kinds: Vec<Named>,
    pub channels: Vec<ChannelView>,
    /// The newest deliveries, newest first.
    pub deliveries: Vec<DeliveryView>,
}

/// One page of deliveries, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DeliveriesPage {
    pub deliveries: Vec<DeliveryView>,
    /// The `before` of the next, older page; absent on the last page.
    pub next: Option<i64>,
}

/// A channel that was not taken, nothing of it stored: why, by key (`name`, `events`,
/// `settings.url`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChannelRefused {
    pub errors: BTreeMap<String, String>,
}

/// The document a webhook receives (`POST`, `application/json`), with the headers
/// `Singularr-Event` (the event), `Singularr-Delivery` (the delivery's id) and, when the channel
/// has a secret, `Authorization: Bearer <secret>`.
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
pub struct WebhookDocument {
    /// The event: `file.upgraded`, or `test`.
    pub event: String,
    pub label: String,
    /// The group's id, or `test`.
    pub group: String,
    pub test: bool,
    pub at: DateTime<Utc>,
    /// `movie` or `show`, when the event is about one.
    pub kind: Option<String>,
    /// What happened, in one line.
    pub summary: String,
    /// What it is about.
    pub subject: String,
    pub title: Option<Card>,
    pub facts: Vec<Fact>,
    /// The title's page, when the workspace address is set.
    pub link: Option<String>,
    /// The event as `GET /api/v1/journal` gives it; null for a test.
    pub journal: Option<JournalEvent>,
}

/// Filters of `GET /api/v1/notifications/deliveries`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeliveryQuery {
    /// One channel's deliveries.
    pub channel: Option<i64>,
    /// Deliveries on the page, 1 to 500. Default 100.
    pub limit: Option<u32>,
    /// Only deliveries older than this one: the `next` of the page before.
    pub before: Option<i64>,
}

// Views ------------------------------------------------------------------------------------------

fn type_view(kind: ChannelType) -> TypeView {
    TypeView {
        kind: kind.as_str().to_string(),
        label: kind.label().to_string(),
        fields: kind
            .fields()
            .iter()
            .map(|f| FieldView {
                key: f.key.to_string(),
                label: f.label.to_string(),
                kind: f.input.as_str().to_string(),
                required: f.required,
                secret: f.is_secret(),
                placeholder: f.placeholder.map(str::to_string),
                help: f.help.map(str::to_string),
                options: f
                    .options
                    .iter()
                    .map(|(value, label)| OptionView {
                        value: value.to_string(),
                        label: label.to_string(),
                    })
                    .collect(),
                default: match f.default {
                    Some(model::Fallback::Text(text)) => Value::from(text),
                    Some(model::Fallback::Int(n)) => Value::from(n),
                    None => Value::Null,
                },
            })
            .collect(),
    }
}

/// Every type of channel, every group, event and kind: what the panel draws its forms from.
pub fn catalog() -> (Vec<TypeView>, Vec<Named>, Vec<EventView>, Vec<Named>) {
    let named = |list: &[(&str, &str)]| {
        list.iter()
            .map(|(id, label)| Named {
                id: id.to_string(),
                label: label.to_string(),
            })
            .collect::<Vec<_>>()
    };
    (
        ChannelType::ALL.into_iter().map(type_view).collect(),
        named(GROUPS),
        EVENTS
            .iter()
            .map(|e| EventView {
                id: e.id.to_string(),
                group: e.group.to_string(),
                label: e.label.to_string(),
            })
            .collect(),
        named(KINDS),
    )
}

fn delivery_view(d: &DeliveryRow, names: &HashMap<i64, String>) -> DeliveryView {
    let notice: Option<Notice> = serde_json::from_value(d.notice.clone()).ok();
    DeliveryView {
        id: d.id,
        channel: d.channel_id,
        channel_name: names.get(&d.channel_id).cloned(),
        event: d.event.clone(),
        label: label_of(&d.event).to_string(),
        headline: notice
            .as_ref()
            .map(message::headline)
            .unwrap_or_else(|| label_of(&d.event).to_string()),
        journal_event: d.event_id.map(|id| format!("e-{id}")),
        state: d.state.clone(),
        attempts: d.attempts,
        at: d.created_at,
        next_at: (d.state == "pending").then_some(d.next_at),
        finished_at: d.finished_at,
        status: d.status,
        detail: d.detail.clone(),
        test: d.test,
    }
}

fn channel_view(
    row: &ChannelRow,
    last: Option<&DeliveryRow>,
    names: &HashMap<i64, String>,
) -> ChannelView {
    let kind = ChannelType::parse(&row.kind);
    let checked = checked_of(row);
    let state = match last {
        _ if !row.enabled => "off",
        None => "new",
        Some(d) if d.state == "failed" => "failing",
        Some(_) => "ok",
    };
    ChannelView {
        id: row.id,
        kind: row.kind.clone(),
        type_label: kind.map(|k| k.label()).unwrap_or("Unknown").to_string(),
        name: row.name.clone(),
        enabled: row.enabled,
        settings: row.settings.clone(),
        secrets: kind.map(|k| checked.set(k)).unwrap_or_default(),
        events: row.events.clone(),
        kinds: row.kinds.clone(),
        destination: kind
            .map(|k| model::destination(k, &checked))
            .unwrap_or_default(),
        state: state.to_string(),
        last_delivery: last.map(|d| delivery_view(d, names)),
        created_at: row.created_at,
        created_by: row.created_by.clone(),
        updated_at: row.updated_at,
        updated_by: row.updated_by.clone(),
    }
}

async fn channel_names(state: &AppState) -> AppResult<HashMap<i64, String>> {
    Ok(state
        .notify
        .channels()
        .await?
        .into_iter()
        .map(|c| (c.id, c.name))
        .collect())
}

/// One channel's record, with its last delivery.
async fn one(state: &AppState, row: &ChannelRow) -> AppResult<ChannelView> {
    let last = state.notify.last_ended().await?;
    let names = HashMap::from([(row.id, row.name.clone())]);
    Ok(channel_view(row, last.get(&row.id), &names))
}

// Routes -----------------------------------------------------------------------------------------

/// Anything that changes or sends is the owner's.
fn owner_only(caller: &Caller) -> Result<(), AppError> {
    match caller {
        Caller::Owner { .. } => Ok(()),
        _ => Err(AppError::Forbidden(
            "Only the owner can change or test notification channels.".to_string(),
        )),
    }
}

/// A change that was not made, as its answer.
fn refused(e: ChangeError) -> Result<Response, AppError> {
    match e {
        ChangeError::Invalid(errors) => Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ChannelRefused { errors }),
        )
            .into_response()),
        ChangeError::NotFound => Err(AppError::not_found(
            "There is no such notification channel.",
        )),
        ChangeError::Db(e) => Err(AppError::Db(e)),
    }
}

/// The types of channel with their fields, the events with their groups, the channels (their
/// settings without secrets, which secrets are set, their state and last delivery) and the
/// newest deliveries.
#[utoipa::path(get, path = "/api/v1/notifications", tag = "notifications",
    responses((status = 200, body = NotificationsView), (status = 401, body = ProblemDoc)))]
async fn overview(State(state): State<AppState>) -> AppResult<Json<NotificationsView>> {
    let rows = state.notify.channels().await?;
    let last = state.notify.last_ended().await?;
    let names: HashMap<i64, String> = rows.iter().map(|c| (c.id, c.name.clone())).collect();
    let newest = state.notify.deliveries(None, None, NEWEST).await?;
    let (types, groups, events, kinds) = catalog();
    Ok(Json(NotificationsView {
        types,
        groups,
        events,
        kinds,
        channels: rows
            .iter()
            .map(|r| channel_view(r, last.get(&r.id), &names))
            .collect(),
        deliveries: newest.iter().map(|d| delivery_view(d, &names)).collect(),
    }))
}

/// Add a channel. It is told about what happens from now on. 422 with the reason per key when
/// what was given makes no channel.
#[utoipa::path(post, path = "/api/v1/notifications/channels", tag = "notifications", request_body = ChannelInput,
    responses((status = 201, body = ChannelView), (status = 422, description = "Nothing was saved: why, by key", body = ChannelRefused),
        (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn create(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(input): ApiJson<ChannelInput>,
) -> Result<Response, AppError> {
    owner_only(&caller)?;
    match state.notify.create(&input, &caller.actor()).await {
        Ok(row) => Ok((StatusCode::CREATED, Json(one(&state, &row).await?)).into_response()),
        Err(e) => refused(e),
    }
}

/// Change a channel: what the body leaves out stays. In `settings`, a field left out stays, a
/// secret given as empty text stays, and `null` empties a field. Disabled, it is sent nothing,
/// and enabled again, nothing from while it was off.
#[utoipa::path(patch, path = "/api/v1/notifications/channels/{id}", tag = "notifications", request_body = ChannelPatch,
    params(("id" = i64, Path, description = "The channel's id")),
    responses((status = 200, body = ChannelView), (status = 422, description = "Nothing was saved: why, by key", body = ChannelRefused),
        (status = 404, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn change(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(patch): ApiJson<ChannelPatch>,
) -> Result<Response, AppError> {
    owner_only(&caller)?;
    match state.notify.change(id, &patch, &caller.actor()).await {
        Ok(row) => Ok(Json(one(&state, &row).await?).into_response()),
        Err(e) => refused(e),
    }
}

/// Remove a channel and what was sent to it. The answer is the channel as it was.
#[utoipa::path(delete, path = "/api/v1/notifications/channels/{id}", tag = "notifications",
    params(("id" = i64, Path, description = "The channel's id")),
    responses((status = 200, body = ChannelView), (status = 404, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn remove(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
) -> Result<Response, AppError> {
    owner_only(&caller)?;
    match state.notify.remove(id, &caller.actor()).await {
        Ok(row) => {
            let names = HashMap::from([(row.id, row.name.clone())]);
            Ok(Json(channel_view(&row, None, &names)).into_response())
        }
        Err(e) => refused(e),
    }
}

/// Send one test message to a channel now. The answer says whether the destination took it,
/// in its own words when it did not; the delivery log keeps it.
#[utoipa::path(post, path = "/api/v1/notifications/channels/{id}/test", tag = "notifications",
    params(("id" = i64, Path, description = "The channel's id")),
    responses((status = 200, body = TestOutcome), (status = 404, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn test_saved(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
) -> Result<Response, AppError> {
    owner_only(&caller)?;
    match state.notify.test_saved(id).await {
        Ok(outcome) => Ok(Json(outcome).into_response()),
        Err(e) => refused(e),
    }
}

/// Send one test message to a channel that is not saved yet: a form is tested before it is
/// saved. With `channel`, a saved channel's secrets fill the ones the form left empty.
#[utoipa::path(post, path = "/api/v1/notifications/test", tag = "notifications", request_body = TestInput,
    responses((status = 200, body = TestOutcome), (status = 422, description = "Not a channel: why, by key", body = ChannelRefused),
        (status = 404, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn test_unsaved(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(input): ApiJson<TestInput>,
) -> Result<Response, AppError> {
    owner_only(&caller)?;
    match state.notify.test_unsaved(&input).await {
        Ok(outcome) => Ok(Json(outcome).into_response()),
        Err(e) => refused(e),
    }
}

/// What was sent, newest first: of one channel or of all, a page at a time.
#[utoipa::path(get, path = "/api/v1/notifications/deliveries", tag = "notifications", params(DeliveryQuery),
    responses((status = 200, body = DeliveriesPage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn deliveries(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<DeliveryQuery>,
) -> AppResult<Json<DeliveriesPage>> {
    let limit = i64::from(q.limit.unwrap_or(100).clamp(1, MAX_LIMIT));
    let mut rows = state
        .notify
        .deliveries(q.channel, q.before, limit + 1)
        .await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let names = channel_names(&state).await?;
    Ok(Json(DeliveriesPage {
        next: if more {
            rows.last().map(|d| d.id)
        } else {
            None
        },
        deliveries: rows.iter().map(|d| delivery_view(d, &names)).collect(),
    }))
}

/// The notification routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/notifications", get(overview))
        .route("/notifications/channels", post(create))
        .route("/notifications/channels/{id}", patch(change).delete(remove))
        .route("/notifications/channels/{id}/test", post(test_saved))
        .route("/notifications/test", post(test_unsaved))
        .route("/notifications/deliveries", get(deliveries))
}

#[derive(OpenApi)]
#[openapi(
    paths(overview, create, change, remove, test_saved, test_unsaved, deliveries),
    components(schemas(WebhookDocument)),
    tags((name = "notifications", description = "Channels (Slack, Discord, email, webhooks, ntfy, Telegram, Pushover, Gotify), the events they subscribe to, and what was sent to them"))
)]
struct NotificationsDoc;

/// The notification routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    NotificationsDoc::openapi()
}

// The describer ----------------------------------------------------------------------------------

/// A poster's whole address: a TMDB path at the size a message shows, a whole address as it
/// is. Anything else is no address a destination can fetch.
fn image(poster: &str) -> Option<String> {
    if poster.starts_with("https://") || poster.starts_with("http://") {
        return Some(poster.to_string());
    }
    let tmdb = poster.starts_with('/')
        && poster[1..].split_once('.').is_some_and(|(name, ext)| {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric())
                && matches!(ext, "jpg" | "png" | "svg" | "webp")
        });
    tmdb.then(|| format!("https://image.tmdb.org/t/p/w342{poster}"))
}

/// The catalog id of the title an event is about.
fn title_of(e: &Event) -> Option<i64> {
    e.body.get("title_id").and_then(Value::as_i64).or_else(|| {
        (e.subject_type == "title")
            .then(|| e.subject_id.parse().ok())
            .flatten()
    })
}

/// Journal events as notices: the event as the journal route gives it, its line in words, its
/// facts, its title card with the poster, and the link to the title's page when the workspace
/// address is set.
pub struct Describer {
    pool: PgPool,
    integrations: BTreeMap<String, String>,
    settings: Arc<Settings>,
}

impl Describer {
    pub fn new(pool: PgPool, config: &Config, settings: Arc<Settings>) -> Describer {
        Describer {
            pool,
            integrations: config
                .integrations
                .iter()
                .map(|i| (i.id.clone(), i.name.clone()))
                .collect(),
            settings,
        }
    }
}

#[async_trait]
impl Describe for Describer {
    async fn describe(&self, events: &[(Event, &'static str)]) -> Result<Vec<Notice>, String> {
        let mut ids: Vec<i64> = events.iter().filter_map(|(e, _)| title_of(e)).collect();
        ids.sort_unstable();
        ids.dedup();
        let refs = catalog_store::title_refs(&self.pool, &ids)
            .await
            .map_err(|e| e.to_string())?;
        let cards: HashMap<i64, Card> = catalog_store::title_cards(&self.pool, &ids)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| {
                let card = Card {
                    id: labels::title_id(&r.kind, r.tmdb_id),
                    kind: r.kind.clone(),
                    title: r.title.clone(),
                    year: r.year,
                    poster: labels::poster(r.poster_url.as_deref()).and_then(|p| image(&p)),
                };
                (r.id, card)
            })
            .collect();
        let names = Names {
            integrations: self.integrations.clone(),
            titles: refs.into_iter().map(|t| (t.id, t)).collect(),
            portal: self
                .settings
                .text("portal.name")
                .unwrap_or_else(|| "the portal".to_string()),
        };
        let base = self
            .settings
            .text("general.url")
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        Ok(events
            .iter()
            .map(|(e, id)| {
                let j = journal::journal_event(e, &names);
                let text = |key: &str| e.body.get(key).and_then(Value::as_str).map(str::to_string);
                // A title the catalog does not hold is named as its event names it.
                let card = title_of(e)
                    .and_then(|t| cards.get(&t).cloned())
                    .or_else(|| {
                        let kind = text("kind").filter(|k| k == "movie" || k == "show")?;
                        Some(Card {
                            id: labels::title_id(
                                &kind,
                                e.body.get("tmdb_id").and_then(Value::as_i64),
                            ),
                            title: text("title")?,
                            year: e
                                .body
                                .get("year")
                                .and_then(Value::as_i64)
                                .and_then(|y| i32::try_from(y).ok()),
                            poster: None,
                            kind,
                        })
                    });
                let link = match (&base, card.as_ref().and_then(|c| c.id.as_ref())) {
                    (Some(base), Some(page)) => Some(format!("{base}/title?id={page}")),
                    _ => None,
                };
                Notice {
                    event: id.to_string(),
                    seq: Some(e.id),
                    at: e.at,
                    kind: card
                        .as_ref()
                        .map(|c| c.kind.clone())
                        .or_else(|| text("kind").filter(|k| k == "movie" || k == "show")),
                    summary: j.summary.clone(),
                    subject: j.subject.label.clone(),
                    title: card,
                    facts: model::facts(&e.kind, &e.body),
                    link,
                    journal: serde_json::to_value(&j).unwrap_or(Value::Null),
                    test: false,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poster_is_a_whole_address_or_none() {
        assert_eq!(
            image("/abc123.jpg").as_deref(),
            Some("https://image.tmdb.org/t/p/w342/abc123.jpg")
        );
        assert_eq!(
            image("https://example.org/p.png").as_deref(),
            Some("https://example.org/p.png")
        );
        assert_eq!(image("/MediaCover/1/poster.jpg"), None);
        assert_eq!(image("poster.jpg"), None);
    }

    #[test]
    fn every_type_s_form_and_every_event_are_described() {
        let (types, groups, events, kinds) = catalog();
        assert_eq!(types.len(), 8);
        let email = types.iter().find(|t| t.kind == "email").unwrap();
        let port = email.fields.iter().find(|f| f.key == "port").unwrap();
        assert_eq!(
            (port.kind.as_str(), port.default.clone()),
            ("number", Value::from(587))
        );
        let password = email.fields.iter().find(|f| f.key == "password").unwrap();
        assert!(password.secret && password.kind == "secret" && !password.required);
        let security = email.fields.iter().find(|f| f.key == "security").unwrap();
        assert_eq!(security.options.len(), 3);
        assert_eq!(groups.len(), 5);
        assert_eq!(events.len(), EVENTS.len());
        assert_eq!(kinds.len(), 2);
        // The demo's sample (web/data/notify.json, copied to the fixture) holds the same types and events.
        let sample: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/contract_notify.json"))
                .unwrap();
        assert_eq!(sample["types"], serde_json::to_value(&types).unwrap());
        assert_eq!(sample["groups"], serde_json::to_value(&groups).unwrap());
        assert_eq!(sample["events"], serde_json::to_value(&events).unwrap());
        assert_eq!(sample["kinds"], serde_json::to_value(&kinds).unwrap());
        assert_eq!(sample["channels"], Value::Array(Vec::new()));
    }
}
