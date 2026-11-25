//! The configured providers, and one title looked up in all of them.

use super::model::{CompareQuery, CompareResponse, ProviderInfo, ProviderTitle};
use super::tmdb::Tmdb;
use super::tvdb::Tvdb;
use super::tvmaze::Tvmaze;
use super::{MetadataError, MetadataProvider, TitleIds};
use crate::catalog::model::TitleKind;
use crate::core::config::Config;
use crate::core::error::{AppError, AppResult};
use crate::core::outbound::Outbound;
use crate::settings::Locale;
use std::sync::Arc;

pub struct ProviderSet {
    pub providers: Vec<Arc<dyn MetadataProvider>>,
    /// TMDB, the source of the rich records.
    pub tmdb: Arc<Tmdb>,
    /// The language and the region the providers are asked in: `general.language` and
    /// `general.region` of the settings, which hold the same handle, so a save takes effect at
    /// once.
    pub locale: Locale,
}

impl ProviderSet {
    pub fn from_config(config: &Config, out: &Outbound) -> Self {
        let tmdb = Arc::new(Tmdb::new(config.tmdb_token.clone(), out.clone()));
        ProviderSet {
            providers: vec![
                tmdb.clone(),
                Arc::new(Tvdb::new(config.tvdb_key.clone(), out.clone())),
                Arc::new(Tvmaze::new(out.clone())),
            ],
            tmdb,
            locale: Locale::new("en-US", &config.watch_region),
        }
    }

    /// The same set, asked in the language and region of this handle (the settings' own).
    pub fn with_locale(mut self, locale: Locale) -> Self {
        self.locale = locale;
        self
    }

    /// The record service over TMDB and the cache.
    pub fn records<'a>(&'a self, pool: &'a sqlx::PgPool) -> super::records::Records<'a> {
        let locale = self.locale.get();
        super::records::Records {
            tmdb: &self.tmdb,
            pool,
            region: locale.region,
            language: locale.language,
        }
    }

    pub fn list(&self) -> Vec<ProviderInfo> {
        self.providers.iter().map(|p| p.info()).collect()
    }
}

fn blank(provider: &str, status: &str, message: Option<String>) -> ProviderTitle {
    ProviderTitle {
        provider: provider.to_string(),
        status: status.to_string(),
        message,
        provider_id: None,
        title: None,
        year: None,
        seasons: Vec::new(),
    }
}

async fn ask(provider: &dyn MetadataProvider, ids: &TitleIds) -> ProviderTitle {
    let info = provider.info();
    if !info.configured {
        // Not an error: the provider is simply not set up.
        return blank(
            &info.id,
            "unconfigured",
            info.needs.map(|var| format!("{var} is not set")),
        );
    }
    if !info.kinds.iter().any(|k| k == ids.kind.as_str()) {
        return blank(
            &info.id,
            "unsupported",
            Some(format!("{} has no {}s", info.name, ids.kind.as_str())),
        );
    }
    match provider.lookup(ids).await {
        Ok(Some(found)) => ProviderTitle {
            provider: info.id,
            status: "ok".to_string(),
            message: None,
            provider_id: Some(found.provider_id),
            title: Some(found.title),
            year: found.year,
            seasons: found.seasons,
        },
        Ok(None) => blank(
            &info.id,
            "not_found",
            Some(format!("{} has no such title", info.name)),
        ),
        Err(MetadataError::Unsupported(why)) => blank(&info.id, "unsupported", Some(why)),
        Err(e) => blank(&info.id, "error", Some(e.to_string())),
    }
}

