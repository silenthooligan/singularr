//! Who the edge says the caller is, and whether to believe it.
//!
//! An authenticating proxy in front of the server (oauth2-proxy behind Caddy's `forward_auth`,
//! for example) puts the signed-in address in `X-Auth-Request-Email` and the user name in
//! `X-Auth-Request-User`. The server can often be reached without that proxy too, and there a
//! client could set those headers itself. [`strip_untrusted_identity`] removes them from every
//! request that does not come from the edge, so a handler that sees the header can trust it.
//! [`Allowlist`] is the exact, lower-cased list of addresses that may sign in.
//!
//! "From the edge" means two things at once: the TCP peer is a listed proxy
//! (`SINGULARR_TRUSTED_PROXY_IPS`) and, when `SINGULARR_EDGE_TOKEN` is set, the request carries
//! that value in `X-Edge-Token`. The token is for networks where the proxy's address is not the
//! proxy's alone: a VPN router on the same host that source-NATs its clients to that address
//! would otherwise pass for the proxy. The proxy sets the header on every request it forwards
//! and overwrites any value a client sent.
//!
//! No proxy is trusted until one is listed: a fresh install honors no identity header, and the
//! start says so in the log. This module says who the caller is and whether the address is
//! listed; what the caller may do is `core::auth`'s.

use super::config::Env;
use axum::{
    extract::{ConnectInfo, Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

/// The variable that lists the proxies whose identity headers are honored.
pub const TRUSTED_PROXIES_VAR: &str = "SINGULARR_TRUSTED_PROXY_IPS";

/// The variable that holds the token the proxy sends in [`EDGE_TOKEN_HEADER`].
pub const EDGE_TOKEN_VAR: &str = "SINGULARR_EDGE_TOKEN";

/// Headers that carry a proxy-asserted identity.
pub const IDENTITY_HEADERS: [&str; 2] = ["x-auth-request-email", "x-auth-request-user"];

/// Header the proxy stamps on forwarded requests with the value of `SINGULARR_EDGE_TOKEN`.
pub const EDGE_TOKEN_HEADER: &str = "x-edge-token";

/// Parse a comma-separated list of IP addresses; entries that do not parse are ignored so one
/// typo cannot silently widen trust to "everything".
pub fn parse_trusted(list: &str) -> Vec<IpAddr> {
    list.split(',')
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .collect()
}

/// IPv4-mapped IPv6 peers (`::ffff:192.0.2.10`) compare equal to their IPv4 form.
pub fn is_trusted_proxy(peer: IpAddr, trusted: &[IpAddr]) -> bool {
    let peer = peer.to_canonical();
    trusted.iter().any(|t| t.to_canonical() == peer)
}

/// Constant-time comparison so the token check does not leak through timing.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The whole trust decision, pure for tests: the peer must be a listed proxy and, when a token
/// is configured, the request must carry it.
pub fn is_edge_request(
    peer: IpAddr,
    headers: &HeaderMap,
    trusted: &[IpAddr],
    token: Option<&str>,
) -> bool {
    if !is_trusted_proxy(peer, trusted) {
        return false;
    }
    match token {
        None => true,
        Some(expected) => headers
            .get(EDGE_TOKEN_HEADER)
            .map(|v| same(v.as_bytes(), expected.as_bytes()))
            .unwrap_or(false),
    }
}

/// The edge as this process trusts it: the listed proxies and the token, read once at the
/// start. The default trusts no proxy.
#[derive(Clone, Default)]
pub struct Edge {
    trusted: Arc<[IpAddr]>,
    token: Option<Arc<str>>,
}

impl Edge {
    /// A blank or whitespace token counts as none.
    pub fn new(trusted: Vec<IpAddr>, token: Option<&str>) -> Edge {
        Edge {
            trusted: trusted.into(),
            token: token
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(Arc::from),
        }
    }

    /// From `SINGULARR_TRUSTED_PROXY_IPS` and `SINGULARR_EDGE_TOKEN`. Unset, the list is
    /// empty: no proxy is trusted.
    pub fn from_env(env: &dyn Env) -> Edge {
        let trusted = env
            .get(TRUSTED_PROXIES_VAR)
            .map(|raw| parse_trusted(&raw))
            .unwrap_or_default();
        Edge::new(trusted, env.get(EDGE_TOKEN_VAR).as_deref())
    }

    /// The proxies whose identity headers are honored.
    pub fn trusted(&self) -> &[IpAddr] {
        &self.trusted
    }

    /// Whether an edge token is configured (for the start's log).
    pub fn token_configured(&self) -> bool {
        self.token.is_some()
    }

    /// Whether this request comes from the edge ([`is_edge_request`]).
    pub fn is_edge_request(&self, peer: IpAddr, headers: &HeaderMap) -> bool {
        is_edge_request(peer, headers, &self.trusted, self.token.as_deref())
    }
}

/// The token itself is never shown.
impl fmt::Debug for Edge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Edge")
            .field("trusted", &self.trusted)
            .field("token", &self.token.as_ref().map(|_| "set"))
            .finish()
    }
}

