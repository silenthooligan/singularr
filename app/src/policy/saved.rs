//! The saved model: quality profiles, lists, advanced rules and naming conventions as a person
//! edits them, one version per save in `policy_models`, and where the policy in force comes
//! from (the setting `system.policySource`): the file `SINGULARR_POLICY` names, read-only, or
//! this model, compiled.
//!
//! A save reads the newest version, changes it, compiles it and adds the next version with a
//! `policy_model_saved` event, in one transaction. Nothing is changed in place, so any version
//! can be read back, compared and restored.

use super::store::{self, ModelRow, NewModel};
use super::{ActivePolicy, PolicyCell};
use crate::core::config::{Config, IntegrationType};
use crate::core::journal::{Journal, event};
use crate::core::model::Event;
use crate::settings::Settings;
use chrono::{DateTime, Utc};
use singularr_core::Lane;
use singularr_core::Policy;
use singularr_core::profile::{self, Model, ModelError, TitleKind};
use sqlx::PgPool;

/// The setting that says where the policy in force comes from: `file` or `saved`.
pub const SOURCE: &str = "system.policySource";

/// Where the policy in force comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The file `SINGULARR_POLICY` names, read-only.
    File,
    /// The saved model.
    Saved,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::File => "file",
            Origin::Saved => "saved",
        }
    }
}

/// Where the policy in force comes from, as the settings say now.
pub fn origin(settings: &Settings) -> Origin {
    match settings.text(SOURCE).as_deref() {
        Some("saved") => Origin::Saved,
        _ => Origin::File,
    }
}

/// Movies or shows, for a lane: by the type of the integration its managers' profiles belong
/// to, else by its words.
pub fn kind_of(config: &Config) -> impl Fn(&Lane) -> TitleKind + '_ {
    move |lane: &Lane| {
        for p in &lane.arr_profiles {
            let id = p.split_once(':').map_or(p.as_str(), |(id, _)| id);
            let kind = config
                .integration(id)
                .map(|i| i.kind)
                .or_else(|| IntegrationType::parse(id));
            match kind {
                Some(IntegrationType::Radarr) => return TitleKind::Movie,
                Some(IntegrationType::Sonarr) => return TitleKind::Show,
                _ => {}
            }
        }
        profile::guess_kind(lane)
    }
}

/// A policy file read into the model. The naming conventions are kept from `keep`.
pub fn imported(file: &Policy, config: &Config, keep: Option<&Model>) -> Model {
    imported_as(file, config, keep, profile::ImportMode::Tiers)
}

/// A policy file read into the model, its lists gathered as `mode` says (tiers, or one list per
/// distinct points vector). The naming conventions are kept from `keep`.
pub fn imported_as(
    file: &Policy,
    config: &Config,
    keep: Option<&Model>,
    mode: profile::ImportMode,
) -> Model {
    let mut model = profile::import_as(file, kind_of(config), mode);
    if let Some(keep) = keep {
        model.naming = keep.naming.clone();
    }
    model
}

/// One saved version.
#[derive(Debug, Clone)]
pub struct Saved {
    pub version: i64,
    pub model: Model,
    pub hash: String,
    pub action: String,
    pub subject: String,
    pub saved_by: String,
    pub saved_at: DateTime<Utc>,
}

/// Why a change was not saved.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// The change is not valid; the text says why.
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    /// The change conflicts with what is there.
    #[error("{0}")]
    Conflict(String),
    #[error("a saved version cannot be read: {0}")]
    Body(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl From<ModelError> for SaveError {
    fn from(e: ModelError) -> Self {
        SaveError::Invalid(e.to_string())
    }
}

fn saved_of(row: ModelRow) -> Result<Saved, SaveError> {
    let version = row.version;
    let model: Model = serde_json::from_value(row.body)
        .map_err(|e| SaveError::Body(format!("version {version}: {e}")))?;
    Ok(Saved {
        version,
        model,
        hash: row.hash,
        action: row.action,
        subject: row.subject,
        saved_by: row.saved_by,
        saved_at: row.saved_at,
    })
}

/// The newest saved version.
pub async fn latest(pool: &PgPool) -> Result<Option<Saved>, SaveError> {
    store::latest_model(pool).await?.map(saved_of).transpose()
}

/// One saved version.
pub async fn at(pool: &PgPool, version: i64) -> Result<Option<Saved>, SaveError> {
    store::model_at(pool, version)
        .await?
        .map(saved_of)
        .transpose()
}

/// What a save is: what it did (`profile_saved`) and to what (`profile:movies-hd`).
#[derive(Debug, Clone, Copy)]
pub struct Change<'a> {
    pub action: &'a str,
    pub subject: &'a str,
    pub actor: &'a str,
}

