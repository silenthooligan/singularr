//! Start the server: read the configuration, load the policy, connect and migrate the
//! database, build the engines, start the sync loops, serve the API and the web client.

use axum::Router;
use axum::routing::get;
use singularr::api::routes;
use singularr::api::state::{AppState, Inner};
use singularr::core::config::{Config, Face, ProcessEnv, RequestsFaceConfig};
use singularr::core::identity::{self, Edge};
use singularr::core::journal::Journal;
use singularr::core::outbound::{Allowlist, Outbound};
use singularr::core::{db, http, observe};
use singularr::engines::service::{EngineSet, allowlist};
use singularr::homemovies::service::{self as homemovies, HomeMovies};
use singularr::lifecycle::sync::{self, SyncDeps};
use singularr::metadata::service::ProviderSet;
use singularr::music::service::{self as music, MusicLibrary};
use singularr::plays::service::{self as plays, PlaysDeps};
use singularr::policy;
use singularr::processing::service::{self as processing, ProcessingDeps};
use singularr::releases::assess::Assessor;
use singularr::requests::service::Requests;
use singularr::requests::sync::{self as request_sync, RequestSyncDeps};
use singularr::settings::Settings;
use std::net::SocketAddr;
use std::sync::Arc;

pub fn init_logging() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Start the face `SINGULARR_FACE` names: the owner's Singularr (the default) or Singularr Requests.
pub async fn serve() -> Result<(), String> {
    match Face::from_env(&ProcessEnv).map_err(|e| e.to_string())? {
        Face::Owner => serve_owner().await,
        Face::Requests => serve_requests().await,
    }
}

