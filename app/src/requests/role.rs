//! The portal's database role, kept by the full process: Singularr Requests reads its views as
//! this role. With `SINGULARR_REQUESTS_DB_PASSWORD` set, each start runs
//! `deploy/requests-role.sql` for the role after the migrations. A missing role is created. An
//! existing role holds the script's grants again: the views of later migrations follow at the
//! next start.
//!
//! The password reaches the database as a SCRAM-SHA-256 verifier computed here (RFC 5802, RFC
//! 7677). PostgreSQL keeps a verifier as it is sent. The password itself is never part of a
//! statement. An existing role keeps its password while a sign-in with the configured one works.
//!
//! Creating a role needs CREATEROLE. Without it a missing role is one warning naming the script.
//! The start goes on. An existing role needs no CREATEROLE: the owner of the tables grants on
//! them. An existing role is taken only when it looks like a portal role: it logs in, holds no
//! power and no membership, has no member but this login, and owns neither the database nor
//! anything in the schema `public`. Its password is checked before any grant changes.
//!
//! The role's statements go over a connection of their own that logs no statement. sqlx would
//! otherwise log a slow one with its text, a verifier included.

use super::store;
use crate::core::config::{PortalRoleConfig, plain_role_name};
use crate::core::smtp::base64;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use sqlx::{ConnectOptions, Connection, PgConnection, PgPool};
use std::time::Duration;

/// `deploy/requests-role.sql`: the role, its grants and nothing else.
pub const SCRIPT: &str = include_str!("../../../deploy/requests-role.sql");

/// The iterations of a new verifier: PostgreSQL's default (`scram_iterations`).
pub const ITERATIONS: u32 = 4096;

/// How long the sign-in that tries the password may take.
const SIGN_IN: Duration = Duration::from_secs(10);

/// The role the script names, the default of `SINGULARR_REQUESTS_DB_ROLE`: the word after
/// `CREATE ROLE`.
pub fn script_role() -> &'static str {
    SCRIPT
        .split("CREATE ROLE ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("singularr_requests")
}

/// The script for `role`: the script's own role name replaced wherever it stands as a word.
pub fn script_for(role: &str) -> String {
    replace_word(SCRIPT, script_role(), role)
}

/// `text` with `name` replaced by `with` wherever `name` is a whole word: no letter, digit or
/// `_` on either side.
fn replace_word(text: &str, name: &str, with: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(name) {
        out.push_str(&rest[..at]);
        let after = rest[at + name.len()..].chars().next();
        let whole = !word(out.chars().next_back()) && !word(after);
        out.push_str(if whole { with } else { name });
        rest = &rest[at + name.len()..];
    }
    out.push_str(rest);
    out
}

/// What the start did with the portal's role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// No portal password is set: the role is left as it is.
    NotAsked,
    /// The role was missing. It exists now, with the password and the script's grants.
    Created { role: String },
    /// The role existed and holds the script's grants again. `password_set`: the configured
    /// password did not sign in. The role has it now.
    Granted { role: String, password_set: bool },
    /// The role could not be kept: the start's one warning about it. The start goes on.
    Warning(String),
}

impl Kept {
    /// The start's line about the role, when there is one.
    pub fn log(&self) {
        match self {
            Kept::NotAsked => {}
            Kept::Created { role } => tracing::info!(
                role = %role,
                "the portal role was created with the grants of deploy/requests-role.sql"
            ),
            Kept::Granted { role, password_set } => tracing::info!(
                role = %role,
                password_set = *password_set,
                "the portal role holds the grants of deploy/requests-role.sql"
            ),
            Kept::Warning(warning) => tracing::warn!("{warning}"),
        }
    }
}

/// Keeps the portal's role, as the module says. A refusal is a warning: the start goes on
/// without the role.
pub async fn keep(pool: &PgPool, config: &PortalRoleConfig) -> Kept {
    let Some(password) = config.password.as_deref() else {
        return Kept::NotAsked;
    };
    let role = match config.role.as_deref() {
        Some(role) => role,
        None => script_role(),
    };
    match keep_role(pool, role, password).await {
        Ok(kept) => kept,
        Err(warning) => Kept::Warning(warning),
    }
}

