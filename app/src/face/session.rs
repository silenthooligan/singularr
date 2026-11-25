//! Sessions, cookies, the Plex PIN in flight, form reading and rate limits of Singularr
//! Requests.
//!
//! - A session token is 32 random bytes in hex. The browser holds it in
//!   `__Host-mq_session` (host-only: no `Domain`, `Path=/`, `Secure`, `HttpOnly`,
//!   `SameSite=Lax`, 30 days); the database holds only its SHA-256.
//! - The Plex PIN between the sign-in and Plex's return sits in `__Host-mq_pin`, signed with
//!   HMAC-SHA256 under the session secret, 15 minutes.
//! - Every other cookie the face receives is ignored, the Google gate's included.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const SESSION_COOKIE: &str = "__Host-mq_session";
pub const PIN_COOKIE: &str = "__Host-mq_pin";
/// How long a session lives.
pub const SESSION_DAYS: i64 = 30;
/// How long a sign-in may wait for Plex.
pub const PIN_SECS: i64 = 15 * 60;

/// 32 random bytes, hex: a session token.
pub fn new_token() -> String {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(a.as_bytes());
    bytes.extend_from_slice(b.as_bytes());
    hex::encode(bytes)
}

/// The hex SHA-256 of a token: what the database stores and compares.
pub fn token_sha256(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// A token as the face accepts it from a cookie: 64 lower-case hex characters.
pub fn plausible_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.iter().map(|b| b ^ 0x36).collect::<Vec<u8>>());
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.iter().map(|b| b ^ 0x5c).collect::<Vec<u8>>());
    outer.update(inner);
    outer.finalize().into()
}

/// Compare two strings in time that does not depend on where they differ.
fn same(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// The PIN cookie's value: `<pin id>.<expires, unix seconds>.<hmac>`.
pub fn pin_cookie_value(secret: &str, pin_id: i64, expires_unix: i64) -> String {
    let mac = hmac_sha256(
        secret.as_bytes(),
        format!("pin|{pin_id}|{expires_unix}").as_bytes(),
    );
    format!("{pin_id}.{expires_unix}.{}", hex::encode(mac))
}

/// The PIN id of a PIN cookie whose signature holds and which has not expired.
pub fn read_pin_cookie(secret: &str, value: &str, now_unix: i64) -> Option<i64> {
    let mut parts = value.split('.');
    let (id, expires, _mac) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let (pin_id, expires_unix): (i64, i64) = (id.parse().ok()?, expires.parse().ok()?);
    let want = pin_cookie_value(secret, pin_id, expires_unix);
    (same(&want, value) && expires_unix > now_unix && pin_id > 0).then_some(pin_id)
}

/// A `Set-Cookie` value for one of the face's cookies: host-only, the whole site, Secure,
/// HttpOnly, SameSite=Lax. `max_age` 0 clears it.
pub fn set_cookie(name: &str, value: &str, max_age: i64) -> String {
    format!("{name}={value}; Path=/; Max-Age={max_age}; Secure; HttpOnly; SameSite=Lax")
}

/// The face's own cookies from a `Cookie` header. Any other cookie is ignored.
pub fn own_cookies(header: Option<&str>) -> HashMap<&'static str, String> {
    let mut out = HashMap::new();
    let Some(header) = header else {
        return out;
    };
    for pair in header.split(';') {
        let Some((name, value)) = pair.trim().split_once('=') else {
            continue;
        };
        for own in [SESSION_COOKIE, PIN_COOKIE] {
            if name.trim() == own && !out.contains_key(own) {
                out.insert(own, value.trim().to_string());
            }
        }
    }
    out
}

/// A change asked for by a page of another site is refused: browsers say where a request
/// comes from in `Sec-Fetch-Site`, and in `Origin`, which must then be the face's own.
pub fn same_origin(sec_fetch_site: Option<&str>, origin: Option<&str>, public_url: &str) -> bool {
    if let Some(site) = sec_fetch_site
        && !matches!(site, "same-origin" | "none")
    {
        return false;
    }
    match origin {
        Some(o) => o.trim_end_matches('/') == public_url,
        None => true,
    }
}

/// Read an `application/x-www-form-urlencoded` body (or a query string) into pairs, in order.
pub fn form_pairs(body: &str) -> Vec<(String, String)> {
    fn hex_value(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() => {
                    match (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                        (Some(h), Some(l)) => {
                            out.push(h * 16 + l);
                            i += 2;
                        }
                        _ => out.push(b'%'),
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    body.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(p), String::new()),
        })
        .collect()
}