/// Singularr Requests: its own configuration, its own database role, the request routes alone.
/// It runs no migration and starts no loop: the full process owns the schema and the work.
async fn serve_requests() -> Result<(), String> {
    init_logging();
    let config = RequestsFaceConfig::from_env().map_err(|e| e.to_string())?;
    println!("{config}");
    let pool = db::connect_url(&config.database_url, config.db_max_connections)
        .await
        .map_err(|e| {
            format!(
                "the database is not reachable: {e}. Check SINGULARR_REQUESTS_DATABASE_URL (now {}).",
                singularr::core::config::redact_url(&config.database_url)
            )
        })?;
    singularr::requests::store::face_rules(&pool).await.map_err(|e| {
        format!(
            "the face's views cannot be read ({e}). Start the full Singularr once (it applies the migrations) and run deploy/requests-role.sql."
        )
    })?;
    let allow = Allowlist::for_requests_face();
    for (origin, rule) in allow.hosts() {
        tracing::info!(target: "singularr::outbound", origin = %origin, label = %rule.label, read_only = rule.read_only, "outbound allowlist");
    }
    let outbound = Outbound::new(allow, config.http_timeout).map_err(|e| e.to_string())?;
    let addr: SocketAddr = std::env::var("SINGULARR_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_string())
        .parse()
        .map_err(|e| format!("SINGULARR_LISTEN is not an address: {e}"))?;
    let public = config.public_url.clone();
    let app = singularr::face::routes::router(singularr::face::routes::FaceState::new(
        config, pool, outbound,
    ));
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    tracing::info!("Singularr Requests is listening on http://{addr} for {public}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(http::shutdown_signal())
    .await
    .map_err(|e| format!("the server stopped: {e}"))
}

async fn serve_owner() -> Result<(), String> {
    init_logging();
    // /metrics: HTTP requests by route and status from the layer, the rest from the app.
    // The pair installs the recorder, so it comes before anything records a metric.
    let (prometheus_layer, metric_handle) = axum_prometheus::PrometheusMetricLayer::pair();
    let config = Config::from_env().map_err(|e| e.to_string())?;
    // The whole configuration, with every secret replaced.
    println!("{config}");
    if config.edge_token_placeholder {
        // The identity middleware would take the placeholder for the token, which anyone can
        // guess.
        return Err(
            "SINGULARR_EDGE_TOKEN is still the placeholder CHANGEME. Set it to the edge's token (the value the proxy sends as X-Edge-Token), or leave it empty to trust the proxy address alone."
                .to_string(),
        );
    }

    let loaded = policy::load(&config.policy_path).map_err(|e| e.to_string())?;
    if let Some(warning) = &loaded.warning {
        tracing::warn!("{warning}");
    }
    let file_policy = loaded.policy;
    tracing::info!(
        name = %file_policy.policy.name,
        version = %file_policy.policy.version,
        hash = %file_policy.hash,
        source = %file_policy.source.label(),
        "policy file loaded"
    );
    let (editions, editions_warning) =
        policy::load_editions(&config.editions_path).map_err(|e| e.to_string())?;
    if let Some(warning) = &editions_warning {
        tracing::warn!("{warning}");
    }
    let editions = Arc::new(editions);
    tracing::info!(
        editions = editions.list.editions.len(),
        hash = %editions.hash,
        source = %editions.source.label(),
        "edition list loaded"
    );

    let pool = db::connect(&config).await.map_err(|e| {
        format!(
            "the database is not reachable: {e}. Check DATABASE_URL (now {}) and that the database exists.",
            singularr::core::config::redact_url(&config.database_url)
        )
    })?;
    db::migrate(&pool)
        .await
        .map_err(|e| format!("the migrations could not be applied: {e}"))?;
    tracing::info!("database migrations are applied");
    let journal = Journal::new();
    let pruned = singularr::api::state::prune_unconfigured(&pool, &journal, &config)
        .await
        .map_err(|e| format!("the rows of removed integrations could not be removed: {e}"))?;
    if !pruned.is_empty() {
        tracing::warn!(report = ?pruned, "rows of integrations the configuration no longer lists were removed or set aside");
    }
    singularr::api::state::sync_libraries(&pool, &config)
        .await
        .map_err(|e| format!("the libraries could not be written: {e}"))?;
    // Every setting with its source: the environment, the instance file, a value saved in the
    // interface, or the default.
    let instance_text = config
        .instance_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    let settings = Arc::new(Settings::new(
        &config,
        &ProcessEnv,
        instance_text.as_deref(),
    ));
    let saved = settings
        .reload(&pool)
        .await
        .map_err(|e| format!("the saved settings could not be read: {e}"))?;
    tracing::info!(saved, "settings loaded");

    let allow = allowlist(&config).map_err(|e| e.to_string())?;
    for (origin, rule) in allow.hosts() {
        tracing::info!(target: "singularr::outbound", origin = %origin, label = %rule.label, read_only = rule.read_only, reads = rule.reads.len(), acts = rule.acts.len(), gets = rule.gets.len(), "outbound allowlist");
    }
    let outbound = Outbound::new(allow, config.http_timeout).map_err(|e| e.to_string())?;
    let engines = Arc::new(EngineSet::from_config(&config, &outbound));
    // What Singularr may do with each integration: what the configuration fixes is in force
    // already, what the owner saved in the interface is put in force here.
    engines
        .access
        .load(&pool)
        .await
        .map_err(|e| format!("the saved access of the integrations could not be read: {e}"))?;
    for (id, name) in engines.access.acting() {
        tracing::info!(integration = %id, "Singularr may act on {name}");
    }
    let metadata =
        Arc::new(ProviderSet::from_config(&config, &outbound).with_locale(settings.locale()));
    let search = Arc::new(singularr::search::Search::new(
        pool.clone(),
        &config,
        &outbound,
    ));
    tracing::info!(provider = search.configured(), "search");

    // The policy in force: the file, or the saved model of quality profiles and lists
    // (system.policySource). A save puts a new one in force while the program runs.
    let active =
        policy::saved::starting_policy(&pool, &journal, &settings, &config, file_policy).await?;
    tracing::info!(
        name = %active.policy.name,
        version = %active.policy.version,
        hash = %active.hash,
        source = %active.source.label(),
        "policy in force"
    );
    let active = policy::PolicyCell::handle(active);
    let policy_event = policy::store::activate(&pool, &journal, &active.current())
        .await
        .map_err(|e| format!("the policy could not be recorded: {e}"))?;

    let owners = identity::Allowlist::parse(&config.owner_emails);
    // No proxy is trusted until one is listed, and a fresh install says so here.
    let edge = Edge::from_env(&ProcessEnv);
    if edge.trusted().is_empty() {
        tracing::warn!(
            "SINGULARR_TRUSTED_PROXY_IPS lists no proxy: no identity header is trusted, so nobody signs in through a proxy until it names the proxy's address"
        );
    } else {
        tracing::info!("identity headers are trusted from {:?}", edge.trusted());
        if !edge.token_configured() {
            tracing::warn!(
                "SINGULARR_EDGE_TOKEN is not set: identity headers are trusted on the peer address alone"
            );
        }
    }

    for engine in &engines.slots {
        observe::engine_up(&engine.id, false);
    }
    // The gauges at /metrics are refreshed from the tables whether or not the sync runs.
    let gauges = sync::spawn_gauges(pool.clone());
    let sync = if config.sync.enabled {
        let assess = Arc::new(Assessor::live(
            active.clone(),
            editions.clone(),
            env!("SINGULARR_BUILD_ID"),
        ));
        Some(sync::spawn(SyncDeps {
            config: config.sync.clone(),
            pool: pool.clone(),
            journal: journal.clone(),
            engines: engines.clone(),
            assess,
            policy_event: policy_event.map(|e| e.id),
        }))
    } else {
        tracing::warn!(
            "SINGULARR_SYNC_ENABLED is off: the catalog is served as it is and no engine is asked"
        );
        None
    };

    // The Seerr integration is an engine: its loop runs only with the sync. The two folders are
    // read-only mounts, not engines, and are scanned either way.
    let request_loops = if config.sync.enabled {
        request_sync::spawn(RequestSyncDeps {
            pool: pool.clone(),
            journal: journal.clone(),
            engines: engines.clone(),
            interval: config.sync.requests,
            jitter: config.sync.jitter,
            rules: Requests::new(&config.request_rules).rules,
        })
    } else {
        Vec::new()
    };
    // The processing services are engines too: one loop each, with the sync.
    let processing_loops = if config.sync.enabled {
        processing::spawn(ProcessingDeps {
            pool: pool.clone(),
            journal: journal.clone(),
            engines: engines.clone(),
            interval: config.sync.processing,
            jitter: config.sync.jitter,
            settings: settings.clone(),
            policy: active.clone(),
        })
    } else {
        Vec::new()
    };
    // Plays: the media server's sessions and history, and Tautulli, with the sync.
    let play_loops = if config.sync.enabled {
        plays::spawn(PlaysDeps::new(
            pool.clone(),
            journal.clone(),
            engines.clone(),
            settings.clone(),
            config.sync.jitter,
        ))
    } else {
        Vec::new()
    };
    // Singularr Requests: what it states about the rules, and the loop that records its intents
    // (in R1 as shadow: nothing reaches Seerr). Both run whether or not the sync does.
    let rules = Requests::new(&config.request_rules).rules;
    singularr::requests::store::write_face_settings(
        &pool,
        i32::try_from(rules.max_seasons).unwrap_or(i32::MAX),
        &rules.gated_sites(),
        settings.text("general.url").as_deref(),
    )
    .await
    .map_err(|e| format!("the request face's settings could not be written: {e}"))?;
    let intents = singularr::requests::intents::spawn(pool.clone(), journal.clone(), rules);
    let music_library = Arc::new(MusicLibrary::new(&config.music.root));
    let home_movies = Arc::new(HomeMovies::new(&config.home_movies.root));
    let scans = [
        sync::spawn_folder_scans(
            pool.clone(),
            journal.clone(),
            Arc::new(editions.list.clone()),
            config.movie_folders.roots.clone(),
            config.movie_folders.interval,
            config.sync.jitter,
        ),
        music::spawn_scans(
            pool.clone(),
            journal.clone(),
            music_library.clone(),
            config.music.interval,
            config.sync.jitter,
        ),
        homemovies::spawn_scans(
            pool.clone(),
            journal.clone(),
            home_movies.clone(),
            config.home_movies.interval,
            config.sync.jitter,
        ),
    ];

    // The search index: in full shortly after the start, then after changes. It reads
    // Singularr's own tables, so it runs whether or not the sync does.
    let indexer = singularr::search::index::spawn(singularr::search::index::IndexDeps {
        pool: pool.clone(),
        journal: journal.clone(),
        search: search.clone(),
        metadata: metadata.clone(),
        interval: config.search.interval,
    });

    // Notifications: every channel's address into the way out, then the dispatcher, which
    // follows the journal whether or not the sync runs. Mail leaves through core::smtp.
    let notify = singularr::notify::service::Notifier::new(
        pool.clone(),
        journal.clone(),
        outbound.clone(),
        Arc::new(singularr::core::smtp::Smtp::new(config.http_timeout)),
    );
    let channels = notify
        .load()
        .await
        .map_err(|e| format!("the notification channels could not be read: {e}"))?;
    tracing::info!(channels, "notification channels loaded");
    for (key, listed) in outbound.targets().list() {
        let (host, label) = (listed.host(), listed.label);
        tracing::info!(target: "singularr::outbound", key = %key, host = %host, label = %label, "notification target");
    }
    let dispatcher = notify
        .clone()
        .spawn(Arc::new(singularr::api::notifications::Describer::new(
            pool.clone(),
            &config,
            settings.clone(),
        )));

    let state = AppState::new(Inner {
        notify,
        config: config.clone(),
        pool: pool.clone(),
        journal,
        policy: active,
        engines,
        metadata,
        owners,
        requests: Arc::new(Requests::new(&config.request_rules)),
        music: music_library,
        home_movies,
        settings,
        pages: Default::default(),
        search,
    });

    let addr: SocketAddr = std::env::var("SINGULARR_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_string())
        .parse()
        .map_err(|e| format!("SINGULARR_LISTEN is not an address: {e}"))?;
    // The interface is a client of the API: static files, handed out for every address the
    // API does not answer.
    let web_dir = std::env::var("SINGULARR_WEB_DIR").unwrap_or_else(|_| "/app/web".to_string());
    if !std::path::Path::new(&web_dir).join("index.html").is_file() {
        tracing::warn!(
            dir = %web_dir,
            "the web client is not there (SINGULARR_WEB_DIR holds no index.html): only the API answers"
        );
    }

    let app = Router::new()
        .merge(routes::router(state))
        .merge(http::health_routes(db::ready_check(pool)))
        .route(
            "/metrics",
            get(move || {
                let handle = metric_handle.clone();
                async move { handle.render() }
            }),
        )
        // The request metrics cover the routes above. The client's files are added after the
        // layer, so each file does not become a series of its own.
        .layer(prometheus_layer)
        .fallback_service(singularr::client::router(web_dir))
        // Identity headers from the edge are honored only from the trusted proxy carrying
        // the edge token; from anyone else they are removed before a handler runs.
        .layer(axum::middleware::from_fn_with_state(
            edge,
            identity::strip_untrusted_identity,
        ))
        .layer(axum::middleware::from_fn(http::request_id));

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    tracing::info!("Singularr is listening on http://{addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        http::shutdown_signal().await;
        tracing::info!("shutdown requested: stopping the sync loops");
        if let Some(sync) = &sync {
            sync.stop();
        }
        for task in request_loops
            .iter()
            .chain(processing_loops.iter())
            .chain(play_loops.iter())
            .chain(scans.iter())
        {
            task.abort();
        }
        intents.abort();
        indexer.abort();
        dispatcher.abort();
        gauges.abort();
        // Open event streams would hold the server open; give requests a moment, then go.
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            std::process::exit(0);
        });
    })
    .await
    .map_err(|e| format!("the server stopped: {e}"))
}
