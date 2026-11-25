//! SQL over `policy_versions` and `policy_models`, and the step that makes a loaded policy the
//! active one.

use super::ActivePolicy;
use crate::core::journal::{Journal, event};
use crate::core::model::Event;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

/// One saved version of the model.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ModelRow {
    pub version: i64,
    pub body: serde_json::Value,
    pub hash: String,
    pub action: String,
    pub subject: String,
    pub saved_by: String,
    pub saved_at: DateTime<Utc>,
}

/// A saved version without its body, for the list of versions.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ModelVersionRow {
    pub version: i64,
    pub hash: String,
    pub action: String,
    pub subject: String,
    pub saved_by: String,
    pub saved_at: DateTime<Utc>,
}

/// What a save writes.
pub struct NewModel<'a> {
    pub version: i64,
    pub body: &'a serde_json::Value,
    pub hash: &'a str,
    pub action: &'a str,
    pub subject: &'a str,
    pub saved_by: &'a str,
}

/// The newest saved version, inside a transaction that is about to add the next.
pub async fn latest_model_in(conn: &mut PgConnection) -> Result<Option<ModelRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelRow>(
        "SELECT version, body, hash, action, subject, saved_by, saved_at
         FROM policy_models ORDER BY version DESC LIMIT 1",
    )
    .fetch_optional(conn)
    .await
}

/// The newest saved version.
pub async fn latest_model(pool: &PgPool) -> Result<Option<ModelRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelRow>(
        "SELECT version, body, hash, action, subject, saved_by, saved_at
         FROM policy_models ORDER BY version DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
}

/// One saved version.
pub async fn model_at(pool: &PgPool, version: i64) -> Result<Option<ModelRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelRow>(
        "SELECT version, body, hash, action, subject, saved_by, saved_at
         FROM policy_models WHERE version = $1",
    )
    .bind(version)
    .fetch_optional(pool)
    .await
}

/// The saved versions, newest first.
pub async fn model_versions(
    pool: &PgPool,
    limit: i64,
) -> Result<Vec<ModelVersionRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelVersionRow>(
        "SELECT version, hash, action, subject, saved_by, saved_at
         FROM policy_models ORDER BY version DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Add a version. Rows are never changed afterwards.
pub async fn insert_model(conn: &mut PgConnection, m: &NewModel<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO policy_models (version, body, hash, action, subject, saved_by)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(m.version)
    .bind(m.body)
    .bind(m.hash)
    .bind(m.action)
    .bind(m.subject)
    .bind(m.saved_by)
    .execute(conn)
    .await
    .map(|_| ())
}

/// The hash of the policy that was loaded most recently, if any was.
pub async fn last_loaded_hash(conn: &mut PgConnection) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT hash FROM policy_versions ORDER BY loaded_at DESC LIMIT 1",
    )
    .fetch_optional(conn)
    .await
}

/// Remember this policy text under its hash and stamp it as loaded now.
pub async fn record_version(conn: &mut PgConnection, p: &ActivePolicy) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO policy_versions (hash, name, version, source, text, loaded_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (hash) DO UPDATE
         SET loaded_at = now(), source = EXCLUDED.source, name = EXCLUDED.name, version = EXCLUDED.version",
    )
    .bind(&p.hash)
    .bind(&p.policy.name)
    .bind(&p.policy.version)
    .bind(p.source.label())
    .bind(&p.text)
    .execute(conn)
    .await
    .map(|_| ())
}

pub async fn count_versions(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM policy_versions")
        .fetch_one(pool)
        .await
}

/// Record the policy this run uses. When it differs from the one loaded before, a
/// `policy_loaded` event is appended and returned; later `score_changed` events name it as
/// their cause.
pub async fn activate(
    pool: &PgPool,
    journal: &Journal,
    policy: &ActivePolicy,
) -> Result<Option<Event>, sqlx::Error> {
    let mut tx = journal.begin(pool).await?;
    let previous = last_loaded_hash(tx.conn()).await?;
    record_version(tx.conn(), policy).await?;
    let loaded = if previous.as_deref() == Some(policy.hash.as_str()) {
        None
    } else {
        Some(
            tx.append_one(event(
                "policy_loaded",
                "policy",
                &policy.hash,
                "policy",
                serde_json::json!({
                    "hash": policy.hash,
                    "name": policy.policy.name,
                    "version": policy.policy.version,
                    "source": policy.source.label(),
                    "previous_hash": previous,
                    "traits": policy.policy.traits.len(),
                    "lanes": policy.policy.lanes.len(),
                    "protections": policy.policy.protections.len(),
                }),
            ))
            .await?,
        )
    };
    tx.commit().await?;
    Ok(loaded)
}
