//! Who is calling, and what they may do.
//!
//! Two kinds of caller exist in stage 1:
//!
//! - The owner: a browser session through the edge. An authenticating proxy puts the
//!   signed-in address in `X-Auth-Request-Email`; `identity::strip_untrusted_identity` has
//!   already removed that header from every request that did not come from the edge, so a
//!   header that is still there can be trusted. The address must be listed in
//!   `SINGULARR_OWNER_EMAILS`. The owner holds every scope.
//! - A machine: `Authorization: Bearer mq_<key>`. Keys are stored as a SHA-256 hash and
//!   compared in constant time. A key holds the scopes it was created with.

use super::error::AppError;
use super::identity::{Allowlist, IDENTITY_HEADERS};
use super::model::{Scope, Whoami};
use super::store;
use axum::http::HeaderMap;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;

pub const KEY_MARK: &str = "mq_";
/// Characters of the key after `mq_` that are stored in the clear to find the row.
pub const PREFIX_LEN: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Owner {
        email: String,
    },
    Key {
        id: i64,
        name: String,
        scopes: Vec<Scope>,
    },
}

impl Caller {
    /// The owner may do everything. A key may do what its scopes say; `admin` includes all.
    pub fn has(&self, scope: Scope) -> bool {
        match self {
            Caller::Owner { .. } => true,
            Caller::Key { scopes, .. } => scopes.contains(&scope) || scopes.contains(&Scope::Admin),
        }
    }

    pub fn require(&self, scope: Scope) -> Result<(), AppError> {
        if self.has(scope) {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!(
                "This key lacks the `{}` scope.",
                scope.as_str()
            )))
        }
    }

    /// The journal's `actor` for something this caller did.
    pub fn actor(&self) -> String {
        match self {
            Caller::Owner { email } => format!("user:{email}"),
            Caller::Key { name, .. } => format!("key:{name}"),
        }
    }

    pub fn whoami(&self) -> Whoami {
        match self {
            Caller::Owner { email } => Whoami {
                kind: "owner".to_string(),
                name: email.clone(),
                scopes: Scope::ALL.to_vec(),
            },
            Caller::Key { name, scopes, .. } => Whoami {
                kind: "key".to_string(),
                name: name.clone(),
                scopes: scopes.clone(),
            },
        }
    }
}

/// A new key. `secret` goes to the caller once; `prefix` and `sha256` are stored.
pub struct MintedKey {
    pub secret: String,
    pub prefix: String,
    pub sha256: String,
}

/// `mq_` and 64 hex characters from two random UUIDs (244 random bits from the OS).
pub fn mint_key() -> MintedKey {
    let random = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let secret = format!("{KEY_MARK}{random}");
    MintedKey {
        prefix: key_prefix(&secret).unwrap_or_default().to_string(),
        sha256: sha256_hex(&secret),
        secret,
    }
}

/// The part of a key that finds its row: `mq_` and the next eight characters.
pub fn key_prefix(secret: &str) -> Option<&str> {
    let rest = secret.strip_prefix(KEY_MARK)?;
    (rest.len() > PREFIX_LEN && rest.is_ascii()).then(|| &secret[..KEY_MARK.len() + PREFIX_LEN])
}

pub fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Compare a presented key with a stored hash without leaking where they differ.
pub fn key_matches(secret: &str, stored_sha256: &str) -> bool {
    let presented = sha256_hex(secret);
    presented.as_bytes().ct_eq(stored_sha256.as_bytes()).into()
}

/// The token of `Authorization: Bearer <token>`.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

/// What the headers alone say, before the key table is asked.
#[derive(Debug, PartialEq, Eq)]
pub enum Presented<'a> {
    Key(&'a str),
    Owner(String),
    /// Signed in at the edge, but the address is not listed.
    Stranger(String),
    Nothing,
}

