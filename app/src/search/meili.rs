//! Meilisearch as the search provider (API v1.x, `Authorization: Bearer <key>`). Singularr's own
//! service, not an engine: the one host the outbound allowlist lets take POST, PUT and DELETE.
//!
//! One index, `<prefix>library`, primary key `id`, searchable `title`, `originalTitle`,
//! `extra`, `year`, filterable `kind` and `pass`. A full write puts the settings, adds every
//! document stamped with the pass, waits for each task, then deletes the documents of earlier
//! passes (`pass != <now>`, Meilisearch 1.2 or later). A search is one `/multi-search` with
//! one query per kind, typo tolerant and ranked by Meilisearch (`showRankingScore`).

use super::model::SearchDoc;
use super::{Hit, SearchError, SearchProvider};
use crate::core::outbound::{Body, Method, OutRequest, Outbound, encode_component};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use std::time::Duration;

/// Documents sent per call.
const BATCH: usize = 5000;
/// How long a write waits for one of Meilisearch's tasks, polled twice a second.
const TASK_WAIT: Duration = Duration::from_secs(120);
const TASK_POLL: Duration = Duration::from_millis(500);

pub struct Meili {
    base: String,
    key: String,
    index: String,
    out: Outbound,
}

/// Meilisearch's words for a failed call: its `message`, else the status.
fn message(status: u16, body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| format!("status {status}"))
}

/// The filter of one kind. Kinds are Singularr's own words, never the caller's text.
fn kind_filter(kind: &str) -> String {
    let safe: String = kind.chars().filter(|c| c.is_ascii_lowercase()).collect();
    format!("kind = {safe}")
}

impl Meili {
    pub fn new(base: &str, key: &str, prefix: &str, out: Outbound) -> Meili {
        Meili {
            base: base.trim_end_matches('/').to_string(),
            key: key.to_string(),
            index: format!("{prefix}library"),
            out,
        }
    }

    pub fn index_name(&self) -> &str {
        &self.index
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, SearchError> {
        let url = format!("{}{path}", self.base);
        let body = match body {
            Some(v) => Body::Json(v.to_string()),
            None => Body::None,
        };
        let req = OutRequest {
            method,
            url,
            headers: Vec::new(),
            body,
            timeout: None,
        }
        .header("Authorization", format!("Bearer {}", self.key))
        .header("Accept", "application/json");
        let res = self.out.send(req).await?;
        if !res.ok() {
            return Err(SearchError::Meili(message(res.status, &res.body)));
        }
        if res.body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&res.body).map_err(|e| SearchError::Meili(e.to_string()))
    }

    fn task_uid(answer: &Value) -> Result<i64, SearchError> {
        answer
            .get("taskUid")
            .and_then(Value::as_i64)
            .ok_or_else(|| SearchError::Meili("the answer named no task".to_string()))
    }

    /// Wait until one of Meilisearch's tasks has run; its failure is an error.
    async fn wait(&self, uid: i64) -> Result<(), SearchError> {
        let started = std::time::Instant::now();
        loop {
            let task = self
                .call(Method::Get, &format!("/tasks/{uid}"), None)
                .await?;
            match task.get("status").and_then(Value::as_str) {
                Some("succeeded") => return Ok(()),
                Some("failed") | Some("canceled") => {
                    let why = task
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("the task did not run");
                    return Err(SearchError::Meili(format!("task {uid}: {why}")));
                }
                _ if started.elapsed() > TASK_WAIT => {
                    return Err(SearchError::Meili(format!(
                        "task {uid} did not finish in {} s",
                        TASK_WAIT.as_secs()
                    )));
                }
                _ => tokio::time::sleep(TASK_POLL).await,
            }
        }
    }
}