async fn keep_role(pool: &PgPool, role: &str, password: &str) -> Result<Kept, String> {
    let left_alone = |why: &str| {
        format!(
            "SINGULARR_REQUESTS_DB_ROLE names {role}, {why}. The portal needs a role of its own (deploy/requests-role.sql). This one is left alone."
        )
    };
    if !plain_role_name(role) {
        return Err(left_alone(
            "a name of more than lowercase letters, digits and _",
        ));
    }
    if role.starts_with("pg_") {
        return Err(left_alone("a name PostgreSQL keeps for its own roles"));
    }
    let Ok(prepared) = stringprep::saslprep(password) else {
        return Err(format!(
            "SINGULARR_REQUESTS_DB_PASSWORD holds a character SCRAM refuses (a control character, for one). The portal role {role} is left alone."
        ));
    };
    let failed = |e: sqlx::Error| {
        format!(
            "The portal role {role} could not be kept: {e}. Run deploy/requests-role.sql by hand as a role that may create roles and grant on the tables, such as postgres. Singularr starts without it."
        )
    };
    let options = (*pool.connect_options())
        .clone()
        .disable_statement_logging();
    let mut quiet = PgConnection::connect_with(&options).await.map_err(failed)?;
    let kept = keep_on(&mut quiet, pool, role, password, &prepared, &left_alone).await;
    let _ = quiet.close().await;
    kept
}

/// The checks and the statements, over the quiet connection.
async fn keep_on(
    quiet: &mut PgConnection,
    pool: &PgPool,
    role: &str,
    password: &str,
    prepared: &str,
    left_alone: &dyn Fn(&str) -> String,
) -> Result<Kept, String> {
    let failed = |e: sqlx::Error| {
        format!(
            "The portal role {role} could not be kept: {e}. Run deploy/requests-role.sql by hand as a role that may create roles and grant on the tables, such as postgres. Singularr starts without it."
        )
    };
    let facts = store::role_facts(quiet, role).await.map_err(failed)?;
    if !facts.plain_name {
        return Err(left_alone("a word SQL reserves"));
    }
    if facts.login == role {
        return Err(left_alone("this database login itself"));
    }
    if facts.powers {
        return Err(left_alone(
            "a role with more than a login (superuser, CREATEROLE, CREATEDB, replication or BYPASSRLS)",
        ));
    }
    let role_name = role.to_string();
    if !facts.role_exists {
        if !facts.may_create_roles {
            return Err(format!(
                "The portal role {role} does not exist. This database login may not create roles (CREATEROLE). Run deploy/requests-role.sql as a role that may, such as postgres. Then set the role's password with psql's \\password {role}. Singularr starts without the role."
            ));
        }
        let verifier = new_verifier(prepared)?;
        store::apply_role_script(quiet, script_for(role), role, Some(&verifier))
            .await
            .map_err(failed)?;
        return Ok(Kept::Created { role: role_name });
    }
    // An existing role is taken as the portal's only when it looks like one.
    if !facts.can_login {
        return Err(left_alone("a role that may not log in"));
    }
    if facts.has_members || facts.is_member {
        return Err(left_alone(
            "a role with members other than this login, or with memberships of its own",
        ));
    }
    if facts.owns_database {
        return Err(left_alone("the owner of this database"));
    }
    if facts.owns_objects {
        return Err(left_alone("the owner of objects in the schema public"));
    }
    // The password first: no grant changes for a role this process cannot sign in as or set.
    let password_set = if signs_in(pool, role, password).await? {
        false
    } else {
        let verifier = new_verifier(prepared)?;
        store::set_role_password(quiet, role, &verifier)
            .await
            .map_err(|e| {
                format!(
                    "The portal role {role} does not sign in with SINGULARR_REQUESTS_DB_PASSWORD. This database login may not change the role's password: {e}. Set it with psql's \\password {role} as a role that may, such as postgres. Its grants are left as they are."
                )
            })?;
        true
    };
    store::apply_role_script(quiet, script_for(role), role, None)
        .await
        .map_err(failed)?;
    Ok(Kept::Granted {
        role: role_name,
        password_set,
    })
}

/// Whether a sign-in as the role with the password works: the role has that password, or the
/// server asks for none. A wrong password (28P01) is false. Any other failure is the warning.
async fn signs_in(pool: &PgPool, role: &str, password: &str) -> Result<bool, String> {
    let options = (*pool.connect_options())
        .clone()
        .username(role)
        .password(password);
    match tokio::time::timeout(SIGN_IN, PgConnection::connect_with(&options)).await {
        Ok(Ok(connection)) => {
            let _ = connection.close().await;
            Ok(true)
        }
        Ok(Err(sqlx::Error::Database(e))) if e.code().as_deref() == Some("28P01") => Ok(false),
        Ok(Err(e)) => Err(format!(
            "A sign-in as the portal role {role} failed: {e}. The role holds the grants of deploy/requests-role.sql. Its password is left as it is."
        )),
        Err(_) => Err(format!(
            "A sign-in as the portal role {role} took longer than {} s. The role holds the grants of deploy/requests-role.sql. Its password is left as it is.",
            SIGN_IN.as_secs()
        )),
    }
}

/// A verifier with a new random salt of 16 bytes, for a password SASLprep has prepared.
fn new_verifier(prepared: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt)
        .map_err(|e| format!("no random salt for the portal role's password: {e}"))?;
    Ok(verifier_of(prepared.as_bytes(), &salt, ITERATIONS))
}

