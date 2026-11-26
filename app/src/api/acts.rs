//! The `/api/v1/acts` routes: one act of the outbox as it stands. The owner cancels a pending act
//! before a worker takes it. The owner marks an unknown act not sent: its file is free for the
//! rules and for a new send. Each route is the owner's alone. The acts that need the owner's look
//! are items of the attention list ([`attention`]).
//!
//! An act asked for through the API takes the caller's `Idempotency-Key` header as its key
//! (`api:<actor>:<key>`): the key is the same act for as long as the act's row is kept. The key
//! with another request is refused (422 `singularr.idempotency_mismatch`). Without the header the
//! key is the request's hash and its 5 min bucket: a double click sends once.

use super::pages::labels;
use super::pages::model::Attention;
use super::routes::ApiPath;
use super::state::AppState;
use crate::core::auth::Caller;
use crate::core::error::{AppError, AppResult};
use crate::core::http::Problem;
use crate::core::outbox::{self, Act, Outbox};
use crate::core::schema::ProblemDoc;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use utoipa::{OpenApi, ToSchema};

/// The header that names an act's key.
pub const IDEMPOTENCY_KEY: &str = "idempotency-key";
/// The longest key a caller may give.
const KEY_MAX: usize = 255;
/// How long a failed send stays in the attention list.
const LOOK_DAYS: i64 = 7;
/// Acts in the attention list at most.
const LOOK_MAX: i64 = 50;

/// The key of an act asked for through the API: the caller's header, else the request's hash and
/// its 5 min bucket. A header that is empty, longer than 255 characters or not visible ASCII is
/// refused.
pub fn key_of(headers: &HeaderMap, actor: &str, hash: &str) -> AppResult<String> {
    let Some(given) = headers.get(IDEMPOTENCY_KEY) else {
        return Ok(outbox::bucket_key(actor, hash, Utc::now()));
    };
    let given = given.to_str().map(str::trim).unwrap_or_default();
    if given.is_empty() || given.len() > KEY_MAX || !given.chars().all(|c| c.is_ascii_graphic()) {
        return Err(AppError::bad_request(
            "`Idempotency-Key` is 1 to 255 visible ASCII characters.",
        ));
    }
    Ok(outbox::api_key(actor, given))
}

/// The answer to a key sent with another request than its act's.
pub fn mismatch(act: &Act) -> Response {
    let mut p = Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "singularr.idempotency_mismatch",
        "Key in use",
    );
    p.detail = Some("This Idempotency-Key was sent with another request.".to_string());
    p.with_act(act.id).into_response()
}

/// What an act is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ActSubject {
    /// `title`.
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

/// One act of the outbox.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActView {
    pub id: i64,
    /// The integration it is sent to.
    pub integration: String,
    /// The action: `send`.
    pub action: String,
    /// `pending`, `sending`, `sent`, `unknown`, `confirmed`, `failed` or `cancelled`.
    pub state: String,
    /// Who asked: `user:<address>`, `rule:<id>`.
    pub actor: String,
    pub subject: Option<ActSubject>,
    /// What is sent, secrets left out.
    #[schema(value_type = Object)]
    pub request: serde_json::Value,
    /// What the service answered. A read's finding is under `confirmed`.
    #[schema(value_type = Option<Object>)]
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    /// Why it failed, in a word: `refused`, `access`, `by_hand`.
    pub error_class: Option<String>,
    /// Its sends so far.
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
    /// When its last send went out.
    pub sent_at: Option<DateTime<Utc>>,
    /// When it was confirmed, failed or cancelled.
    pub settled_at: Option<DateTime<Utc>>,
}

impl ActView {
    pub fn of(a: &Act) -> ActView {
        ActView {
            id: a.id,
            integration: a.integration.clone(),
            action: a.action.clone(),
            state: a.state.clone(),
            actor: a.actor.clone(),
            subject: match (&a.subject_type, &a.subject_id) {
                (Some(kind), Some(id)) => Some(ActSubject {
                    kind: kind.clone(),
                    id: id.clone(),
                }),
                _ => None,
            },
            request: a.request.clone(),
            result: a.result.clone(),
            error: a.error.clone(),
            error_class: a.error_class.clone(),
            attempts: a.attempts,
            created_at: a.created_at,
            sent_at: a.sent_at,
            settled_at: a.settled_at,
        }
    }
}

/// The acts that need the owner's look, as items of the attention list. An unconfirmed act is
/// `warn`: its item names the act for the control that marks it not sent. A send a person asked
/// for that failed after its dialog closed is `bad` for seven days: it failed after the route's
/// wait.
pub async fn attention(state: &AppState) -> AppResult<Vec<Attention>> {
    let since = Utc::now() - chrono::Duration::days(LOOK_DAYS);
    let acts =
        outbox::store::to_look_at(&state.pool, super::processing::SEND_WAIT, since, LOOK_MAX)
            .await?;
    let outbox = state.outbox();
    Ok(acts.iter().map(|a| item(&outbox, a)).collect())
}