#[async_trait]
impl SearchProvider for Meili {
    fn id(&self) -> &'static str {
        super::MEILISEARCH
    }

    async fn replace(&self, docs: &[SearchDoc]) -> Result<u64, SearchError> {
        let index = encode_component(&self.index);
        let pass = Utc::now().timestamp_millis();
        // Updating the settings of an index that does not exist yet creates it.
        let mut tasks = vec![
            Self::task_uid(
                &self
                    .call(
                        Method::Put,
                        &format!("/indexes/{index}/settings/searchable-attributes"),
                        Some(json!(["title", "originalTitle", "extra", "year"])),
                    )
                    .await?,
            )?,
            Self::task_uid(
                &self
                    .call(
                        Method::Put,
                        &format!("/indexes/{index}/settings/filterable-attributes"),
                        Some(json!(["kind", "pass"])),
                    )
                    .await?,
            )?,
        ];
        for chunk in docs.chunks(BATCH) {
            let batch: Vec<Value> = chunk
                .iter()
                .map(|d| {
                    let mut v = serde_json::to_value(d).unwrap_or(Value::Null);
                    if let Some(o) = v.as_object_mut() {
                        o.insert("pass".to_string(), json!(pass));
                    }
                    v
                })
                .collect();
            tasks.push(Self::task_uid(
                &self
                    .call(
                        Method::Post,
                        &format!("/indexes/{index}/documents?primaryKey=id"),
                        Some(Value::Array(batch)),
                    )
                    .await?,
            )?);
        }
        // Every write must have run before the old documents go, or a failed batch would leave
        // the index empty.
        for uid in tasks {
            self.wait(uid).await?;
        }
        let gone = Self::task_uid(
            &self
                .call(
                    Method::Post,
                    &format!("/indexes/{index}/documents/delete"),
                    Some(json!({ "filter": format!("pass != {pass}") })),
                )
                .await?,
        )?;
        self.wait(gone).await?;
        Ok(u64::try_from(self.documents().await?).unwrap_or(0))
    }

    async fn search(
        &self,
        query: &str,
        kinds: &[String],
        limit: usize,
    ) -> Result<Vec<Hit>, SearchError> {
        if kinds.is_empty() || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let queries: Vec<Value> = kinds
            .iter()
            .map(|k| {
                json!({
                    "indexUid": self.index,
                    "q": query.trim(),
                    "filter": kind_filter(k),
                    "limit": limit,
                    "showRankingScore": true,
                })
            })
            .collect();
        let answer = self
            .call(
                Method::Post,
                "/multi-search",
                Some(json!({ "queries": queries })),
            )
            .await?;
        let mut hits = Vec::new();
        for result in answer
            .get("results")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            for h in result
                .get("hits")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                let score = h
                    .get("_rankingScore")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                if let Ok(doc) = serde_json::from_value::<SearchDoc>(h.clone()) {
                    hits.push(Hit { doc, score });
                }
            }
        }
        Ok(hits)
    }

    async fn documents(&self) -> Result<i64, SearchError> {
        let stats = self
            .call(
                Method::Get,
                &format!("/indexes/{}/stats", encode_component(&self.index)),
                None,
            )
            .await?;
        Ok(stats
            .get("numberOfDocuments")
            .and_then(Value::as_i64)
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::Allowlist;
    use crate::core::outbound::recorded::Recorded;
    use std::sync::Arc;

    const BASE: &str = "http://10.0.0.9:7700";

    fn meili(recorded: Arc<Recorded>) -> Meili {
        let mut allow = Allowlist::default();
        allow.allow_service("meilisearch", BASE).unwrap();
        Meili::new(
            BASE,
            "search-key",
            "singularr_",
            Outbound::with_transport(allow, recorded),
        )
    }

    fn doc(id: &str, kind: &str, title: &str) -> SearchDoc {
        SearchDoc {
            id: id.to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            original_title: None,
            year: Some(1999),
            extra: Vec::new(),
            poster: None,
            rating: None,
            parent: None,
            artist: None,
        }
    }

    #[tokio::test]
    async fn a_full_write_waits_for_every_task_before_the_old_documents_go() {
        let recorded = Recorded::new();
        let at = |p: &str| format!("{BASE}{p}");
        for (method, path, uid) in [
            (
                Method::Put,
                "/indexes/singularr_library/settings/searchable-attributes",
                1,
            ),
            (
                Method::Put,
                "/indexes/singularr_library/settings/filterable-attributes",
                2,
            ),
            (
                Method::Post,
                "/indexes/singularr_library/documents?primaryKey=id",
                3,
            ),
            (
                Method::Post,
                "/indexes/singularr_library/documents/delete",
                4,
            ),
        ] {
            recorded.answer(method, &at(path), 202, &format!("{{\"taskUid\": {uid}}}"));
        }
        recorded.answer(
            Method::Get,
            &at("/tasks/"),
            200,
            "{\"status\": \"succeeded\"}",
        );
        recorded.answer(
            Method::Get,
            &at("/indexes/singularr_library/stats"),
            200,
            "{\"numberOfDocuments\": 2}",
        );
        let m = meili(recorded.clone());
        let n = m
            .replace(&[
                doc("movie-603", "movie", "The Matrix"),
                doc("album-1", "album", "Kind of Blue"),
            ])
            .await
            .unwrap();
        assert_eq!(n, 2);
        let seen = recorded.seen.lock().unwrap().clone();
        let delete_at = seen
            .iter()
            .position(|(_, u)| u.ends_with("/documents/delete"))
            .unwrap();
        let waits_before = seen[..delete_at]
            .iter()
            .filter(|(m, u)| *m == Method::Get && u.contains("/tasks/"))
            .count();
        assert_eq!(waits_before, 3, "every write is waited for first: {seen:?}");
    }

    #[tokio::test]
    async fn a_failed_task_stops_the_write_before_anything_is_deleted() {
        let recorded = Recorded::new();
        let at = |p: &str| format!("{BASE}{p}");
        recorded.answer(
            Method::Put,
            &at("/indexes/singularr_library/settings/"),
            202,
            "{\"taskUid\": 1}",
        );
        recorded.answer(
            Method::Post,
            &at("/indexes/singularr_library/documents?primaryKey=id"),
            202,
            "{\"taskUid\": 3}",
        );
        recorded.answer(
            Method::Get,
            &at("/tasks/"),
            200,
            "{\"status\": \"failed\", \"error\": {\"message\": \"invalid document\"}}",
        );
        let m = meili(recorded.clone());
        let err = m
            .replace(&[doc("movie-603", "movie", "The Matrix")])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid document"), "{err}");
        assert!(
            !recorded
                .urls_seen()
                .iter()
                .any(|u| u.ends_with("/documents/delete")),
            "nothing is deleted after a failed write"
        );
    }

    #[tokio::test]
    async fn a_search_asks_one_query_per_kind_and_reads_the_hits() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Post,
            &format!("{BASE}/multi-search"),
            200,
            r#"{"results": [
                {"indexUid": "singularr_library", "hits": [
                    {"id": "movie-603", "kind": "movie", "title": "The Matrix", "year": 1999, "extra": [], "pass": 5, "_rankingScore": 0.97}
                ]},
                {"indexUid": "singularr_library", "hits": []}
            ]}"#,
        );
        let m = meili(recorded.clone());
        let hits = m
            .search("matirx", &["movie".to_string(), "show".to_string()], 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc.id, "movie-603");
        assert!((hits[0].score - 0.97).abs() < 1e-9);
        assert_eq!(kind_filter("movie\" OR 1=1"), "kind = movie");
        // The key travels in the header, never in an address.
        assert!(
            recorded
                .urls_seen()
                .iter()
                .all(|u| !u.contains("search-key"))
        );
        let err = meili(Recorded::new())
            .search("x", &["movie".to_string()], 5)
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::Outbound(_)), "{err}");
    }
}
