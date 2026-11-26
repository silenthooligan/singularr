//! The bootstrap: what the process reads before anything else. The database login and its pool,
//! the instance file, who may sign in and through which edge, the time limit of a call and
//! which program this process is (`SINGULARR_FACE`). Variables come from the process
//! environment, or from a table in a test. A value that is empty or still the placeholder
//! `CHANGEME` counts as unset.

use std::collections::BTreeMap;
use std::time::Duration;

/// The value `.env` ships with for every secret.
pub const PLACEHOLDER: &str = "CHANGEME";

/// A source of variables, so tests do not touch the process environment.
pub trait Env {
    fn get(&self, name: &str) -> Option<String>;
    /// The name of every variable, for a check that goes by how names end.
    fn names(&self) -> Vec<String>;
}

pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn names(&self) -> Vec<String> {
        std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .collect()
    }
}

impl Env for BTreeMap<&str, &str> {
    fn get(&self, name: &str) -> Option<String> {
        BTreeMap::get(self, name).map(|v| v.to_string())
    }

    fn names(&self) -> Vec<String> {
        self.keys().map(|name| name.to_string()).collect()
    }
}

/// A set value: trimmed, not empty and not the placeholder.
pub fn set(env: &dyn Env, name: &str) -> Option<String> {
    env.get(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != PLACEHOLDER)
}

/// A number of seconds: `default` while the variable is unset, at least `min`. A value that is
/// no number or below the minimum is a warning.
pub fn seconds(
    env: &dyn Env,
    name: &str,
    default: u64,
    min: u64,
    warnings: &mut Vec<String>,
) -> Duration {
    let value = match set(env, name) {
        None => default,
        Some(raw) => match raw.parse::<u64>() {
            Ok(n) if n >= min => n,
            Ok(n) => {
                warnings.push(format!(
                    "{name}={n} is below the minimum of {min}; using {min}"
                ));
                min
            }
            Err(_) => {
                warnings.push(format!(
                    "{name}={raw:?} is not a number of seconds; using {default}"
                ));
                default
            }
        },
    };
    Duration::from_secs(value)
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set. Set it in .env (see the README, Environment).")]
    Missing(&'static str),
    #[error("{0} is not usable: {1}")]
    Invalid(&'static str, String),
    #[error("the instance file {path} is not usable: {message}")]
    Instance { path: String, message: String },
    #[error(
        "Singularr Requests must not hold the full process's secrets, and these are set in its environment: {0}. Give the singularr-requests service only its own variables."
    )]
    NotForTheFace(String),
}

/// Which program this process is: `SINGULARR_FACE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// Singularr, the workspace: everything. The default.
    Owner,
    /// Singularr Requests, the portal: the request pages alone.
    Requests,
}

impl Face {
    pub fn from_env(env: &dyn Env) -> Result<Face, ConfigError> {
        match set(env, "SINGULARR_FACE")
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("owner") | Some("singularr") => Ok(Face::Owner),
            Some("requests") => Ok(Face::Requests),
            Some(other) => Err(ConfigError::Invalid(
                "SINGULARR_FACE",
                format!("`{other}` is neither `owner` nor `requests`"),
            )),
        }
    }
}

/// The bootstrap of the full process: the database, the instance file, the owners and the
/// edge, the time limit of a call.
#[derive(Clone)]
pub struct Bootstrap {
    pub database_url: String,
    pub db_max_connections: u32,
    /// The instance file, when `SINGULARR_INSTANCE` names one.
    pub instance_path: Option<String>,
    pub owner_emails: String,
    pub edge_token_set: bool,
    /// `SINGULARR_EDGE_TOKEN` still holds the placeholder. The identity middleware would take
    /// the placeholder for the token, so the server refuses to start like this.
    pub edge_token_placeholder: bool,
    /// `SINGULARR_TRUSTED_PROXY_IPS` as written. Unset, no proxy is trusted (`identity`).
    pub trusted_proxies: Option<String>,
    pub http_timeout: Duration,
}

impl Bootstrap {
    /// The bootstrap of this environment. `instance` is the path of the instance file the
    /// configuration is read with. A value that cannot be used is a warning.
    pub fn from(
        env: &dyn Env,
        instance: Option<&str>,
        warnings: &mut Vec<String>,
    ) -> Result<Bootstrap, ConfigError> {
        let database_url = env
            .get("DATABASE_URL")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or(ConfigError::Missing("DATABASE_URL"))?;
        let owner_emails = set(env, "SINGULARR_OWNER_EMAILS").unwrap_or_default();
        if owner_emails.is_empty() {
            warnings.push("SINGULARR_OWNER_EMAILS is empty: no owner sign-in".to_string());
        }
        // No proxy is trusted until one is listed (`identity`).
        let trusted_proxies = set(env, "SINGULARR_TRUSTED_PROXY_IPS");
        if trusted_proxies.is_none() {
            warnings.push(
                "SINGULARR_TRUSTED_PROXY_IPS is not set: no identity header is trusted".to_string(),
            );
        }
        let edge_token_set = set(env, "SINGULARR_EDGE_TOKEN").is_some();
        let edge_token_placeholder = env
            .get("SINGULARR_EDGE_TOKEN")
            .is_some_and(|v| v.trim() == PLACEHOLDER);
        if trusted_proxies.is_some() && !edge_token_set && !edge_token_placeholder {
            warnings.push(
                "SINGULARR_EDGE_TOKEN is not set: identity headers trusted by proxy address"
                    .to_string(),
            );
        }
        Ok(Bootstrap {
            database_url,
            db_max_connections: set(env, "SINGULARR_DB_MAX_CONNECTIONS")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(10)
                .clamp(2, 50),
            instance_path: instance.map(str::to_string),
            owner_emails,
            edge_token_set,
            edge_token_placeholder,
            trusted_proxies,
            http_timeout: seconds(env, "SINGULARR_HTTP_TIMEOUT_SECS", 30, 5, warnings),
        })
    }
}

/// `postgres://user:secret@host/db` with the password replaced.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "(set)".to_string();
    };
    match rest.rsplit_once('@') {
        Some((userinfo, host)) => {
            let user = userinfo.split(':').next().unwrap_or("");
            format!("{scheme}://{user}:***@{host}")
        }
        None => format!("{scheme}://{rest}"),
    }
}
