//! What the API and the web app ask about downloads.

use super::model::{Download, DownloadCounts, DownloadQuery};
use super::store;
use crate::core::error::AppResult;
use crate::core::model::{Page, Paging};
use sqlx::PgPool;

pub async fn downloads(pool: &PgPool, q: &DownloadQuery) -> AppResult<Page<Download>> {
    let paging = Paging::new(q.page, q.per_page);
    let (items, total) = store::list(pool, q, paging).await?;
    Ok(Page::new(items, paging, total))
}

/// Downloads no engine owns, or that sit in a queue with no title behind them.
pub async fn orphans(pool: &PgPool, q: &DownloadQuery) -> AppResult<Page<Download>> {
    let q = DownloadQuery {
        orphan: Some(true),
        ..q.clone()
    };
    downloads(pool, &q).await
}

pub async fn counts(pool: &PgPool) -> AppResult<DownloadCounts> {
    Ok(store::counts(pool).await?)
}