/// Remove the identity headers unless the request came from the edge. The edge token header
/// itself goes in every case, so no handler can see or echo it.
fn strip(headers: &mut HeaderMap, from_edge: bool) {
    if !from_edge {
        for name in IDENTITY_HEADERS {
            headers.remove(name);
        }
    }
    headers.remove(EDGE_TOKEN_HEADER);
}

/// Axum middleware, with the [`Edge`] as its state
/// (`axum::middleware::from_fn_with_state(edge, strip_untrusted_identity)`): remove the identity
/// headers unless the request comes from the edge, and the edge token header always. Requires
/// the router to be served with `into_make_service_with_connect_info::<SocketAddr>()`.
pub async fn strip_untrusted_identity(
    State(edge): State<Edge>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Response {
    let from_edge = edge.is_edge_request(peer.ip(), req.headers());
    strip(req.headers_mut(), from_edge);
    next.run(req).await
}

/// Exact, lower-cased e-mail allowlist. Empty admits nobody.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowlist(Vec<String>);

impl Allowlist {
    /// From a comma-separated string such as an environment variable.
    pub fn parse(raw: &str) -> Self {
        Self(
            raw.split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect(),
        )
    }

    /// Exact match after trimming and lower-casing; never a substring.
    pub fn allows(&self, email: &str) -> bool {
        let email = email.trim().to_lowercase();
        !email.is_empty() && self.0.contains(&email)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn parses_a_list_and_skips_garbage() {
        let v = parse_trusted(" 192.0.2.10, 2001:db8::10 ,not-an-ip,, ");
        assert_eq!(v.len(), 2);
        assert!(v.contains(&"192.0.2.10".parse().unwrap()));
        assert!(v.contains(&"2001:db8::10".parse().unwrap()));
    }

    #[test]
    fn trusts_only_listed_peers() {
        let trusted = parse_trusted("192.0.2.10");
        assert!(is_trusted_proxy("192.0.2.10".parse().unwrap(), &trusted));
        assert!(is_trusted_proxy(
            "::ffff:192.0.2.10".parse().unwrap(),
            &trusted
        ));
        assert!(!is_trusted_proxy("192.0.2.74".parse().unwrap(), &trusted));
        assert!(!is_trusted_proxy("127.0.0.1".parse().unwrap(), &trusted));
        assert!(!is_trusted_proxy("192.0.2.10".parse().unwrap(), &[]));
    }

    #[test]
    fn edge_request_needs_the_peer_and_the_token() {
        let trusted = parse_trusted("192.0.2.10");
        let proxy: IpAddr = "192.0.2.10".parse().unwrap();
        let vpn_via_snat: IpAddr = "192.0.2.10".parse().unwrap();
        let lan: IpAddr = "192.0.2.74".parse().unwrap();
        let mut with = HeaderMap::new();
        with.insert(EDGE_TOKEN_HEADER, "s3cret".parse().unwrap());
        let mut wrong = HeaderMap::new();
        wrong.insert(EDGE_TOKEN_HEADER, "s3cre7".parse().unwrap());
        let none = HeaderMap::new();

        // No token configured: the peer alone decides.
        assert!(is_edge_request(proxy, &none, &trusted, None));
        assert!(!is_edge_request(lan, &with, &trusted, None));
        // Token configured: both are required.
        assert!(is_edge_request(proxy, &with, &trusted, Some("s3cret")));
        assert!(!is_edge_request(
            vpn_via_snat,
            &none,
            &trusted,
            Some("s3cret")
        ));
        assert!(!is_edge_request(proxy, &wrong, &trusted, Some("s3cret")));
        assert!(!is_edge_request(lan, &with, &trusted, Some("s3cret")));
    }

    #[test]
    fn a_fresh_install_trusts_no_proxy() {
        let unset: BTreeMap<&str, &str> = BTreeMap::new();
        let edge = Edge::from_env(&unset);
        assert!(edge.trusted().is_empty());
        assert!(!edge.token_configured());
        let mut with = HeaderMap::new();
        with.insert(EDGE_TOKEN_HEADER, "s3cret".parse().unwrap());
        for peer in ["192.0.2.10", "127.0.0.1", "::1"] {
            assert!(
                !edge.is_edge_request(peer.parse().unwrap(), &with),
                "{peer}"
            );
        }

        let set = BTreeMap::from([
            (TRUSTED_PROXIES_VAR, "192.0.2.10, nonsense"),
            (EDGE_TOKEN_VAR, " s3cret "),
        ]);
        let edge = Edge::from_env(&set);
        assert_eq!(edge.trusted(), &["192.0.2.10".parse::<IpAddr>().unwrap()]);
        assert!(edge.token_configured());
        assert!(edge.is_edge_request("192.0.2.10".parse().unwrap(), &with));
        assert!(
            !format!("{edge:?}").contains("s3cret"),
            "the token is shown"
        );

        let blank = BTreeMap::from([(EDGE_TOKEN_VAR, "  ")]);
        assert!(!Edge::from_env(&blank).token_configured());
    }

    #[test]
    fn allowlist_is_exact_and_case_insensitive() {
        let allow = Allowlist::parse("Jordan.Lee@example.org, riley.lee@example.org ,");
        assert!(allow.allows("jordan.lee@EXAMPLE.org"));
        assert!(allow.allows(" riley.lee@example.org "));
        assert!(!allow.allows("jordan.test@example.org"));
        assert!(!allow.allows("lee"));
        assert!(!allow.allows(""));
        assert!(!Allowlist::parse("").allows("jordan.lee@example.org"));
    }

    #[tokio::test]
    async fn middleware_strips_headers_from_untrusted_peers_and_drops_the_token() {
        use axum::{Router, body::Body, middleware::from_fn_with_state, routing::get};
        use tower::ServiceExt;

        async fn echo(headers: HeaderMap) -> String {
            let email = headers
                .get("x-auth-request-email")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("none");
            let token = if headers.contains_key(EDGE_TOKEN_HEADER) {
                "token-visible"
            } else {
                "token-gone"
            };
            format!("{email} {token}")
        }
        let send = |app: Router, peer: &'static str, token: &'static str| async move {
            let mut req = Request::builder()
                .uri("/")
                .header("x-auth-request-email", "jordan@example.org")
                .header(EDGE_TOKEN_HEADER, token)
                .body(Body::empty())
                .unwrap();
            req.extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            let res = app.oneshot(req).await.unwrap();
            let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
            String::from_utf8(body.to_vec()).unwrap()
        };

        // The default: nobody is trusted, so the identity goes for every peer.
        let fresh = Router::new()
            .route("/", get(echo))
            .layer(from_fn_with_state(
                Edge::default(),
                strip_untrusted_identity,
            ));
        for peer in ["192.0.2.10:5000", "127.0.0.1:5000"] {
            assert_eq!(
                send(fresh.clone(), peer, "s3cret").await,
                "none token-gone",
                "peer {peer}"
            );
        }

        // A listed proxy with the token keeps it; anyone else, or a wrong token, loses it.
        let edge = Edge::new(parse_trusted("192.0.2.10"), Some("s3cret"));
        let app = Router::new()
            .route("/", get(echo))
            .layer(from_fn_with_state(edge, strip_untrusted_identity));
        for (peer, token, expect) in [
            ("192.0.2.10:5000", "s3cret", "jordan@example.org token-gone"),
            ("192.0.2.10:5000", "wrong", "none token-gone"),
            ("192.0.2.74:5000", "s3cret", "none token-gone"),
        ] {
            assert_eq!(
                send(app.clone(), peer, token).await,
                expect,
                "peer {peer}, token {token}"
            );
        }
    }
}
