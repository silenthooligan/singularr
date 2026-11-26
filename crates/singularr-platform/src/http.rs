//! The server's HTTP conventions: one error shape, a request id on every answer, the health
//! route pair, and the signal that stops the server.
//!
//! - [`Problem`] is an RFC 9457 `application/problem+json` body with the extension `code`.
//!   Handlers return it as an error; the status comes from the problem itself.
//! - [`request_id`] is a middleware that honors an incoming `X-Request-Id` or mints a UUID,
//!   stores it as a request extension, and echoes it on the response. [`RequestId`] extracts it
//!   in handlers.
//! - [`health_routes`] mounts `/healthz` (the process is up) and `/readyz` (its dependencies
//!   answer).
//! - [`check_health`] is `singularrd health`: it asks `/readyz` of the server at the listen
//!   address. The images' health checks run it.
//! - [`shutdown_signal`] resolves when the process is asked to stop.

use axum::{
    Json, Router,
    extract::{FromRequestParts, Request},
    http::{HeaderValue, StatusCode, header::HeaderName, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use std::convert::Infallible;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::outbound::{OutRequest, Outbound};

pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// The request id for the current request, minted or forwarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl<S: Send + Sync> FromRequestParts<S> for RequestId {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<RequestId>()
            .cloned()
            .unwrap_or_else(|| RequestId("unset".into())))
    }
}

/// Middleware: forward a sane incoming `X-Request-Id` (printable, up to 128 bytes) or mint a
/// UUID v4; keep it as an extension and set it on the response.
pub async fn request_id(mut req: Request, next: Next) -> Response {
    let incoming = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_graphic()))
        .map(str::to_owned);
    let id = incoming.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    req.extensions_mut().insert(RequestId(id.clone()));
    let mut res = next.run(req).await;
    if let Ok(v) = HeaderValue::from_str(&id) {
        res.headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), v);
    }
    res
}

/// RFC 9457 problem details with a stable code.
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    /// Always `about:blank`.
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Stable machine-readable code, for example `singularr.not_found`.
    pub code: String,
    /// The act the problem is about, when there is one (`GET /api/v1/acts/{id}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub act: Option<i64>,
}

impl Problem {
    pub fn new(status: StatusCode, code: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            kind: "about:blank".into(),
            title: title.into(),
            status: status.as_u16(),
            detail: None,
            code: code.into(),
            act: None,
        }
    }

    /// The problem names the act it is about.
    pub fn with_act(mut self, act: i64) -> Self {
        self.act = Some(act);
        self
    }

    pub fn status_code(&self) -> StatusCode {
        StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let mut res = (status, Json(self)).into_response();
        res.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        res
    }
}

/// A readiness probe: `Ok(())` when dependencies answer, `Err(reason)` otherwise.
pub type ReadyCheck =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

/// `/healthz` answers 200 while the process runs; `/readyz` runs the check and answers 200 or
/// 503 with the reason. Mount with `Router::merge`.
pub fn health_routes<S: Clone + Send + Sync + 'static>(ready: ReadyCheck) -> Router<S> {
    Router::new()
        .route("/healthz", get(|| async { (StatusCode::OK, "ok") }))
        .route(
            "/readyz",
            get(move || {
                let ready = ready.clone();
                async move {
                    match ready().await {
                        Ok(()) => (StatusCode::OK, "ready".to_string()),
                        Err(reason) => (
                            StatusCode::SERVICE_UNAVAILABLE,
                            format!("not ready: {reason}"),
                        ),
                    }
                }
            }),
        )
}

/// The address `singularrd health` asks: `/readyz` at the listen address. A wildcard address
/// (`0.0.0.0`, `::`) stands for this machine. The loopback of its family is asked.
pub fn ready_url(listen: &str) -> Result<String, String> {
    let addr: SocketAddr = listen
        .trim()
        .parse()
        .map_err(|e| format!("SINGULARR_LISTEN is not an address: {e}"))?;
    let ip = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    Ok(format!(
        "http://{}/readyz",
        SocketAddr::new(ip, addr.port())
    ))
}

/// `singularrd health`: `GET /readyz` of the server at the listen address. The way out lists
/// that address alone, read-only, with that one GET ([`Outbound::direct`]). No proxy of the
/// environment carries it. `Ok` on 200. Any other answer, or none, is the reason it is not
/// ready.
pub async fn check_health(listen: &str, timeout: Duration) -> Result<(), String> {
    let url = ready_url(listen)?;
    let base = url.trim_end_matches("/readyz");
    let out =
        Outbound::direct("singularrd", base, "/readyz", timeout).map_err(|e| e.to_string())?;
    // A time limit of its own: the probe is sent once, within the image's health check.
    let res = out
        .send(OutRequest::get(&url).timeout(timeout))
        .await
        .map_err(|e| e.to_string())?;
    if res.status == 200 {
        return Ok(());
    }
    Err(format!(
        "{url} answered {}: {}",
        res.status,
        String::from_utf8_lossy(&res.body).trim()
    ))
}