/// Where the providers that answered disagree. Specials (season 0) are compared too.
fn differences(providers: &[ProviderTitle]) -> Vec<String> {
    let answered: Vec<&ProviderTitle> = providers.iter().filter(|p| p.status == "ok").collect();
    let mut out = Vec::new();
    let each = |f: &dyn Fn(&ProviderTitle) -> String| -> Vec<(String, String)> {
        answered
            .iter()
            .map(|p| (p.provider.clone(), f(p)))
            .collect()
    };
    let describe = |values: &[(String, String)]| {
        values
            .iter()
            .map(|(p, v)| format!("{p} says {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let disagree = |values: &[(String, String)]| values.windows(2).any(|w| w[0].1 != w[1].1);

    let titles = each(&|p| format!("\"{}\"", p.title.clone().unwrap_or_default()));
    if titles
        .windows(2)
        .any(|w| !w[0].1.eq_ignore_ascii_case(&w[1].1))
    {
        out.push(format!("Title: {}.", describe(&titles)));
    }
    let years = each(&|p| {
        p.year
            .map(|y| y.to_string())
            .unwrap_or_else(|| "no year".to_string())
    });
    if disagree(&years) {
        out.push(format!("Year: {}.", describe(&years)));
    }
    let mut seasons: Vec<i32> = answered
        .iter()
        .flat_map(|p| p.seasons.iter().map(|s| s.season))
        .collect();
    seasons.sort();
    seasons.dedup();
    for season in seasons {
        let of = |p: &ProviderTitle| p.seasons.iter().find(|s| s.season == season).cloned();
        let counts = each(&|p| match of(p) {
            Some(s) => format!("{} episodes", s.episodes),
            None => "no such season".to_string(),
        });
        if disagree(&counts) {
            out.push(format!("Season {season}: {}.", describe(&counts)));
        }
        let dates = each(&|p| {
            of(p)
                .and_then(|s| s.first_air_date)
                .unwrap_or_else(|| "no date".to_string())
        });
        let known: Vec<(String, String)> =
            dates.into_iter().filter(|(_, d)| d != "no date").collect();
        if disagree(&known) {
            out.push(format!(
                "Season {season} first aired: {}.",
                describe(&known)
            ));
        }
    }
    out
}

pub async fn compare(set: &ProviderSet, q: &CompareQuery) -> AppResult<CompareResponse> {
    let kind = match q.kind.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        None => TitleKind::Show,
        Some(k) => TitleKind::parse(k).ok_or_else(|| {
            AppError::bad_request(format!("kind is `movie` or `show`, not `{k}`"))
        })?,
    };
    let imdb = q
        .imdb
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase);
    if let Some(imdb) = &imdb {
        let digits = imdb.strip_prefix("tt").unwrap_or("");
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(AppError::bad_request(format!(
                "imdb is written like tt0903747, not `{imdb}`"
            )));
        }
    }
    let ids = TitleIds {
        kind,
        tmdb: q.tmdb.filter(|n| *n > 0),
        tvdb: q.tvdb.filter(|n| *n > 0),
        imdb,
    };
    if ids.tmdb.is_none() && ids.tvdb.is_none() && ids.imdb.is_none() {
        return Err(AppError::bad_request(
            "`tmdb`, `tvdb` or `imdb` is required.",
        ));
    }
    let providers =
        futures::future::join_all(set.providers.iter().map(|p| ask(p.as_ref(), &ids))).await;
    Ok(CompareResponse {
        kind: kind.as_str().to_string(),
        tmdb: ids.tmdb,
        tvdb: ids.tvdb,
        imdb: ids.imdb,
        differences: differences(&providers),
        providers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method};

    fn set(recorded: Arc<Recorded>, tmdb: bool, tvdb: bool) -> ProviderSet {
        let out = Outbound::with_transport(Allowlist::with_metadata_providers(), recorded);
        let tmdb = Arc::new(Tmdb::new(
            tmdb.then(|| "tmdb-token".to_string()),
            out.clone(),
        ));
        ProviderSet {
            providers: vec![
                tmdb.clone(),
                Arc::new(Tvdb::new(tvdb.then(|| "tvdb-key".to_string()), out.clone())),
                Arc::new(Tvmaze::new(out)),
            ],
            tmdb,
            locale: Locale::default(),
        }
    }

    fn record_all(recorded: &Recorded) {
        recorded.answer(
            Method::Get,
            "api.themoviedb.org/3/find/81189",
            200,
            include_str!("../../tests/fixtures/tmdb_find.json"),
        );
        recorded.answer(
            Method::Get,
            "api.themoviedb.org/3/tv/1396",
            200,
            include_str!("../../tests/fixtures/tmdb_tv.json"),
        );
        recorded.answer(
            Method::Post,
            "api4.thetvdb.com/v4/login",
            200,
            include_str!("../../tests/fixtures/tvdb_login.json"),
        );
        recorded.answer(
            Method::Get,
            "api4.thetvdb.com/v4/series/81189/extended",
            200,
            include_str!("../../tests/fixtures/tvdb_series_extended.json"),
        );
        recorded.answer_with(
            Method::Get,
            "api.tvmaze.com/lookup/shows",
            301,
            "",
            &[("location", "https://api.tvmaze.com/shows/169")],
        );
        recorded.answer(
            Method::Get,
            "api.tvmaze.com/shows/169",
            200,
            include_str!("../../tests/fixtures/tvmaze_show.json"),
        );
    }

    fn query(tvdb: i64) -> CompareQuery {
        CompareQuery {
            kind: Some("show".to_string()),
            tvdb: Some(tvdb),
            ..CompareQuery::default()
        }
    }

    #[tokio::test]
    async fn one_show_is_read_from_three_providers_and_differences_are_named() {
        let recorded = Recorded::new();
        record_all(&recorded);
        let res = compare(&set(recorded.clone(), true, true), &query(81189))
            .await
            .unwrap();
        let by = |id: &str| res.providers.iter().find(|p| p.provider == id).unwrap();
        assert_eq!(by("tmdb").status, "ok");
        assert_eq!(by("tmdb").title.as_deref(), Some("Breaking Bad"));
        assert_eq!(by("tmdb").year, Some(2008));
        assert_eq!(by("tmdb").provider_id.as_deref(), Some("1396"));
        assert_eq!(by("tmdb").seasons.len(), 3);
        assert_eq!(by("tvdb").status, "ok");
        assert_eq!(
            by("tvdb")
                .seasons
                .iter()
                .find(|s| s.season == 1)
                .unwrap()
                .episodes,
            7
        );
        assert_eq!(by("tvmaze").status, "ok");
        assert_eq!(by("tvmaze").provider_id.as_deref(), Some("169"));
        // The fixtures disagree on season 2 and on the specials, on purpose.
        assert!(
            res.differences.iter().any(|d| d.starts_with("Season 2:")),
            "{:?}",
            res.differences
        );
        assert!(
            res.differences.iter().any(|d| d.starts_with("Season 0:")),
            "{:?}",
            res.differences
        );
        assert!(!res.differences.iter().any(|d| d.starts_with("Title")));

        // GET everywhere, except the one TVDB sign-in.
        let posts: Vec<String> = recorded
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| *m != Method::Get)
            .map(|(_, u)| u.clone())
            .collect();
        assert_eq!(posts, vec!["https://api4.thetvdb.com/v4/login".to_string()]);
    }

    #[tokio::test]
    async fn an_unconfigured_provider_is_reported_as_such_and_not_called() {
        let recorded = Recorded::new();
        record_all(&recorded);
        let res = compare(&set(recorded.clone(), false, false), &query(81189))
            .await
            .unwrap();
        let by = |id: &str| res.providers.iter().find(|p| p.provider == id).unwrap();
        assert_eq!(by("tmdb").status, "unconfigured");
        assert_eq!(
            by("tmdb").message.as_deref(),
            Some("SINGULARR_TMDB_TOKEN is not set")
        );
        assert_eq!(by("tvdb").status, "unconfigured");
        assert_eq!(by("tvmaze").status, "ok");
        assert!(
            recorded
                .urls_seen()
                .iter()
                .all(|u| u.contains("api.tvmaze.com"))
        );
        assert!(
            res.differences.is_empty(),
            "one answer has nothing to differ from"
        );
    }

    #[tokio::test]
    async fn a_missing_title_and_a_failing_provider_do_not_fail_the_comparison() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "api.themoviedb.org/3/find/5",
            200,
            "{\"tv_results\":[],\"movie_results\":[]}",
        );
        recorded.answer(
            Method::Post,
            "api4.thetvdb.com/v4/login",
            401,
            "{\"status\":\"failure\"}",
        );
        recorded.answer(Method::Get, "api.tvmaze.com/lookup/shows", 404, "");
        let res = compare(&set(recorded, true, true), &query(5))
            .await
            .unwrap();
        let by = |id: &str| res.providers.iter().find(|p| p.provider == id).unwrap();
        assert_eq!(by("tmdb").status, "not_found");
        assert_eq!(by("tvdb").status, "error");
        assert_eq!(by("tvmaze").status, "not_found");
    }

    #[tokio::test]
    async fn bad_questions_are_refused() {
        let s = set(Recorded::new(), false, false);
        assert!(compare(&s, &CompareQuery::default()).await.is_err());
        let bad_kind = CompareQuery {
            kind: Some("album".into()),
            tvdb: Some(1),
            ..CompareQuery::default()
        };
        assert!(compare(&s, &bad_kind).await.is_err());
        let bad_imdb = CompareQuery {
            imdb: Some("0903747".into()),
            ..CompareQuery::default()
        };
        assert!(compare(&s, &bad_imdb).await.is_err());
        // TVmaze has no movies: reported, not an error.
        let movie = CompareQuery {
            kind: Some("movie".into()),
            imdb: Some("tt0133093".into()),
            ..CompareQuery::default()
        };
        let res = compare(&s, &movie).await.unwrap();
        assert_eq!(
            res.providers
                .iter()
                .find(|p| p.provider == "tvmaze")
                .unwrap()
                .status,
            "unsupported"
        );
        assert_eq!(s.list().len(), 3);
    }
}
