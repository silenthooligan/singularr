//! The processing send as an act of the outbox (platform design, section 4.7): one file handed to
//! one flow of a processing service. The send by hand and the rules' sends enqueue it. A worker
//! of the `acts` queue sends it once. A read confirms it: the job by id when the bridge named
//! one, else the run the send could have opened ([`store::run_of_send`]). It is never sent again
//! on its own (`Resend::Never`). Each processing pass confirms the open sends its service's runs
//! answer for ([`confirm_by_runs`]).
//!
//! A rule's claim of a file and its act are one thing: the claim names its act. Each change of
//! the act settles the claim ([`Sends::changed`]). A confirmed act is sent. A clear no frees the
//! file. An unknown act keeps it until a read finds the run or the owner settles it as not sent.
//!
//! Every process delivers the sends: an act enqueued before the sync was turned off still settles
//! its claim. No send goes out while the sync is off ([`NO_SYNC`]): it fails before anything is
//! sent. Its claim is held.

use super::model::{file_name, run_id};
use super::service::{SendOutcome, episode_of, send, store_runs};
use super::store;
use crate::catalog::store::TitleRef;
use crate::core::journal::{Journal, JournalTx, event};
use crate::core::outbound::OutboundError;
use crate::core::outbox::{
    ABSENT, ACCESS, Act, BY_HAND, CANCELLED, CONFIRMED, ConfirmBy, Confirmation, Deliver,
    Delivered, FAILED, Held, NewAct, Outbox, Plan, Resend, UNAVAILABLE, UNKNOWN,
};
use crate::engines::service::EngineSet;
use crate::engines::{EngineError, Refusal};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

/// The action of a send, as the bridge's adapter declares it.
pub const ACTION: &str = "send";
/// The first read of a send: at once. The bridge names its job in its answer.
pub const FIRST_READ: Duration = Duration::ZERO;
/// The reads after it: 30 s on, then twice as long each time. Six reads take 15.5 min.
pub const READ_EVERY: Duration = Duration::from_secs(30);
/// No read later than a day after the send: a run of the service answers for a send that long
/// ([`store::CLAIM_RUN_WITHIN_SECS`]).
pub const READ_WINDOW: Duration = Duration::from_secs(86_400);
/// The detail of a send that got no clear answer: the service may have taken the file.
pub const NO_CLEAR_ANSWER: &str = "No clear answer";
/// The class of a failed send whose service takes no file on demand.
pub const NOT_OFFERED: &str = "not_offered";
/// The class of a failed send that erred before the service could take the file.
pub const ERROR: &str = "error";
/// The class of a send held back while the integrations' sync is off: no service is asked.
pub const NO_SYNC: &str = "sync_off";
/// Why a send is held back while the sync is off.
pub const SYNC_IS_OFF: &str = "Integration sync is off";

/// How a send is confirmed: by reads, never sent again on its own.
pub const PLAN: Plan = Plan {
    confirm: ConfirmBy::Read {
        first: FIRST_READ,
        every: READ_EVERY,
        window: READ_WINDOW,
    },
    resend: Resend::Never,
};

/// What a send asks: the file, the flow and the title the file belongs to.
pub fn request(title: &TitleRef, path: &str, flow: &str) -> Value {
    json!({
        "title": {"id": title.id, "kind": title.kind, "tmdb_id": title.tmdb_id,
                  "title": title.title, "year": title.year},
        "path": path,
        "flow": flow,
    })
}

/// The open key of a send: one act of a file and a flow of a service whose fate is not known.
pub fn open_key(service: &str, path: &str, flow: &str) -> String {
    format!("send:{service}:{path}:{flow}")
}

/// A send the owner asked for, under `key` (`outbox::api_key`, `outbox::bucket_key`).
pub fn by_hand(
    service: &str,
    title: &TitleRef,
    path: &str,
    flow: &str,
    actor: &str,
    key: String,
) -> NewAct {
    NewAct {
        integration: service.to_string(),
        action: ACTION.to_string(),
        key,
        open_key: Some(open_key(service, path, flow)),
        request: request(title, path, flow),
        subject: Some(("title".to_string(), title.id.to_string())),
        actor: actor.to_string(),
        caused_by: None,
    }
}

/// What a rule's send is: its rule, the file it matched and the claim of that file.
#[derive(Debug, Clone, Copy)]
pub struct RuleSend<'a> {
    pub rule: i64,
    pub copy: i64,
    pub claim: i64,
    pub service: &'a str,
    pub title: &'a TitleRef,
    pub path: &'a str,
    pub flow: &'a str,
    /// The import the file came with.
    pub event: Option<i64>,
}

/// A rule's send: `rule:<rule>:<file>:<claim>`. The claim is in the key: a clear no frees the
/// file. The next send of it is a new act of a new claim.
pub fn by_rule(s: RuleSend<'_>) -> NewAct {
    NewAct {
        integration: s.service.to_string(),
        action: ACTION.to_string(),
        key: format!("rule:{}:{}:{}", s.rule, s.copy, s.claim),
        open_key: Some(open_key(s.service, s.path, s.flow)),
        request: request(s.title, s.path, s.flow),
        subject: Some(("title".to_string(), s.title.id.to_string())),
        actor: format!("rule:{}", s.rule),
        caused_by: s.event,
    }
}