/// Resolves when the process is asked to stop: SIGTERM (`docker stop`, a redeploy) or SIGINT.
/// Start the drain from it: stop the loops and let the HTTP server stop accepting. (A process
/// that is PID 1 in its container ignores SIGTERM unless it listens for it.)
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(mut term), Ok(mut int)) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
            }
            _ => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware::from_fn};
    use tower::ServiceExt;

    #[tokio::test]
    async fn request_id_is_forwarded_or_minted_and_echoed() {
        async fn show(id: RequestId) -> String {
            id.0
        }
        let app = Router::new()
            .route("/", get(show))
            .layer(from_fn(request_id));

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("x-request-id", "abc-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.headers().get("x-request-id").unwrap(), "abc-123");
        let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"abc-123");

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("x-request-id", "bad id with spaces")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let minted = res
            .headers()
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(minted.len(), 36, "a uuid was minted instead: {minted}");
    }

    #[tokio::test]
    async fn problem_renders_as_problem_json_with_its_status() {
        let mut p = Problem::new(StatusCode::BAD_REQUEST, "test.invalid", "Invalid request");
        p.detail = Some("name is required".to_string());
        let res = p.into_response();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "application/problem+json"
        );
        let body: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 4096).await.unwrap())
                .unwrap();
        assert_eq!(body["type"], "about:blank");
        assert_eq!(body["code"], "test.invalid");
        assert_eq!(body["status"], 400);
        assert_eq!(body["detail"], "name is required");

        let bare = Problem::new(StatusCode::NOT_FOUND, "test.missing", "Not found");
        let body = serde_json::to_value(&bare).unwrap();
        assert!(body.get("detail").is_none(), "an absent detail is left out");
    }

    #[tokio::test]
    async fn health_routes_report_ready_and_not_ready() {
        let down: ReadyCheck = Arc::new(|| Box::pin(async { Err("db down".to_string()) }));
        let app: Router = health_routes(down);
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"not ready: db down");

        let up: ReadyCheck = Arc::new(|| Box::pin(async { Ok(()) }));
        let res = health_routes::<()>(up)
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// `singularrd health` asks `/readyz` of a server that listens: 200 is ready, 503 is not,
    /// and no answer is not either. A wildcard listen address is asked at the loopback.
    #[tokio::test]
    async fn the_health_command_asks_readyz_at_the_listen_address() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let up = Arc::new(AtomicBool::new(true));
        let flag = up.clone();
        let check: ReadyCheck = Arc::new(move || {
            let ready = flag.load(Ordering::SeqCst);
            Box::pin(async move {
                if ready {
                    Ok(())
                } else {
                    Err("the database does not answer".to_string())
                }
            })
        });
        let app: Router = health_routes(check);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let wait = Duration::from_secs(5);
        assert_eq!(check_health(&addr.to_string(), wait).await, Ok(()));
        up.store(false, Ordering::SeqCst);
        let err = check_health(&addr.to_string(), wait).await.unwrap_err();
        assert!(
            err.contains("503") && err.contains("the database does not answer"),
            "{err}"
        );
        // The way out of the health command: that address with that one GET. Every other path
        // and host is refused before anything is sent.
        let base = format!("http://{addr}");
        let out = Outbound::direct("singularrd", &base, "/readyz", wait).unwrap();
        for other in ["/healthz", "/api/v1/whoami", "/readyz/x", "/"] {
            let e = out
                .send(OutRequest::get(format!("{base}{other}")))
                .await
                .unwrap_err();
            assert!(
                matches!(e, crate::outbound::OutboundError::NotListed { .. }),
                "{other}: {e}"
            );
        }
        let e = out
            .send(OutRequest::get("http://127.0.0.2:9/readyz"))
            .await
            .unwrap_err();
        assert!(
            matches!(e, crate::outbound::OutboundError::HostNotAllowed { .. }),
            "{e}"
        );
        // Only an address of this machine, written as one.
        for elsewhere in [
            "http://192.0.2.1:3000",
            "http://localhost:3000",
            "http://example.org",
        ] {
            assert!(
                Outbound::direct("singularrd", elsewhere, "/readyz", wait).is_err(),
                "{elsewhere}"
            );
        }
        // Nothing listens there any more.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gone = closed.local_addr().unwrap();
        drop(closed);
        assert!(check_health(&gone.to_string(), wait).await.is_err());
        assert_eq!(
            ready_url("0.0.0.0:3000").unwrap(),
            "http://127.0.0.1:3000/readyz"
        );
        assert_eq!(ready_url("[::]:3000").unwrap(), "http://[::1]:3000/readyz");
        assert_eq!(
            ready_url(" 10.0.0.5:8080 ").unwrap(),
            "http://10.0.0.5:8080/readyz"
        );
        assert!(ready_url("nowhere").is_err());
    }
}