/// The SCRAM-SHA-256 verifier of a password, as PostgreSQL stores it:
/// `SCRAM-SHA-256$<iterations>:<salt>$<StoredKey>:<ServerKey>`, each part after the
/// iterations in base64. The password is prepared with SASLprep first, as the client prepares
/// it to sign in. `None`: SASLprep refuses the password.
pub fn verifier(password: &str, salt: &[u8], iterations: u32) -> Option<String> {
    let prepared = stringprep::saslprep(password).ok()?;
    Some(verifier_of(prepared.as_bytes(), salt, iterations))
}

fn verifier_of(prepared: &[u8], salt: &[u8], iterations: u32) -> String {
    let salted = salted_password(prepared, salt, iterations);
    let client_key = hmac(&salted, b"Client Key");
    let stored_key = Sha256::digest(client_key);
    let server_key = hmac(&salted, b"Server Key");
    format!(
        "SCRAM-SHA-256${iterations}:{}${}:{}",
        base64(salt),
        base64(stored_key.as_slice()),
        base64(&server_key)
    )
}

/// HMAC-SHA-256.
fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// SaltedPassword := Hi(password, salt, iterations): PBKDF2 over HMAC-SHA-256, one block of
/// 32 bytes (RFC 5802, section 2.2).
fn salted_password(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut first = Vec::with_capacity(salt.len() + 4);
    first.extend_from_slice(salt);
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac(password, &first);
    let mut out = u;
    for _ in 1..iterations {
        u = hmac(password, &u);
        for (o, b) in out.iter_mut().zip(u) {
            *o ^= b;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7677's example: the password `pencil`, this salt and 4096 iterations.
    const SALT: [u8; 16] = [
        91, 109, 153, 104, 157, 18, 53, 142, 236, 160, 75, 20, 18, 54, 250, 129,
    ];

    #[test]
    fn the_verifier_of_rfc_7677_s_example_is_the_text_postgresql_stores() {
        assert_eq!(base64(&SALT), "W22ZaJ0SNY7soEsUEjb6gQ==");
        assert_eq!(
            verifier("pencil", &SALT, 4096).as_deref(),
            Some(
                "SCRAM-SHA-256$4096:W22ZaJ0SNY7soEsUEjb6gQ==$WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU="
            )
        );
    }

    #[test]
    fn the_keys_sign_rfc_7677_s_exchange() {
        // The client's proof and the server's signature of the RFC's exchange, from the salted
        // password this module computes.
        let salted = salted_password(b"pencil", &SALT, 4096);
        let client_key = hmac(&salted, b"Client Key");
        let stored_key: [u8; 32] = Sha256::digest(client_key).into();
        let server_key = hmac(&salted, b"Server Key");
        let auth = "n=user,r=rOprNGfwEbeRWgbNEkqO,\
                    r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096,\
                    c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0";
        let signature = hmac(&stored_key, auth.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(signature)
            .map(|(k, s)| k ^ s)
            .collect();
        assert_eq!(
            base64(&proof),
            "dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
        assert_eq!(
            base64(&hmac(&server_key, auth.as_bytes())),
            "6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4="
        );
    }

    #[test]
    fn a_new_verifier_has_a_salt_of_its_own() {
        let a = new_verifier("a-password").unwrap();
        let b = new_verifier("a-password").unwrap();
        assert!(a.starts_with("SCRAM-SHA-256$4096:"), "{a}");
        assert_ne!(a, b);
        assert!(!a.contains("a-password"));
    }

    #[test]
    fn a_password_saslprep_refuses_makes_no_verifier() {
        assert!(verifier("bell\u{7}", &SALT, 4096).is_none());
        // A space other than the ASCII one is the ASCII one, as the client prepares it.
        assert_eq!(
            verifier("pen\u{a0}cil", &SALT, 4096),
            verifier("pen cil", &SALT, 4096)
        );
    }

    #[test]
    fn the_script_names_its_role_and_runs_for_another() {
        let own = script_role();
        assert!(plain_role_name(own), "{own}");
        assert!(SCRIPT.contains(&format!("rolname = '{own}'")));
        assert_eq!(script_for(own), SCRIPT);
        let other = script_for("portal_test_role");
        assert!(other.contains("CREATE ROLE portal_test_role LOGIN"));
        assert!(other.contains("rolname = 'portal_test_role'"));
        assert!(other.contains("GRANT USAGE ON SCHEMA public TO portal_test_role;"));
        assert!(!other.contains(own), "every mention is replaced");
        assert_eq!(
            other.matches("portal_test_role").count(),
            SCRIPT.matches(own).count()
        );
        // A longer word that holds the name keeps it.
        assert_eq!(
            replace_word("r TO r; r_id_seq xr 'r' (r)", "r", "face"),
            "face TO face; r_id_seq xr 'face' (face)"
        );
    }
}