/// Change the saved model: the newest version, changed by `edit`, filled, compiled and saved
/// as the next version with its event, in one transaction. With no version yet, `seed` gives
/// the model to start from. Returns what `edit` returned, the new version and its policy.
pub async fn change<T>(
    pool: &PgPool,
    journal: &Journal,
    what: Change<'_>,
    seed: impl FnOnce() -> Model,
    edit: impl FnOnce(&mut Model) -> Result<T, SaveError>,
) -> Result<(T, Saved, ActivePolicy), SaveError> {
    let mut tx = journal.begin(pool).await?;
    let previous = match store::latest_model_in(tx.conn()).await? {
        Some(row) => Some(saved_of(row)?),
        None => None,
    };
    let mut model = match &previous {
        Some(p) => p.model.clone(),
        None => seed(),
    };
    let out = edit(&mut model)?;
    model.fill();
    let version = previous.as_ref().map_or(0, |p| p.version) + 1;
    let active = ActivePolicy::from_model(&model, version)?;
    let body = serde_json::to_value(&model).map_err(|e| SaveError::Body(e.to_string()))?;
    store::insert_model(
        tx.conn(),
        &NewModel {
            version,
            body: &body,
            hash: &active.hash,
            action: what.action,
            subject: what.subject,
            saved_by: what.actor,
        },
    )
    .await?;
    tx.append_one(event(
        "policy_model_saved",
        "policy",
        version,
        what.actor,
        serde_json::json!({
            "version": version,
            "previous": previous.as_ref().map(|p| p.version),
            "action": what.action,
            "subject": what.subject,
            "hash": active.hash,
            "profiles": model.profiles.len(),
            "lists": model.lists.len(),
            "rules": model.rules.len(),
        }),
    ))
    .await?;
    tx.commit().await?;
    let saved = Saved {
        version,
        model,
        hash: active.hash.clone(),
        action: what.action.to_string(),
        subject: what.subject.to_string(),
        saved_by: what.actor.to_string(),
        saved_at: Utc::now(),
    };
    Ok((out, saved, active))
}

/// Put a policy in force: remember it (with a `policy_loaded` event when it differs from the
/// one before) and swap it in. Returns the event.
pub async fn put_in_force(
    pool: &PgPool,
    journal: &Journal,
    cell: &PolicyCell,
    active: ActivePolicy,
) -> Result<Option<Event>, sqlx::Error> {
    let loaded = super::store::activate(pool, journal, &active).await?;
    cell.replace(active);
    Ok(loaded)
}

/// The policy a start puts in force: the file's, or the saved model's. A saved source with no
/// version yet starts from the file, imported and saved as the first version.
pub async fn starting_policy(
    pool: &PgPool,
    journal: &Journal,
    settings: &Settings,
    config: &Config,
    file: ActivePolicy,
) -> Result<ActivePolicy, String> {
    if origin(settings) == Origin::File {
        return Ok(file);
    }
    match latest(pool).await {
        Ok(Some(s)) => match ActivePolicy::from_model(&s.model, s.version) {
            Ok(active) => Ok(active),
            Err(e) => {
                tracing::error!(version = s.version, error = %e, "the saved model does not compile; the policy file is in force");
                Ok(file)
            }
        },
        Ok(None) => {
            let model = imported(&file.policy, config, None);
            let what = Change {
                action: "seeded",
                subject: "policy",
                actor: "singularr",
            };
            let (_, saved, active) = change(pool, journal, what, || model, |_| Ok(()))
                .await
                .map_err(|e| {
                    format!("the policy file could not be saved as the first model version: {e}")
                })?;
            tracing::info!(
                version = saved.version,
                "the policy file was saved as the first model version"
            );
            Ok(active)
        }
        Err(e) => Err(format!("the saved model could not be read: {e}")),
    }
}

/// Save an earlier version again as the newest: the undo of every change after it.
pub async fn restore(
    pool: &PgPool,
    journal: &Journal,
    version: i64,
    actor: &str,
) -> Result<(Saved, ActivePolicy), SaveError> {
    let old = at(pool, version)
        .await?
        .ok_or_else(|| SaveError::NotFound(format!("There is no saved version {version}.")))?;
    let subject = format!("version:{version}");
    let what = Change {
        action: "restored",
        subject: &subject,
        actor,
    };
    let ((), saved, active) = change(
        pool,
        journal,
        what,
        || old.model.clone(),
        |m| {
            *m = old.model.clone();
            Ok(())
        },
    )
    .await?;
    Ok((saved, active))
}
