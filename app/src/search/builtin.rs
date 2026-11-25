//! Built-in search: PostgreSQL over the module's copy of the documents. Words match as
//! prefixes (full text, `simple` configuration), the whole phrase anywhere in the words, and,
//! with the trigram extension, similar words (a typo). Ranked by an exact title, a title that
//! starts with the words, the full-text rank and the similarity.

use super::model::SearchDoc;
use super::store;
use super::{Hit, SearchError, SearchProvider};
use async_trait::async_trait;
use sqlx::PgPool;
use tokio::sync::OnceCell;

pub struct Builtin {
    pool: PgPool,
    trigram: OnceCell<bool>,
}

/// The query's words as full-text prefixes: each run of letters and digits, `word:*`, joined
/// with `&`. Nothing else reaches `to_tsquery`.
pub fn tsquery(query: &str) -> String {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("{}:*", w.to_lowercase()))
        .collect::<Vec<_>>()
        .join(" & ")
}

/// A LIKE pattern's wildcards and its escape character taken literally.
pub fn like_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl Builtin {
    pub fn new(pool: PgPool) -> Builtin {
        Builtin {
            pool,
            trigram: OnceCell::new(),
        }
    }

    /// Whether the database has the trigram extension; asked once.
    pub async fn trigram(&self) -> bool {
        *self
            .trigram
            .get_or_init(|| async { store::has_trigram(&self.pool).await.unwrap_or(false) })
            .await
    }
}

#[async_trait]
impl SearchProvider for Builtin {
    fn id(&self) -> &'static str {
        super::BUILTIN
    }

    async fn replace(&self, docs: &[SearchDoc]) -> Result<u64, SearchError> {
        Ok(store::replace(&self.pool, docs).await?)
    }

    async fn search(
        &self,
        query: &str,
        kinds: &[String],
        limit: usize,
    ) -> Result<Vec<Hit>, SearchError> {
        let phrase = query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if phrase.is_empty() || kinds.is_empty() {
            return Ok(Vec::new());
        }
        let escaped = like_escape(&phrase);
        let ts = tsquery(&phrase);
        let prefix = format!("{escaped}%");
        let contains = format!("%{escaped}%");
        let q = store::Query {
            phrase: &phrase,
            tsquery: &ts,
            prefix: &prefix,
            contains: &contains,
            kinds,
            limit: i64::try_from(limit).unwrap_or(50),
        };
        let trigram = self.trigram().await;
        Ok(store::search(&self.pool, &q, trigram)
            .await?
            .into_iter()
            .map(|(doc, score)| Hit { doc, score })
            .collect())
    }

    async fn documents(&self) -> Result<i64, SearchError> {
        Ok(store::count(&self.pool).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_letters_and_digits_reach_the_full_text_query() {
        assert_eq!(tsquery("The Matrix"), "the:* & matrix:*");
        assert_eq!(
            tsquery("spider-man: no way"),
            "spider:* & man:* & no:* & way:*"
        );
        assert_eq!(tsquery("Amélie 2001"), "amélie:* & 2001:*");
        assert_eq!(tsquery("'); DROP TABLE x; --"), "drop:* & table:* & x:*");
        assert_eq!(tsquery("!!!"), "");
        assert_eq!(like_escape("100%_a\\b"), "100\\%\\_a\\\\b");
    }
}