/// One act as an item of the attention list: its title's name with the episode, else its file's.
fn item(outbox: &Outbox, act: &Act) -> Attention {
    let words = outbox.words(act);
    let text = |key: &str| words.get(key).and_then(Value::as_str);
    let what = text("flow_label")
        .or_else(|| text("flow"))
        .unwrap_or(act.action.as_str())
        .to_string();
    let episode = act
        .asked("path")
        .and_then(crate::processing::service::episode_of)
        .filter(|_| text("kind") == Some("show"));
    let title = match (text("title"), episode) {
        (Some(t), Some((season, episode))) => format!("{t} S{season:02} E{episode:02}"),
        (Some(t), None) => t.to_string(),
        (None, _) => text("file").unwrap_or(act.integration.as_str()).to_string(),
    };
    let page = text("kind")
        .and_then(|kind| labels::title_id(kind, words.get("tmdb_id").and_then(Value::as_i64)));
    let unknown = act.state == outbox::UNKNOWN;
    Attention {
        id: format!("act-{}", act.id),
        kind: "act".to_string(),
        title,
        detail: if unknown {
            format!("Unconfirmed: {what}")
        } else {
            format!("Failed: {}", act.error.as_deref().unwrap_or(what.as_str()))
        },
        tone: if unknown { "warn" } else { "bad" }.to_string(),
        since: if unknown {
            act.sent_at.or(Some(act.created_at))
        } else {
            act.settled_at
        },
        act: Some(act.id),
        page,
    }
}

fn owner_only(caller: &Caller) -> AppResult<()> {
    if matches!(caller, Caller::Owner { .. }) {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "Only the owner can read or settle acts.".to_string(),
        ))
    }
}

async fn found(state: &AppState, id: i64) -> AppResult<Act> {
    outbox::act(&state.pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no act {id}.")))
}

/// An act that is not in the state a hand on it needs.
fn not_now(act: &Act, title: &str) -> Response {
    let mut p = Problem::new(StatusCode::CONFLICT, "singularr.conflict", title);
    p.detail = Some(format!("Act {} is {}.", act.id, act.state));
    p.with_act(act.id).into_response()
}

/// One act as it stands: what was asked, of which integration, its state, what the service
/// answered and what a read found. The owner alone.
#[utoipa::path(get, path = "/api/v1/acts/{id}", tag = "acts",
    params(("id" = i64, Path, description = "The act's id")),
    responses((status = 200, body = ActView), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn acts_one(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<Json<ActView>> {
    owner_only(&caller)?;
    Ok(Json(ActView::of(&found(&state, id).await?)))
}

/// Cancel a pending act before a worker takes it. Nothing is sent. The owner alone.
#[utoipa::path(post, path = "/api/v1/acts/{id}/cancel", tag = "acts",
    params(("id" = i64, Path, description = "The act's id")),
    responses((status = 200, body = ActView), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc),
        (status = 409, description = "`singularr.conflict`: the act is no longer pending", body = ProblemDoc),
        (status = 401, body = ProblemDoc)))]
async fn acts_cancel(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<Response> {
    owner_only(&caller)?;
    let act = found(&state, id).await?;
    match state.outbox().cancel(id, &caller.actor()).await? {
        Some(cancelled) => Ok(Json(ActView::of(&cancelled)).into_response()),
        None => Ok(not_now(&act, "Not pending")),
    }
}

/// Settle an unknown act as not sent: the owner's word that the service did not take it. It
/// fails. Its file is free for the rules and for a new send. The owner alone.
#[utoipa::path(post, path = "/api/v1/acts/{id}/not-sent", tag = "acts",
    params(("id" = i64, Path, description = "The act's id")),
    responses((status = 200, body = ActView), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc),
        (status = 409, description = "`singularr.conflict`: the act is still sending, was sent or is settled", body = ProblemDoc),
        (status = 401, body = ProblemDoc)))]
async fn acts_not_sent(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<Response> {
    owner_only(&caller)?;
    let act = found(&state, id).await?;
    match state.outbox().not_sent(id, &caller.actor()).await? {
        Some(settled) => Ok(Json(ActView::of(&settled)).into_response()),
        None => {
            // The act as it stands now: it may have moved since it was read.
            let now = found(&state, id).await.unwrap_or(act);
            let title = match now.state.as_str() {
                outbox::PENDING | outbox::SENDING => "Still sending",
                outbox::SENT | outbox::CONFIRMED => "Already sent",
                _ => "Already settled",
            };
            Ok(not_now(&now, title))
        }
    }
}

/// The acts routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/acts/{id}", get(acts_one))
        .route("/acts/{id}/cancel", post(acts_cancel))
        .route("/acts/{id}/not-sent", post(acts_not_sent))
}

#[derive(OpenApi)]
#[openapi(
    paths(acts_one, acts_cancel, acts_not_sent),
    tags((name = "acts", description = "What Singularr asked a service to do: each act as it was sent and settled"))
)]
struct ActsDoc;

/// The acts routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    ActsDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn an_act_s_key_is_the_caller_s_or_the_request_s() {
        let mut headers = HeaderMap::new();
        let made = key_of(&headers, "user:o@x", "abc").unwrap();
        assert!(made.starts_with("api:user:o@x:abc:"), "{made}");
        headers.insert(IDEMPOTENCY_KEY, HeaderValue::from_static("click-1"));
        assert_eq!(
            key_of(&headers, "user:o@x", "abc").unwrap(),
            "api:user:o@x:click-1"
        );
        let long = "k".repeat(256);
        for bad in ["", "two words", long.as_str()] {
            headers.insert(IDEMPOTENCY_KEY, HeaderValue::from_str(bad).unwrap());
            assert!(key_of(&headers, "user:o@x", "abc").is_err(), "{bad:?}");
        }
    }
}
