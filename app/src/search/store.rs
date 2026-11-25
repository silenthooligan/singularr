//! SQL over the search module's tables: `search_documents` (the built-in index) and
//! `search_state` (what each provider was last given).

use super::model::SearchDoc;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sqlx::types::Json;

/// Documents written per statement.
const CHUNK: usize = 2000;

/// The database has the trigram extension, so built-in search can tolerate typos.
pub async fn has_trigram(pool: &PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm')")
        .fetch_one(pool)
        .await
}

/// Replace every document with these, in one transaction. Returns the documents written (a
/// second document with an id already written is left out).
pub async fn replace(pool: &PgPool, docs: &[SearchDoc]) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM search_documents")
        .execute(&mut *tx)
        .await?;
    let mut written = 0;
    for chunk in docs.chunks(CHUNK) {
        let ids: Vec<String> = chunk.iter().map(|d| d.id.clone()).collect();
        let kinds: Vec<String> = chunk.iter().map(|d| d.kind.clone()).collect();
        let titles: Vec<String> = chunk.iter().map(|d| d.title.to_lowercase()).collect();
        let words: Vec<String> = chunk.iter().map(SearchDoc::words).collect();
        let bodies: Vec<String> = chunk
            .iter()
            .map(|d| serde_json::to_string(d).unwrap_or_else(|_| "{}".to_string()))
            .collect();
        written += sqlx::query(
            "INSERT INTO search_documents (id, kind, title, words, body)
             SELECT u.id, u.kind, u.title, u.words, u.body::jsonb
             FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[], $5::text[])
                  AS u(id, kind, title, words, body)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&ids)
        .bind(&kinds)
        .bind(&titles)
        .bind(&words)
        .bind(&bodies)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(written)
}

pub async fn count(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM search_documents")
        .fetch_one(pool)
        .await
}

/// What a search asks of the built-in index. `phrase` is the whole query in lower case;
/// `tsquery` the words as prefixes joined with `&` (only letters and digits in each), or
/// empty; `prefix` and `contains` are LIKE patterns of the phrase with its wildcards escaped.
pub struct Query<'a> {
    pub phrase: &'a str,
    pub tsquery: &'a str,
    pub prefix: &'a str,
    pub contains: &'a str,
    pub kinds: &'a [String],
    /// Hits per kind.
    pub limit: i64,
}

/// The best documents per kind, best first within each kind, with their scores. With the
/// trigram extension a document also matches on similar words (a typo), and similarity adds
/// to the score.
pub async fn search(
    pool: &PgPool,
    q: &Query<'_>,
    trigram: bool,
) -> Result<Vec<(SearchDoc, f64)>, sqlx::Error> {
    let query = if trigram {
        sqlx::query_as::<_, (Json<SearchDoc>, f64)>(
            "SELECT r.body, r.score FROM (
                SELECT m.body, m.kind, m.score,
                       row_number() OVER (PARTITION BY m.kind ORDER BY m.score DESC, m.title, m.id) AS n
                FROM (
                    SELECT d.id, d.body, d.kind, d.title,
                           (CASE WHEN d.title = $1 THEN 3.0::float8 ELSE 0.0::float8 END
                            + CASE WHEN d.title LIKE $3 ESCAPE '\\' THEN 1.5::float8 ELSE 0.0::float8 END
                            + CASE WHEN $2 <> '' THEN ts_rank(d.words_tsv, to_tsquery('simple', $2))::float8
                                   ELSE 0.0::float8 END
                            + word_similarity($1, d.words)::float8) AS score
                    FROM search_documents d
                    WHERE d.kind = ANY($5)
                      AND (($2 <> '' AND d.words_tsv @@ to_tsquery('simple', $2))
                           OR d.words LIKE $4 ESCAPE '\\'
                           OR word_similarity($1, d.words) >= 0.5)
                ) m
             ) r
             WHERE r.n <= $6
             ORDER BY r.kind, r.n",
        )
    } else {
        sqlx::query_as::<_, (Json<SearchDoc>, f64)>(
            "SELECT r.body, r.score FROM (
                SELECT m.body, m.kind, m.score,
                       row_number() OVER (PARTITION BY m.kind ORDER BY m.score DESC, m.title, m.id) AS n
                FROM (
                    SELECT d.id, d.body, d.kind, d.title,
                           (CASE WHEN d.title = $1 THEN 3.0::float8 ELSE 0.0::float8 END
                            + CASE WHEN d.title LIKE $3 ESCAPE '\\' THEN 1.5::float8 ELSE 0.0::float8 END
                            + CASE WHEN $2 <> '' THEN ts_rank(d.words_tsv, to_tsquery('simple', $2))::float8
                                   ELSE 0.0::float8 END) AS score
                    FROM search_documents d
                    WHERE d.kind = ANY($5)
                      AND (($2 <> '' AND d.words_tsv @@ to_tsquery('simple', $2))
                           OR d.words LIKE $4 ESCAPE '\\')
                ) m
             ) r
             WHERE r.n <= $6
             ORDER BY r.kind, r.n",
        )
    };
    let rows = query
        .bind(q.phrase)
        .bind(q.tsquery)
        .bind(q.prefix)
        .bind(q.contains)
        .bind(q.kinds)
        .bind(q.limit)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|(Json(d), s)| (d, s)).collect())
}

/// What a provider was last given.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct StateRow {
    pub provider: String,
    pub documents: i64,
    pub last_indexed: Option<DateTime<Utc>>,
    pub state: String,
    pub detail: Option<String>,
}

/// Record how a pass went for a provider. `documents` is `None` when the pass failed (the
/// count stays); `indexed` says the index was written in full now.
pub async fn write_state(
    pool: &PgPool,
    provider: &str,
    documents: Option<i64>,
    indexed: bool,
    state: &str,
    detail: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO search_state (provider, documents, last_indexed, state, detail, updated_at)
         VALUES ($1, COALESCE($2, 0), CASE WHEN $3 THEN now() END, $4, $5, now())
         ON CONFLICT (provider) DO UPDATE SET
             documents = COALESCE($2, search_state.documents),
             last_indexed = CASE WHEN $3 THEN now() ELSE search_state.last_indexed END,
             state = EXCLUDED.state, detail = EXCLUDED.detail, updated_at = now()",
    )
    .bind(provider)
    .bind(documents)
    .bind(indexed)
    .bind(state)
    .bind(detail)
    .execute(pool)
    .await
    .map(|_| ())
}

pub async fn read_state(pool: &PgPool, provider: &str) -> Result<Option<StateRow>, sqlx::Error> {
    sqlx::query_as::<_, StateRow>(
        "SELECT provider, documents, last_indexed, state, detail FROM search_state
         WHERE provider = $1",
    )
    .bind(provider)
    .fetch_optional(pool)
    .await
}