/// A bearer token wins over a session: a caller that sends a key is judged by the key.
pub fn presented<'a>(headers: &'a HeaderMap, owners: &Allowlist) -> Presented<'a> {
    if let Some(token) = bearer(headers) {
        return Presented::Key(token);
    }
    let email = headers
        .get(IDENTITY_HEADERS[0])
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    match email {
        Some(email) if owners.allows(&email) => Presented::Owner(email),
        Some(email) => Presented::Stranger(email),
        None => Presented::Nothing,
    }
}

/// Resolve the caller of a request. Call only behind `strip_untrusted_identity`.
pub async fn identify(
    headers: &HeaderMap,
    pool: &PgPool,
    owners: &Allowlist,
) -> Result<Caller, AppError> {
    match presented(headers, owners) {
        Presented::Owner(email) => Ok(Caller::Owner { email }),
        Presented::Stranger(email) => {
            tracing::warn!(%email, "signed-in address is not listed in SINGULARR_OWNER_EMAILS");
            Err(AppError::Forbidden("Not an owner account.".to_string()))
        }
        Presented::Nothing => Err(AppError::Unauthorized),
        Presented::Key(secret) => {
            let prefix = key_prefix(secret).ok_or(AppError::Unauthorized)?;
            let row = store::api_key_by_prefix(pool, prefix)
                .await?
                .ok_or(AppError::Unauthorized)?;
            // The hash is compared even for a revoked key, so timing says nothing about it.
            let matches = key_matches(secret, &row.sha256);
            if !matches || row.revoked_at.is_some() {
                return Err(AppError::Unauthorized);
            }
            if let Err(e) = store::touch_api_key(pool, row.id).await {
                tracing::warn!(error = %e, "could not record the key's last use");
            }
            Ok(Caller::Key {
                id: row.id,
                name: row.name.clone(),
                scopes: row.scopes(),
            })
        }
    }
}

/// Create a key. The secret is returned once and only its hash is stored.
pub async fn create_key(
    pool: &PgPool,
    journal: &super::journal::Journal,
    caller: &Caller,
    new: &super::model::NewApiKey,
) -> Result<super::model::CreatedApiKey, AppError> {
    let name = new.name.trim();
    if name.is_empty() || name.len() > 80 {
        return Err(AppError::bad_request(
            "`name` is required, at most 80 characters.",
        ));
    }
    let mut scopes = new.scopes.clone();
    scopes.sort();
    scopes.dedup();
    if scopes.is_empty() {
        return Err(AppError::bad_request(
            "`scopes` needs one of read, library, acquire, settings, admin.",
        ));
    }
    let minted = mint_key();
    let mut tx = journal.begin(pool).await?;
    let row = store::insert_api_key(
        tx.conn(),
        name,
        &minted.prefix,
        &minted.sha256,
        &scopes,
        &caller.actor(),
    )
    .await?;
    tx.append_one(super::journal::event(
        "key_created",
        "key",
        row.id,
        &caller.actor(),
        serde_json::json!({ "name": row.name, "prefix": row.prefix, "scopes": row.scopes }),
    ))
    .await?;
    tx.commit().await?;
    Ok(super::model::CreatedApiKey {
        key: row.info(),
        secret: minted.secret,
    })
}