/// The word of a refusal, as a failed send's class keeps it.
pub fn refusal_class(why: Refusal) -> &'static str {
    match why {
        Refusal::NotFound => "not_found",
        Refusal::AlreadyStaged => "already_staged",
        Refusal::Refused => "refused",
        Refusal::Unauthorized => "unauthorized",
        Refusal::Failed => "failed",
    }
}

/// The processing sends: the delivery of the `send` action.
pub struct Sends {
    pool: PgPool,
    journal: Journal,
    engines: Arc<EngineSet>,
    /// The integrations' sync runs (`SINGULARR_SYNC_ENABLED`). Off, no send goes out.
    sync: bool,
    plan: Plan,
}

impl Sends {
    pub fn new(pool: PgPool, journal: Journal, engines: Arc<EngineSet>, sync: bool) -> Sends {
        Sends {
            pool,
            journal,
            engines,
            sync,
            plan: PLAN,
        }
    }

    /// Sends read on another plan: tests read sooner.
    pub fn with_plan(mut self, plan: Plan) -> Sends {
        self.plan = plan;
        self
    }
}

/// An outbox that delivers the processing sends. `sync` says whether the integrations' sync runs.
pub fn outbox(pool: &PgPool, journal: &Journal, engines: &Arc<EngineSet>, sync: bool) -> Outbox {
    with(
        Outbox::new(pool.clone(), journal.clone()),
        pool,
        journal,
        engines,
        sync,
    )
}

/// This outbox with the processing sends.
pub fn with(
    outbox: Outbox,
    pool: &PgPool,
    journal: &Journal,
    engines: &Arc<EngineSet>,
    sync: bool,
) -> Outbox {
    outbox.deliver(
        ACTION,
        Arc::new(Sends::new(
            pool.clone(),
            journal.clone(),
            engines.clone(),
            sync,
        )),
    )
}

/// The title a send is about, as its request holds it.
fn title_of(act: &Act) -> Option<&Value> {
    act.request.get("title").filter(|t| t.is_object())
}

/// The run a read found for a send.
pub fn confirmed_run(act: &Act) -> Option<&str> {
    act.result.as_ref()?.get("confirmed")?.get("run")?.as_str()
}

/// What a rule's claim becomes when its act failed: an outcome of the rule's log and why.
pub fn claim_outcome(act: &Act) -> (&'static str, String) {
    let why = act.error.clone().unwrap_or_default();
    match act.error_class.as_deref() {
        Some(NOT_OFFERED) => ("held", "Sending not supported".to_string()),
        Some(ACCESS) => ("held", "Read-only".to_string()),
        Some(UNAVAILABLE) => ("held", "Service not configured".to_string()),
        Some(NO_SYNC) => ("held", SYNC_IS_OFF.to_string()),
        Some("not_found" | "already_staged" | "refused" | "unauthorized" | "failed") => {
            ("refused", why)
        }
        Some(BY_HAND) => ("failed", "Not sent".to_string()),
        Some(ABSENT) => ("failed", "Not done".to_string()),
        _ => ("failed", why),
    }
}

#[async_trait]
impl Deliver for Sends {
    fn plan(&self, _act: &Act) -> Plan {
        self.plan
    }

    fn held(&self, act: &Act) -> Option<Held> {
        if !self.engines.access.allows(&act.integration, &act.action) {
            return Some(Held::new("Read-only", ACCESS));
        }
        (!self.sync).then(|| Held::new(SYNC_IS_OFF, NO_SYNC))
    }

    async fn send(&self, act: &Act) -> Delivered {
        let no = |error: &str, class: &str| Delivered::No {
            error: error.to_string(),
            class: class.to_string(),
            result: None,
        };
        let (Some(path), Some(flow)) = (act.asked("path"), act.asked("flow")) else {
            return no("The act names no file or no flow", ERROR);
        };
        let Some(processor) = self.engines.processor(&act.integration) else {
            return no("Service not configured", UNAVAILABLE);
        };
        match send(processor, path, flow).await {
            Ok(SendOutcome::Taken(handed)) => {
                Delivered::Answered(json!({"mode": handed.mode, "job": handed.job}))
            }
            Ok(SendOutcome::NotOffered) => no("Sending not supported", NOT_OFFERED),
            Ok(SendOutcome::Unanswered) => Delivered::Unknown {
                error: NO_CLEAR_ANSWER.to_string(),
            },
            Ok(SendOutcome::Refused {
                why,
                reason,
                detail,
                job,
            }) => Delivered::No {
                error: detail
                    .clone()
                    .or_else(|| reason.clone())
                    .unwrap_or_else(|| refusal_class(why).to_string()),
                class: refusal_class(why).to_string(),
                result: Some(json!({"reason": reason, "detail": detail, "job": job})),
            },
            // The way out refused the act: the access was taken back while it was on its way.
            Err(EngineError::Outbound(OutboundError::NotAllowed { .. })) => no("Read-only", ACCESS),
            // `send` errs only when the file surely was not handed over.
            Err(e) => no(&e.to_string(), ERROR),
        }
    }