/// A sliding-window limit per key: at most `max` events in `window`.
pub struct Limiter {
    max: usize,
    window: Duration,
    seen: Mutex<HashMap<String, Vec<Instant>>>,
}

impl Limiter {
    pub fn new(max: usize, window: Duration) -> Limiter {
        Limiter {
            max,
            window,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Count one event for `key`; false when the key is over its limit (the event is not
    /// counted then).
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.len() > 10_000 {
            let window = self.window;
            seen.retain(|_, times| times.iter().any(|t| now.duration_since(*t) < window));
        }
        let times = seen.entry(key.to_string()).or_default();
        times.retain(|t| now.duration_since(*t) < self.window);
        if times.len() >= self.max {
            return false;
        }
        times.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_hex_and_stored_hashed() {
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        assert!(plausible_token(&a) && plausible_token(&b));
        assert_eq!(token_sha256(&a).len(), 64);
        assert_ne!(token_sha256(&a), a);
        assert!(!plausible_token("abc"));
        assert!(!plausible_token(&a.to_uppercase()));
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // RFC 4231, test case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than the block.
        let mac = hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex::encode(mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn a_pin_cookie_holds_only_when_signed_and_fresh() {
        let secret = "0123456789abcdef0123456789abcdef";
        let v = pin_cookie_value(secret, 42, 2_000);
        assert_eq!(read_pin_cookie(secret, &v, 1_000), Some(42));
        assert_eq!(read_pin_cookie(secret, &v, 2_000), None, "expired");
        assert_eq!(
            read_pin_cookie("another secret 0123456789abcdef!", &v, 1_000),
            None
        );
        let forged = v.replacen("42.", "43.", 1);
        assert_eq!(read_pin_cookie(secret, &forged, 1_000), None);
        assert_eq!(read_pin_cookie(secret, "42.2000", 1_000), None);
        assert_eq!(read_pin_cookie(secret, &format!("{v}.x"), 1_000), None);
    }

    #[test]
    fn cookies_are_host_only_and_strict() {
        let c = set_cookie(SESSION_COOKIE, "abc", 60);
        assert!(c.starts_with("__Host-mq_session=abc;"));
        for part in ["Path=/", "Secure", "HttpOnly", "SameSite=Lax", "Max-Age=60"] {
            assert!(c.contains(part), "{part} in {c}");
        }
        assert!(!c.to_ascii_lowercase().contains("domain"));
        // Only the face's own cookies are read; the gate's cookie for .example.org is ignored.
        let jar = own_cookies(Some(
            "_oauth2_proxy=gate-session; __Host-mq_session=tok; mq_session=x; __Host-mq_pin=p",
        ));
        assert_eq!(jar.len(), 2);
        assert_eq!(jar.get(SESSION_COOKIE).map(String::as_str), Some("tok"));
        assert_eq!(jar.get(PIN_COOKIE).map(String::as_str), Some("p"));
        assert!(own_cookies(Some("_oauth2_proxy=a")).is_empty());
        assert!(own_cookies(None).is_empty());
    }

    #[test]
    fn changes_from_another_site_are_refused() {
        let me = "https://requests.example.org";
        assert!(same_origin(Some("same-origin"), Some(me), me));
        assert!(same_origin(None, None, me));
        assert!(!same_origin(Some("cross-site"), None, me));
        assert!(!same_origin(
            Some("same-site"),
            Some("https://singularr.example.org"),
            me
        ));
        assert!(!same_origin(None, Some("https://evil.example"), me));
    }

    #[test]
    fn forms_are_read_in_order() {
        assert_eq!(
            form_pairs("seasons=1&seasons=2&title=Breaking+Bad%21&empty="),
            vec![
                ("seasons".to_string(), "1".to_string()),
                ("seasons".to_string(), "2".to_string()),
                ("title".to_string(), "Breaking Bad!".to_string()),
                ("empty".to_string(), String::new()),
            ]
        );
        assert_eq!(
            form_pairs("q=caf%C3%A9&bad=%zz&end=%4"),
            vec![
                ("q".to_string(), "café".to_string()),
                ("bad".to_string(), "%zz".to_string()),
                ("end".to_string(), "%4".to_string()),
            ]
        );
        assert!(form_pairs("").is_empty());
    }

    #[test]
    fn the_limiter_counts_per_key() {
        let l = Limiter::new(2, Duration::from_secs(60));
        assert!(l.allow("a"));
        assert!(l.allow("a"));
        assert!(!l.allow("a"));
        assert!(l.allow("b"));
    }
}
