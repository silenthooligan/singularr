//! What an engine hands the catalog, and the port through which a file is assessed.
//!
//! The catalog does not parse or score. It asks an [`Assess`] implementation, which the
//! `releases` module provides, so the layer order holds and there is one way to assess a file.

use super::model::TitleKind;
use serde::Serialize;
use sha2::{Digest, Sha256};
use singularr_core::{EditionRead, ParsedRelease, Verdict};

/// What an engine hands the catalog: the SDK's types at their old paths.
pub use singularr_sdk::model::{EngineRef, FileFacts, SeasonFacts, TitleFacts};

/// Who a sync of this engine writes as: `sync:<integration id>`.
pub fn actor(engine: &EngineRef) -> String {
    format!("sync:{}", engine.id)
}

/// The identity shared by every engine that holds the title.
pub fn ext_key(title: &TitleFacts, engine: &str) -> String {
    let id = |v: Option<i64>| v.filter(|n| *n > 0);
    match title.kind {
        TitleKind::Movie => match (id(title.tmdb_id), title.imdb_id.as_deref()) {
            (Some(tmdb), _) => format!("movie:tmdb:{tmdb}"),
            (None, Some(imdb)) if !imdb.is_empty() => format!("movie:imdb:{imdb}"),
            _ => format!("movie:{engine}:{}", title.engine_title_id),
        },
        TitleKind::Show => match id(title.tvdb_id) {
            Some(tvdb) => format!("show:tvdb:{tvdb}"),
            None => format!("show:{engine}:{}", title.engine_title_id),
        },
    }
}

/// Changes when anything the catalog stores about the title changes.
pub fn title_facts_hash(title: &TitleFacts) -> String {
    hash_json(title)
}

pub fn sort_key(title: &TitleFacts) -> String {
    let base = title
        .sort_title
        .as_deref()
        .unwrap_or(&title.title)
        .to_lowercase();
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = base.strip_prefix(article) {
            return rest.to_string();
        }
    }
    base
}

/// Changes when the file, its lane or what its title brings to the assessment changes:
/// everything an assessment depends on, plus everything stored.
pub fn file_facts_hash(file: &FileFacts, title: &TitleContext<'_>) -> String {
    hash_json(&(file, title.lane, title.tags, title.original_language))
}

fn hash_json<T: Serialize>(value: &T) -> String {
    let text = serde_json::to_string(value).unwrap_or_default();
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// What a file's title brings to its assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleContext<'a> {
    /// The engine that holds the file: an arr integration id.
    pub engine: &'a str,
    pub lane: Option<&'a str>,
    pub tags: &'a [String],
    /// The title's original language, as an English name.
    pub original_language: Option<&'a str>,
}

/// What Singularr makes of one file.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    /// The name that was parsed and where it came from.
    pub release_name: String,
    pub name_source: &'static str,
    /// The name as written, parsed as it stands. The shadow comparison uses this.
    pub parsed: ParsedRelease,
    /// The release as held: the arr's record first, the parsed name for the rest. This is
    /// what the verdict scores.
    pub held: ParsedRelease,
    /// `None` when the file has no lane, or the lane is not in the policy.
    pub verdict: Option<Verdict>,
    /// Steps in the lane's quality ladder, stored next to the verdict's rank.
    pub ladder_steps: Option<usize>,
    pub mq_quality: Option<String>,
    pub mq_group: Option<String>,
    pub mq_edition: Option<String>,
    pub mq_score: Option<i32>,
    pub diff_quality: bool,
    pub diff_group: bool,
    pub diff_edition: bool,
    pub diff_score: bool,
    pub protections: Vec<String>,
    /// Custom formats the arr matched for which Singularr matched no trait.
    pub arr_only_formats: Vec<String>,
    /// Scored traits Singularr matched whose custom format the arr did not match.
    pub singularr_only_traits: Vec<String>,
    /// The edition the copy belongs to, read from the release as held and the arr's edition.
    pub edition: EditionRead,
    /// Its place in the edition list's order of precedence, for sorting.
    pub edition_rank: i32,
}

impl Assessment {
    /// The verdict as it is stored and returned: with the ladder's step count.
    pub fn verdict_view(&self) -> Option<super::model::VerdictView> {
        self.verdict
            .clone()
            .map(|verdict| super::model::VerdictView {
                verdict,
                ladder_steps: self.ladder_steps,
            })
    }
}

/// The port the catalog assesses files through.
pub trait Assess: Send + Sync {
    /// Names the policy and the build behind the assessments. A copy stored under another
    /// stamp is assessed again.
    fn stamp(&self) -> String;

    /// The lane an engine's quality profile places a title in, by the policy's lanes.
    fn lane_for(&self, engine: &str, profile: Option<&str>) -> Option<String>;

    /// The key of the default edition, the one a copy with no marker belongs to.
    fn default_edition(&self) -> String;

    fn assess(&self, file: &FileFacts, title: &TitleContext<'_>) -> Assessment;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::model::TitleAbout;

    pub fn title(id: i64, kind: TitleKind) -> TitleFacts {
        TitleFacts {
            engine_title_id: id,
            kind,
            title: "The Matrix".to_string(),
            sort_title: None,
            year: Some(1999),
            tmdb_id: Some(603),
            tvdb_id: None,
            imdb_id: Some("tt0133093".to_string()),
            original_language: Some("English".to_string()),
            poster_url: None,
            status: Some("released".to_string()),
            path: Some("/media/movies/The Matrix (1999) {tmdb-603}".to_string()),
            monitored: true,
            profile: Some("HD-1080p".to_string()),
            lane: Some("movies-hd".to_string()),
            tags: vec![],
            added_at: None,
            file_count: 1,
            size_bytes: 1,
            seasons: vec![],
            episode_count: None,
            total_episode_count: None,
            about: TitleAbout::default(),
            files: None,
        }
    }

    #[test]
    fn titles_are_keyed_by_the_id_the_engines_share() {
        let mut t = title(7, TitleKind::Movie);
        assert_eq!(ext_key(&t, "radarr"), "movie:tmdb:603");
        assert_eq!(ext_key(&t, "radarr-4k"), "movie:tmdb:603");
        t.tmdb_id = Some(0);
        assert_eq!(ext_key(&t, "radarr"), "movie:imdb:tt0133093");
        t.imdb_id = None;
        assert_eq!(ext_key(&t, "radarr"), "movie:radarr:7");
        let mut s = title(3, TitleKind::Show);
        s.tvdb_id = Some(81189);
        assert_eq!(ext_key(&s, "sonarr"), "show:tvdb:81189");
        s.tvdb_id = None;
        assert_eq!(ext_key(&s, "sonarr-4k"), "show:sonarr-4k:3");
    }

    #[test]
    fn the_hash_follows_the_facts_and_articles_do_not_sort() {
        let a = title(7, TitleKind::Movie);
        let mut b = a.clone();
        assert_eq!(title_facts_hash(&a), title_facts_hash(&b));
        b.tags.push("open-matte".to_string());
        assert_ne!(title_facts_hash(&a), title_facts_hash(&b));
        let mut c = a.clone();
        c.about.rating = Some(8.7);
        assert_ne!(
            title_facts_hash(&a),
            title_facts_hash(&c),
            "a new rating is a new fact"
        );
        assert_eq!(sort_key(&a), "matrix");
    }
}