    async fn confirm(&self, act: &Act) -> Result<Confirmation, String> {
        let service = act.integration.as_str();
        // The job the bridge named: asked for by id and stored.
        if let Some(job) = act.answered("job")
            && let Some(processor) = self.engines.processor(service)
        {
            match processor.jobs_by_id(&[job.to_string()]).await {
                Ok(Some(found)) if !found.jobs.is_empty() => {
                    store_runs(&self.pool, service, &found.jobs).await?;
                    return Ok(Confirmation::Done(json!({"run": run_id(service, job)})));
                }
                // Not listed so far or not to be asked by id: the table answers.
                Ok(_) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        match store::run_of_send(&self.pool, act)
            .await
            .map_err(|e| e.to_string())?
        {
            Some(run) => Ok(Confirmation::Done(json!({"run": run}))),
            None => Ok(Confirmation::NotYet),
        }
    }

    fn words(&self, act: &Act) -> Map<String, Value> {
        let mut out = Map::new();
        if let Some(t) = title_of(act) {
            for (key, from) in [
                ("title_id", "id"),
                ("title", "title"),
                ("kind", "kind"),
                ("tmdb_id", "tmdb_id"),
            ] {
                if let Some(v) = t.get(from) {
                    out.insert(key.to_string(), v.clone());
                }
            }
        }
        if let Some(path) = act.asked("path") {
            out.insert("file".to_string(), json!(file_name(path)));
        }
        if let Some(flow) = act.asked("flow") {
            out.insert("flow".to_string(), json!(flow));
            if let Some(p) = self.engines.processor(&act.integration) {
                out.insert("flow_label".to_string(), json!(p.flow_label(flow)));
            }
        }
        out.insert("service".to_string(), json!(act.integration));
        out
    }

    async fn changed(&self, tx: &mut JournalTx, act: &Act) -> Result<(), sqlx::Error> {
        match act.state.as_str() {
            CONFIRMED => {
                let run = confirmed_run(act);
                store::claim_sent(tx.conn(), act.id, run).await?;
                if let Some(sent) = self.sent_event(act, run) {
                    tx.append_one(sent).await?;
                }
            }
            FAILED => {
                let (outcome, detail) = claim_outcome(act);
                store::claim_settled(tx.conn(), act.id, outcome, &detail).await?;
            }
            CANCELLED => store::claim_settled(tx.conn(), act.id, "held", "Cancelled").await?,
            UNKNOWN => {
                let why = act.error.as_deref().unwrap_or(NO_CLEAR_ANSWER);
                store::claim_unclear(tx.conn(), act.id, why).await?
            }
            _ => {}
        }
        Ok(())
    }
}

impl Sends {
    /// The `processing_sent` event of a confirmed send: about the title, by who asked. Its body
    /// names the file, the flow, the mode, the service, the job and the run. `None` for a send of
    /// no title.
    fn sent_event(&self, act: &Act, run: Option<&str>) -> Option<crate::core::journal::NewEvent> {
        let title = title_of(act)?;
        let title_id = title.get("id")?.as_i64()?;
        let path = act.asked("path").unwrap_or_default();
        let flow = act.asked("flow").unwrap_or_default();
        let service = act.integration.as_str();
        let job = act.answered("job").map(str::to_string).or_else(|| {
            run.and_then(|r| r.strip_prefix(&format!("{service}:")).map(str::to_string))
        });
        let mut body = json!({
            "title_id": title_id,
            "title": title.get("title"),
            "kind": title.get("kind"),
            "tmdb_id": title.get("tmdb_id"),
            "file": file_name(path),
            "flow": flow,
            "flow_label": self.engines.processor(service).map(|p| p.flow_label(flow)),
            "mode": act.answered("mode"),
            "service": service,
            "job": job,
            "run": run,
            "act": act.id,
        });
        if title.get("kind").and_then(Value::as_str) == Some("show")
            && let Some((season, episode)) = episode_of(path)
        {
            body["season"] = json!(season);
            body["episodes"] = json!([episode]);
        }
        Some(event(
            "processing_sent",
            "title",
            title_id,
            &act.actor,
            body,
        ))
    }

    /// The journal the sends' own writes go through.
    pub fn journal(&self) -> &Journal {
        &self.journal
    }
}

/// The open sends of a service that its runs answer for: each confirmed with its run. The
/// processing pass calls it after it stored the service's runs. Returns how many.
pub async fn confirm_by_runs(
    outbox: &Outbox,
    pool: &PgPool,
    service: &str,
) -> Result<u64, sqlx::Error> {
    let mut confirmed = 0;
    for (act, run) in store::sends_with_runs(pool, service).await? {
        if outbox.found(act, json!({"run": run})).await?.is_some() {
            confirmed += 1;
        }
    }
    Ok(confirmed)
}