/// Revoke a key. It stops working at once and stays listed as revoked.
pub async fn revoke_key(
    pool: &PgPool,
    journal: &super::journal::Journal,
    caller: &Caller,
    id: i64,
) -> Result<super::model::ApiKeyInfo, AppError> {
    let mut tx = journal.begin(pool).await?;
    let row = store::revoke_api_key(tx.conn(), id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no active key {id}.")))?;
    tx.append_one(super::journal::event(
        "key_revoked",
        "key",
        row.id,
        &caller.actor(),
        serde_json::json!({ "name": row.name, "prefix": row.prefix }),
    ))
    .await?;
    tx.commit().await?;
    Ok(row.info())
}

pub async fn list_keys(pool: &PgPool) -> Result<Vec<super::model::ApiKeyInfo>, AppError> {
    Ok(store::list_api_keys(pool)
        .await?
        .iter()
        .map(|k| k.info())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (n, v) in pairs {
            h.insert(*n, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn a_minted_key_verifies_and_another_does_not() {
        let key = mint_key();
        assert!(key.secret.starts_with("mq_"));
        assert_eq!(key.secret.len(), 3 + 64);
        assert_eq!(key.prefix.len(), 3 + PREFIX_LEN);
        assert!(key.secret.starts_with(&key.prefix));
        assert_eq!(key.sha256.len(), 64);
        assert!(!key.sha256.contains(&key.secret[3..]));
        assert!(key_matches(&key.secret, &key.sha256));
        let other = mint_key();
        assert_ne!(other.secret, key.secret);
        assert!(!key_matches(&other.secret, &key.sha256));
        assert!(!key_matches(
            &key.secret[..key.secret.len() - 1],
            &key.sha256
        ));
    }

    #[test]
    fn prefixes_come_only_from_well_formed_keys() {
        assert_eq!(key_prefix("mq_0123456789abcdef"), Some("mq_01234567"));
        assert_eq!(key_prefix("mq_short"), None);
        assert_eq!(key_prefix("sk_0123456789abcdef"), None);
        assert_eq!(key_prefix(""), None);
    }

    #[test]
    fn the_bearer_token_is_read_from_the_header() {
        assert_eq!(
            bearer(&headers(&[("authorization", "Bearer mq_abc")])),
            Some("mq_abc")
        );
        assert_eq!(
            bearer(&headers(&[("authorization", "bearer  mq_abc ")])),
            Some("mq_abc")
        );
        assert_eq!(bearer(&headers(&[("authorization", "Basic abc")])), None);
        assert_eq!(bearer(&headers(&[("authorization", "Bearer ")])), None);
        assert_eq!(bearer(&HeaderMap::new()), None);
    }

    #[test]
    fn headers_decide_who_is_presented() {
        let owners = Allowlist::parse("owner@example.com");
        assert_eq!(presented(&HeaderMap::new(), &owners), Presented::Nothing);
        assert_eq!(
            presented(
                &headers(&[("x-auth-request-email", "Owner@Example.com")]),
                &owners
            ),
            Presented::Owner("owner@example.com".to_string())
        );
        assert_eq!(
            presented(
                &headers(&[("x-auth-request-email", "guest@example.com")]),
                &owners
            ),
            Presented::Stranger("guest@example.com".to_string())
        );
        // A key is judged as a key even when a session is present.
        assert_eq!(
            presented(
                &headers(&[
                    ("x-auth-request-email", "owner@example.com"),
                    ("authorization", "Bearer mq_whatever")
                ]),
                &owners
            ),
            Presented::Key("mq_whatever")
        );
        // An empty owner list admits no session.
        assert_eq!(
            presented(
                &headers(&[("x-auth-request-email", "owner@example.com")]),
                &Allowlist::parse("")
            ),
            Presented::Stranger("owner@example.com".to_string())
        );
    }

    #[test]
    fn scopes_gate_what_a_key_may_do() {
        let reader = Caller::Key {
            id: 1,
            name: "reader".into(),
            scopes: vec![Scope::Read],
        };
        assert!(reader.has(Scope::Read));
        assert!(!reader.has(Scope::Admin));
        assert!(reader.require(Scope::Settings).is_err());
        let admin = Caller::Key {
            id: 2,
            name: "admin".into(),
            scopes: vec![Scope::Admin],
        };
        assert!(admin.has(Scope::Read) && admin.has(Scope::Acquire));
        let owner = Caller::Owner {
            email: "owner@example.com".into(),
        };
        assert!(Scope::ALL.into_iter().all(|s| owner.has(s)));
        assert_eq!(owner.actor(), "user:owner@example.com");
        assert_eq!(reader.actor(), "key:reader");
    }
}
