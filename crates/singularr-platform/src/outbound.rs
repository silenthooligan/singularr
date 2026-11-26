//! The one way out. Every HTTP call Singularr makes goes through [`Outbound::send`].
//!
//! The rules every call is checked against are the SDK's (`singularr_sdk::http`): the allowlist
//! built from the calls each adapter declares, the access switches and the notification targets.
//! This module holds what sends: the reqwest transport and [`Outbound`]. `Outbound` checks each
//! call, sends it, logs it and counts it. The SDK's types keep their paths here.
//!
//! - Each integration is called by its profile (`HttpProfile`). A read that gets no connection,
//!   a broken one, a 429 or a 502 to 504 is sent again. It waits for the `Retry-After` the
//!   service sent or a random time. An act, a long call (one with a time limit of its own), a
//!   trial and a call of Singularr's own go once. A `Retry-After` defers every call to the
//!   integration.
//! - Calls at once and per period are capped. Lost connections, reads with no answer in time
//!   and 502 to 504 pause an integration's calls once they repeat. Each call counts once, on its
//!   final outcome. One trial call then ends the pause or starts another. A trial its caller
//!   drops lets the next call be the trial. An answer is read up to the profile's size.
//! - Redirects are never followed here; the caller decides, through this module again.
//! - Every call is logged with method, host, path, status and duration. The query string is
//!   not logged, and neither is the path of a target, which may be its secret.

use async_trait::async_trait;
use reqwest::Url;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub use singularr_sdk::error::{
    ConnectorError, OutboundError, Refusal, TIMED_OUT, TOO_LARGE, UNREACHABLE,
};
pub use singularr_sdk::http::{
    Access, AccessSwitch, ActCall, Allowlist, Body, BodyRule, BodyTest, Breaker, Calls, Class,
    Expect, GetCall, Grant, HostRule, HttpProfile, Method, OutRequest, OutResponse, PLEX_PIN_PATH,
    PLEX_TV_HOST, Part, QueryRule, QueryValue, Rate, ReadCall, Retry, TARGET_ADDRESS, TMDB_HOST,
    TVDB_HOST, TVMAZE_HOST, Target, Targets, Transport, multipart, retry_after,
};
/// The recorded transport of the SDK's test kit, at its old path.
pub use singularr_sdk::testkit as recorded;

/// A call that may be sent.
#[derive(Debug, Clone)]
pub struct Checked {
    pub url: Url,
    /// Who is called, for logs and metrics: the label of the host rule or of the target.
    pub label: String,
    /// The call passed as a POST to a notification target, not by a host rule.
    pub target: bool,
    /// What the call is: a read, an act, or a call of Singularr's own.
    pub class: Class,
}

struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// The request of a call: its method, time limit, headers and body.
    fn request(&self, url: Url, req: &OutRequest) -> reqwest::RequestBuilder {
        let method = match req.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self.client.request(method, url);
        if let Some(limit) = req.timeout {
            builder = builder.timeout(limit);
        }
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        builder = match &req.body {
            Body::None => builder,
            Body::Form(pairs) => {
                let encoded = pairs
                    .iter()
                    .map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v)))
                    .collect::<Vec<_>>()
                    .join("&");
                builder
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(encoded)
            }
            Body::Json(text) => builder
                .header("content-type", "application/json")
                .body(text.clone()),
            Body::Multipart(parts) => {
                let (kind, bytes) = multipart(parts);
                builder.header("content-type", kind).body(bytes)
            }
        };
        builder
    }
}

/// An answer's status and headers.
fn head_of(res: &reqwest::Response) -> (u16, Vec<(String, String)>) {
    let headers = res
        .headers()
        .iter()
        .filter_map(|(n, v)| Some((n.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    (res.status().as_u16(), headers)
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn send(&self, url: Url, req: &OutRequest) -> Result<OutResponse, String> {
        let res = self
            .request(url, req)
            .send()
            .await
            .map_err(|e| describe(&e))?;
        let (status, headers) = head_of(&res);
        // The body as reqwest gathered it, without a copy.
        let body = Vec::from(res.bytes().await.map_err(|e| describe(&e))?);
        Ok(OutResponse {
            status,
            headers,
            body,
        })
    }

    async fn send_into(
        &self,
        url: Url,
        req: &OutRequest,
        sink: &mut (dyn std::io::Write + Send),
    ) -> Result<OutResponse, String> {
        let mut res = self
            .request(url, req)
            .send()
            .await
            .map_err(|e| describe(&e))?;
        let (status, headers) = head_of(&res);
        if !(200..300).contains(&status) {
            let body = Vec::from(res.bytes().await.map_err(|e| describe(&e))?);
            return Ok(OutResponse {
                status,
                headers,
                body,
            });
        }
        while let Some(chunk) = res.chunk().await.map_err(|e| describe(&e))? {
            sink.write_all(&chunk).map_err(|e| e.to_string())?;
        }
        Ok(OutResponse {
            status,
            headers,
            body: Vec::new(),
        })
    }

    /// The answer read as it arrives. It is dropped once it passes `limit`.
    async fn send_capped(
        &self,
        url: Url,
        req: &OutRequest,
        limit: usize,
    ) -> Result<OutResponse, String> {
        let too_large = || format!("{TOO_LARGE}: more than {limit} bytes");
        let mut res = self
            .request(url, req)
            .send()
            .await
            .map_err(|e| describe(&e))?;
        if res.content_length().is_some_and(|n| n > limit as u64) {
            return Err(too_large());
        }
        let (status, headers) = head_of(&res);
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| describe(&e))? {
            if body.len() + chunk.len() > limit {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(OutResponse {
            status,
            headers,
            body,
        })
    }
}

/// A reqwest error without its URL (the URL can carry a query that is not for logs).
fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return TIMED_OUT.to_string();
    }
    if e.is_connect() {
        return UNREACHABLE.to_string();
    }
    let text = e.to_string();
    match e.url() {
        Some(url) => text.replace(url.as_str(), "(url)"),
        None => text,
    }
}

/// The value of an `Authorization` header for HTTP basic authentication (RFC 7617).
pub fn basic_auth(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        super::smtp::base64(format!("{user}:{password}").as_bytes())
    )
}

/// Percent-encode one query or form component.
pub fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Clone)]
pub struct Outbound {
    allow: Arc<Allowlist>,
    targets: Targets,
    transport: Arc<dyn Transport>,
    /// Each integration's guard, by the label it is called under.
    guards: Arc<Mutex<HashMap<String, Arc<Guard>>>>,
}

/// What one integration's profile keeps between calls: the calls at once, the next turn and the
/// breaker. The next turn is the rate's or the end of a `Retry-After`.
struct Guard {
    /// The label the integration is called under: what a pause names.
    label: String,
    profile: HttpProfile,
    permits: tokio::sync::Semaphore,
    state: Mutex<GuardState>,
}

#[derive(Default)]
struct GuardState {
    /// No call starts before then.
    next: Option<Instant>,
    /// Calls that failed in a row.
    failures: u16,
    /// Calls pause until then.
    open_until: Option<Instant>,
    /// The trial call after a pause is on its way.
    trial: bool,
    /// What the last failure said.
    cause: Option<String>,
}

/// What a call's final outcome is to the breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Count {
    /// No connection, a read with no answer in time, or a 502 to 504.
    Failed,
    /// The service answered.
    Answered,
    /// Neither: a connection that broke, an answer too large, an act with no answer in time, or
    /// another 5xx. The count stays as it is.
    Neither,
}

/// The trial call after a pause while it is on its way. Dropped before it is settled (its
/// caller gave up), it lets the next call be the trial.
struct Trial<'a> {
    guard: &'a Guard,
    settled: bool,
}

impl Drop for Trial<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.guard.state().trial = false;
        }
    }
}

impl Guard {
    fn new(label: &str, profile: HttpProfile) -> Guard {
        Guard {
            label: label.to_string(),
            profile,
            permits: tokio::sync::Semaphore::new(usize::from(profile.in_flight.max(1))),
            state: Mutex::new(GuardState::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, GuardState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How long the next call waits for its turn: the rate's spacing or a `Retry-After`.
    /// The turn is taken.
    fn turn(&self) -> Duration {
        let mut s = self.state();
        let now = Instant::now();
        let start = s.next.filter(|t| *t > now).unwrap_or(now);
        if let Some(rate) = self.profile.rate {
            s.next = Some(start + rate.per / rate.calls.max(1));
        }
        start.saturating_duration_since(now)
    }

    /// Whether a call may go: `Some` for the trial after a pause. A pause refuses it with the
    /// time it has left.
    fn admit(&self) -> Result<Option<Trial<'_>>, OutboundError> {
        let mut s = self.state();
        let now = Instant::now();
        let paused = |left: Duration, cause: &Option<String>| {
            tracing::warn!(target: "singularr::outbound", integration = %self.label, "paused after repeated failures");
            OutboundError::CircuitOpen {
                integration: self.label.clone(),
                seconds: left.as_secs().max(1),
                cause: cause.clone().unwrap_or_else(|| "no answer".to_string()),
            }
        };
        match s.open_until {
            Some(until) if now < until => Err(paused(until - now, &s.cause)),
            Some(_) if s.trial => Err(paused(Duration::ZERO, &s.cause)),
            Some(_) => {
                s.trial = true;
                Ok(Some(Trial {
                    guard: self,
                    settled: false,
                }))
            }
            None => Ok(None),
        }
    }

    /// A `Retry-After` defers the next turn.
    fn defer(&self, wait: Duration) {
        let mut s = self.state();
        let until = Instant::now() + wait;
        s.next = Some(s.next.map_or(until, |t| t.max(until)));
    }

    /// What a call's final outcome does: a failure counts toward a pause (a trial's starts one
    /// at once). An answer ends the count and the pause. Neither leaves them as they are.
    fn settle(&self, count: Count, cause: Option<String>, trial: Option<Trial<'_>>) {
        let was_trial = trial.is_some();
        if let Some(mut trial) = trial {
            trial.settled = true;
        }
        let mut s = self.state();
        if was_trial {
            s.trial = false;
        }
        match count {
            Count::Failed => {
                s.failures = s.failures.saturating_add(1);
                s.cause = cause;
                if was_trial || s.failures >= self.profile.breaker.failures {
                    s.open_until = Some(Instant::now() + self.profile.breaker.open_for);
                    metrics::counter!("singularr_outbound_paused_total").increment(1);
                }
            }
            Count::Answered => {
                s.failures = 0;
                s.open_until = None;
                s.cause = None;
            }
            Count::Neither => {}
        }
    }
}

/// A call's outcome for the breaker: how it counts, what it said when it failed, whether a read
/// may be sent again. Only a lost connection, a read with no answer in time and a 502 to 504 count
/// toward a pause. An act with no answer in time may have taken place: it counts as neither.
fn verdict(
    result: &Result<OutResponse, OutboundError>,
    class: Class,
    host: &str,
) -> (Count, Option<String>, bool) {
    match result {
        Ok(res) => match res.status {
            429 => (Count::Answered, None, true),
            502..=504 => (
                Count::Failed,
                Some(format!("{host} answered {}", res.status)),
                true,
            ),
            500..=599 => (Count::Neither, None, false),
            _ => (Count::Answered, None, false),
        },
        Err(e) => {
            let class_of = ConnectorError::from(e);
            let count = match class_of {
                ConnectorError::Unreachable { .. } => Count::Failed,
                ConnectorError::Timeout { .. } if class == Class::Read => Count::Failed,
                _ => Count::Neither,
            };
            let cause = (count == Count::Failed).then(|| e.to_string());
            (count, cause, class_of.retry_read())
        }
    }
}

/// The wait a 429 or a 502 to 504 asked for in its `Retry-After`.
fn asked_wait(result: &Result<OutResponse, OutboundError>) -> Option<Duration> {
    result
        .as_ref()
        .ok()
        .filter(|res| matches!(res.status, 429 | 502..=504))
        .and_then(|res| retry_after(res.header("retry-after")))
}

/// A number drawn evenly from 0 to 1, for the waits between attempts: a fresh one each time.
pub(crate) fn jitter() -> f64 {
    static SEED: AtomicU64 = AtomicU64::new(0);
    let mut x = SEED.load(Ordering::Relaxed);
    if x == 0 {
        x = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            | 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    SEED.store(x, Ordering::Relaxed);
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// One reqwest client: a connect timeout, a total timeout, no redirects, and a user agent that
/// names the program. With `proxies`, the environment's proxy (`HTTP_PROXY`, `ALL_PROXY` and
/// their lower-case forms) carries the calls.
fn reqwest_client(timeout: Duration, proxies: bool) -> Result<reqwest::Client, OutboundError> {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10).min(timeout))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("singularr/", env!("CARGO_PKG_VERSION")));
    if !proxies {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|e| OutboundError::Transport {
        host: "(client)".to_string(),
        message: e.to_string(),
    })
}

impl Outbound {
    /// The production client: one reqwest client, a connect timeout, a total timeout, no
    /// redirects, and a user agent that names the program.
    pub fn new(allow: Allowlist, timeout: Duration) -> Result<Self, OutboundError> {
        Ok(Outbound {
            allow: Arc::new(allow),
            targets: Targets::default(),
            transport: Arc::new(ReqwestTransport {
                client: reqwest_client(timeout, true)?,
            }),
            guards: Arc::default(),
        })
    }

    /// A client for one address of this machine and one GET there, past any proxy: a proxy
    /// in the environment (`HTTP_PROXY`, common beside a VPN container) would otherwise carry a
    /// call to 127.0.0.1. The address is an IP literal: loopback, or one this machine holds.
    /// One read-only rule lists it with the one GET. Every other path and host is refused.
    /// `singularrd health` asks its own server through it.
    pub fn direct(
        label: &str,
        base: &str,
        get: &str,
        timeout: Duration,
    ) -> Result<Self, OutboundError> {
        let url = Url::parse(base).map_err(|_| OutboundError::BadUrl(base.to_string()))?;
        let ip = url
            .host_str()
            .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
            .and_then(|host| host.parse::<std::net::IpAddr>().ok());
        // An address this machine holds is one a socket binds to.
        let local =
            ip.is_some_and(|ip| ip.is_loopback() || std::net::UdpSocket::bind((ip, 0)).is_ok());
        if !local {
            return Err(OutboundError::BadUrl(format!(
                "{base} (not an address of this machine)"
            )));
        }
        let mut allow = Allowlist::default();
        allow.allow_integration(label, base, Calls::default().get(get))?;
        Ok(Outbound {
            allow: Arc::new(allow),
            targets: Targets::default(),
            transport: Arc::new(ReqwestTransport {
                client: reqwest_client(timeout, false)?,
            }),
            guards: Arc::default(),
        })
    }

    /// A client over another transport (recorded answers in tests).
    pub fn with_transport(allow: Allowlist, transport: Arc<dyn Transport>) -> Self {
        Outbound {
            allow: Arc::new(allow),
            targets: Targets::default(),
            transport,
            guards: Arc::default(),
        }
    }

    pub fn allowlist(&self) -> &Allowlist {
        &self.allow
    }

    /// The addresses notification channels post to.
    pub fn targets(&self) -> &Targets {
        &self.targets
    }

    /// Who the host of this URL is listed as: the label of the rule a call there goes by (an
    /// integration, a metadata provider, one of Singularr's own services), else of a rule at its
    /// origin. `None` for an origin without a rule.
    pub fn listed(&self, url: &str) -> Option<String> {
        self.allow.listed(url)
    }

    /// The whole decision for one call, pure. A host with a rule is judged by its rule alone
    /// ([`Allowlist::check_call`]). A call to any other host passes only as a POST to the
    /// exact address of a target.
    pub fn check(&self, method: Method, url: &str, body: &Body) -> Result<Checked, OutboundError> {
        match self.allow.classify(method, url, body) {
            Ok((url, rule, class)) => Ok(Checked {
                url,
                label: rule.label.clone(),
                target: false,
                class,
            }),
            Err(OutboundError::HostNotAllowed { host }) => {
                let found = Url::parse(url).ok().and_then(|mut asked| {
                    asked.set_fragment(None);
                    self.targets
                        .find(method, &asked)
                        .map(|target| (asked, target))
                });
                match found {
                    Some((url, target)) => Ok(Checked {
                        url,
                        label: target.label,
                        target: true,
                        class: Class::Own,
                    }),
                    None => Err(OutboundError::HostNotAllowed { host }),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Check, send, log. A status outside 2xx is returned, not turned into an error; use
    /// [`Outbound::get_json`] when only a 2xx answer is useful. A read that fails as its
    /// profile says is sent again. An act is sent once. The call's final outcome counts once
    /// toward a pause of its integration.
    pub async fn send(&self, req: OutRequest) -> Result<OutResponse, OutboundError> {
        let checked = self.admit(&req)?;
        let guard = self.guard(&checked.label);
        // A notification's post and a call of Singularr's own count toward no pause. Their
        // callers have their own attempts.
        let breaks = checked.class != Class::Own;
        self.wait(guard.turn()).await;
        let trial = if breaks { guard.admit()? } else { None };
        // A read is sent again after a failure. A trial, a long call, an act and a call of
        // Singularr's own go once.
        let again = checked.class == Class::Read && req.timeout.is_none() && trial.is_none();
        let attempts = if again {
            u32::from(guard.profile.retry.attempts.max(1))
        } else {
            1
        };
        let host = checked.url.host_str().unwrap_or_default().to_string();
        let mut attempt = 0;
        loop {
            if attempt > 0 {
                self.wait(guard.turn()).await;
            }
            let permit = guard.permits.acquire().await.ok();
            let result = self
                .dispatch(&req, &checked, None, guard.profile.max_body, attempt)
                .await;
            drop(permit);
            let (count, cause, retry) = verdict(&result, checked.class, &host);
            let asked = asked_wait(&result);
            if let Some(wait) = asked {
                guard.defer(wait);
            }
            attempt += 1;
            if retry && attempt < attempts {
                metrics::counter!("singularr_outbound_retries_total", "target" => checked.label.clone())
                    .increment(1);
                // A `Retry-After` is the next turn. Otherwise a random wait.
                if asked.is_none() {
                    self.wait(guard.profile.retry.delay(attempt, jitter()))
                        .await;
                }
                continue;
            }
            if breaks {
                guard.settle(count, cause, trial);
            }
            return result;
        }
    }

    /// [`Outbound::send`] for a file too large to hold: the same check, guard, log and count,
    /// sent once. A 2xx answer's body is written into `sink` as it arrives. The answer comes back
    /// without it.
    pub async fn send_into(
        &self,
        req: OutRequest,
        sink: &mut (dyn std::io::Write + Send),
    ) -> Result<OutResponse, OutboundError> {
        let checked = self.admit(&req)?;
        let guard = self.guard(&checked.label);
        let breaks = checked.class != Class::Own;
        self.wait(guard.turn()).await;
        let trial = if breaks { guard.admit()? } else { None };
        let permit = guard.permits.acquire().await.ok();
        let result = self
            .dispatch(&req, &checked, Some(sink), usize::MAX, 0)
            .await;
        drop(permit);
        let host = checked.url.host_str().unwrap_or_default();
        let (count, cause, _) = verdict(&result, checked.class, host);
        if let Some(wait) = asked_wait(&result) {
            guard.defer(wait);
        }
        if breaks {
            guard.settle(count, cause, trial);
        }
        result
    }

    /// The guard of the integration listed under `label`, made with its profile at its first
    /// call.
    fn guard(&self, label: &str) -> Arc<Guard> {
        let mut guards = self.guards.lock().unwrap_or_else(PoisonError::into_inner);
        guards
            .entry(label.to_string())
            .or_insert_with(|| Arc::new(Guard::new(label, self.allow.profile_of(label))))
            .clone()
    }

    /// Wait this long before a call: slept through, or kept by a transport that keeps its
    /// waits (the recorded one).
    async fn wait(&self, wait: Duration) {
        if wait.is_zero() {
            return;
        }
        self.transport.waited(wait);
        if self.transport.sleeps() {
            tokio::time::sleep(wait).await;
        }
    }

    /// A call checked, or refused: a refusal is logged and counted.
    fn admit(&self, req: &OutRequest) -> Result<Checked, OutboundError> {
        self.check(req.method, &req.url, &req.body).inspect_err(|e| {
            tracing::warn!(target: "singularr::outbound", method = req.method.as_str(), error = %e, "refused");
            metrics::counter!("singularr_outbound_refused_total").increment(1);
        })
    }

    /// Send a checked call, then log and count what came back.
    async fn dispatch(
        &self,
        req: &OutRequest,
        checked: &Checked,
        sink: Option<&mut (dyn std::io::Write + Send)>,
        max_body: usize,
        attempt: u32,
    ) -> Result<OutResponse, OutboundError> {
        let host = checked.url.host_str().unwrap_or_default().to_string();
        let path = if checked.target {
            TARGET_ADDRESS.to_string()
        } else {
            checked.url.path().to_string()
        };
        let label = checked.label.clone();
        let started = Instant::now();
        let result = match sink {
            Some(sink) => {
                self.transport
                    .send_into(checked.url.clone(), req, sink)
                    .await
            }
            None => {
                self.transport
                    .send_capped(checked.url.clone(), req, max_body)
                    .await
            }
        };
        let ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(res) => {
                tracing::info!(
                    target: "singularr::outbound",
                    method = req.method.as_str(),
                    host = %host,
                    path = %path,
                    status = res.status,
                    ms,
                    attempt,
                    "{label}"
                );
                metrics::counter!(
                    "singularr_outbound_requests_total",
                    "target" => label,
                    "status" => res.status.to_string()
                )
                .increment(1);
                Ok(res)
            }
            Err(message) => {
                tracing::warn!(
                    target: "singularr::outbound",
                    method = req.method.as_str(),
                    host = %host,
                    path = %path,
                    ms,
                    attempt,
                    error = %message,
                    "{label}"
                );
                metrics::counter!(
                    "singularr_outbound_requests_total",
                    "target" => label,
                    "status" => "error"
                )
                .increment(1);
                if message.starts_with(TOO_LARGE) {
                    return Err(OutboundError::TooLarge {
                        host,
                        limit: max_body,
                    });
                }
                Err(OutboundError::Transport { host, message })
            }
        }
    }

    /// GET a URL and read a 2xx JSON answer.
    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<T, OutboundError> {
        let mut req = OutRequest::get(url);
        for (n, v) in headers {
            req = req.header(n, *v);
        }
        let res = self.send(req).await?;
        expect_ok(url, &res)?;
        res.json()
    }
}

/// Turn a status outside 2xx into an error that names the host and path.
pub fn expect_ok(url: &str, res: &OutResponse) -> Result<(), OutboundError> {
    if res.ok() {
        return Ok(());
    }
    let parsed = Url::parse(url).ok();
    Err(OutboundError::Status {
        host: parsed
            .as_ref()
            .and_then(|u| u.host_str())
            .unwrap_or_default()
            .to_string(),
        path: parsed.map(|u| u.path().to_string()).unwrap_or_default(),
        status: res.status,
    })
}

#[cfg(test)]
mod tests {
    use super::recorded::Recorded;
    use super::*;

    fn allow() -> Allowlist {
        let mut a = Allowlist::with_metadata_providers();
        a.allow_engine("radarr", "http://192.0.2.61:7979", &[])
            .unwrap();
        a.allow_engine("sonarr", "http://192.0.2.61:8989/", &[])
            .unwrap();
        a.allow_engine(
            "qbittorrent",
            "http://192.0.2.62:8080",
            &["/api/v2/auth/login"],
        )
        .unwrap();
        a
    }

    #[tokio::test]
    async fn a_refused_call_never_reaches_the_transport() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v3/system/status",
            200,
            r#"{"version":"5"}"#,
        );
        let out = Outbound::with_transport(allow(), recorded.clone());
        let err = out
            .send(OutRequest::post(
                "http://192.0.2.61:7979/api/v3/command",
                Body::Json(r#"{"name":"RescanMovie"}"#.into()),
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, OutboundError::NotListed { .. }));
        assert!(err.to_string().contains("not a listed call"));
        let err = out
            .send(OutRequest::get("https://example.org/x"))
            .await
            .unwrap_err();
        assert!(matches!(err, OutboundError::HostNotAllowed { .. }));
        assert!(recorded.seen.lock().unwrap().is_empty(), "nothing was sent");

        let v: serde_json::Value = out
            .get_json(
                "http://192.0.2.61:7979/api/v3/system/status",
                &[("X-Api-Key", "k")],
            )
            .await
            .unwrap();
        assert_eq!(v["version"], "5");
        assert_eq!(recorded.methods_seen(), vec![Method::Get]);
    }

    #[tokio::test]
    async fn a_streamed_answer_is_checked_like_any_call() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v3/system/status",
            200,
            r#"{"version":"5"}"#,
        );
        recorded.answer(Method::Get, "/api/v3/missing", 404, "gone");
        let out = Outbound::with_transport(allow(), recorded.clone());
        // Refused before a byte is sent, and nothing written.
        let mut sink = Vec::new();
        let err = out
            .send_into(OutRequest::get("https://example.org/x"), &mut sink)
            .await
            .unwrap_err();
        assert!(matches!(err, OutboundError::HostNotAllowed { .. }));
        assert!(sink.is_empty() && recorded.methods_seen().is_empty());
        // A 2xx answer's body goes to the sink and not into the answer; another answer keeps
        // its body and writes nothing.
        let res = out
            .send_into(
                OutRequest::get("http://192.0.2.61:7979/api/v3/system/status"),
                &mut sink,
            )
            .await
            .unwrap();
        assert_eq!((res.status, res.body.is_empty()), (200, true));
        assert_eq!(sink, br#"{"version":"5"}"#);
        let mut none = Vec::new();
        let missing = out
            .send_into(
                OutRequest::get("http://192.0.2.61:7979/api/v3/missing"),
                &mut none,
            )
            .await
            .unwrap();
        assert_eq!(
            (missing.status, missing.body.as_slice()),
            (404, &b"gone"[..])
        );
        assert!(none.is_empty());
    }

    #[test]
    fn components_are_percent_encoded() {
        assert_eq!(encode_component("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(encode_component("Movie.2004-GRP"), "Movie.2004-GRP");
    }

    #[test]
    fn basic_authentication_is_the_user_and_password_in_base64() {
        assert_eq!(basic_auth("fxuser", "fxpass"), "Basic Znh1c2VyOmZ4cGFzcw==");
    }

    const WORKER: &str = "http://10.0.0.60:8265";
    const BRIDGE: &str = "http://10.0.0.60:8787";

    /// A worker whose records are asked with POST, and a bridge that takes one act.
    fn integrations() -> Allowlist {
        let mut a = allow();
        a.allow_integration(
            "worker",
            WORKER,
            Calls::default()
                .read_when(
                    Method::Post,
                    "/api/v2/cruddb",
                    "/data/mode",
                    &["getAll", "getById"],
                )
                .read(Method::Post, "/api/v2/client/status-tables"),
        )
        .unwrap();
        a.allow_integration(
            "bridge",
            BRIDGE,
            Calls::default().act("send", Method::Post, "/stage"),
        )
        .unwrap();
        a
    }

    fn json(text: &str) -> Body {
        Body::Json(text.to_string())
    }

    #[tokio::test]
    async fn the_transport_is_reached_by_reads_and_by_allowed_acts_only() {
        let recorded = Recorded::new();
        recorded.answer_body(Method::Post, "/api/v2/cruddb", "Flows", 200, r#"["flows"]"#);
        recorded.answer_body(
            Method::Post,
            "/api/v2/cruddb",
            "Library",
            200,
            r#"["libraries"]"#,
        );
        recorded.answer(Method::Post, "/api/v2/cruddb", 200, "");
        recorded.answer(Method::Post, "/stage", 201, r#"{"staged": true}"#);
        let allow = integrations();
        let switch = allow.switch("bridge").unwrap();
        let out = Outbound::with_transport(allow, recorded.clone());
        let ask = |collection: &str, mode: &str| {
            OutRequest::post(
                format!("{WORKER}/api/v2/cruddb"),
                json(&format!(
                    r#"{{"data": {{"collection": "{collection}", "mode": "{mode}"}}}}"#
                )),
            )
        };
        // Each question gets the answer recorded for its body, else the one for any body.
        let libraries = out.send(ask("LibrarySettingsJSONDB", "getAll")).await;
        assert_eq!(libraries.unwrap().text(), r#"["libraries"]"#);
        let flows = out.send(ask("FlowsJSONDB", "getAll")).await.unwrap();
        assert_eq!(flows.text(), r#"["flows"]"#);
        let other = out.send(ask("FileJSONDB", "getById")).await.unwrap();
        assert!(other.ok() && other.body.is_empty());
        let refused = out.send(ask("FileJSONDB", "removeOne")).await;
        assert!(matches!(refused, Err(OutboundError::NotListed { .. })));
        let stage = || {
            OutRequest::post(
                format!("{BRIDGE}/stage"),
                json(r#"{"path": "/library/a.mkv", "flow": "convert"}"#),
            )
            .header("Authorization", "Bearer k")
        };
        let refused = out.send(stage()).await;
        assert!(matches!(refused, Err(OutboundError::NotAllowed { .. })));
        assert_eq!(recorded.methods_seen(), vec![Method::Post; 3], "the reads");
        switch.set(Access::Act);
        assert_eq!(out.send(stage()).await.unwrap().status, 201);
        // What was sent is on record with its headers and its body.
        let sent = recorded.calls_sent();
        assert_eq!(sent.len(), 4);
        let last = &sent[3];
        assert_eq!(
            (last.method, last.url.as_str()),
            (Method::Post, "http://10.0.0.60:8787/stage")
        );
        assert_eq!(
            last.headers,
            vec![("Authorization".to_string(), "Bearer k".to_string())]
        );
        assert!(matches!(&last.body, Body::Json(text) if text.contains("convert")));
    }

    #[tokio::test]
    async fn a_call_that_gets_no_answer_in_time_says_so() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/api/v3/movie", 200, "[]");
        // The answer recorded last for a call is the one it gets: here, none.
        recorded.fail(Method::Get, "/api/v3/movie", TIMED_OUT);
        recorded.fail(
            Method::Get,
            "/api/v3/tag",
            "connection refused or unreachable",
        );
        let out = Outbound::with_transport(allow(), recorded.clone());
        let at = |path: &str| OutRequest::get(format!("http://192.0.2.61:7979/api/v3{path}"));
        let late = out.send(at("/movie")).await.unwrap_err();
        assert!(late.timed_out(), "{late}");
        assert_eq!(late.to_string(), "192.0.2.61 did not answer: timed out");
        let down = out.send(at("/tag")).await.unwrap_err();
        assert!(matches!(down, OutboundError::Transport { .. }) && !down.timed_out());
        // The read that reached no one went three times in all. The one that timed out went once.
        // A call that never left is no late answer.
        let refused = out
            .send(OutRequest::post(
                "http://192.0.2.61:7979/api/v3/movie",
                Body::None,
            ))
            .await
            .unwrap_err();
        assert!(matches!(refused, OutboundError::NotListed { .. }) && !refused.timed_out());
        assert_eq!(recorded.methods_seen(), vec![Method::Get; 4]);
    }

    const HOOK: &str = "https://hooks.example.com/services/T000/B000/secret-token";

    /// A notification channel's address: POST to exactly that address passes, and nothing
    /// else on its host does.
    #[tokio::test]
    async fn a_target_takes_post_at_its_own_address_and_nothing_else() {
        let recorded = Recorded::new();
        recorded.answer(Method::Post, "hooks.example.com", 200, "ok");
        let out = Outbound::with_transport(allow(), recorded.clone());
        let post = |url: &str| out.check(Method::Post, url, &Body::Json("{}".into()));
        // Before it is listed its host is refused like any other.
        assert!(matches!(
            post(HOOK),
            Err(OutboundError::HostNotAllowed { .. })
        ));
        out.targets()
            .put("channel:1", Target::new("notify:slack", HOOK).unwrap());
        let passed = post(HOOK).unwrap();
        assert!(passed.target);
        assert_eq!(passed.label, "notify:slack");
        // A fragment is no part of an address.
        assert!(post(&format!("{HOOK}#section")).is_ok());
        // Another path, a path below it, another query, the root, another scheme, another
        // port, another host: all refused.
        for url in [
            "https://hooks.example.com/services/T000/B000/other",
            "https://hooks.example.com/services/T000/B000/secret-token/x",
            "https://hooks.example.com/services/T000/B000/secret-token?wait=true",
            "https://hooks.example.com/",
            "http://hooks.example.com/services/T000/B000/secret-token",
            "https://hooks.example.com:8443/services/T000/B000/secret-token",
            "https://example.com/services/T000/B000/secret-token",
        ] {
            assert!(
                matches!(post(url), Err(OutboundError::HostNotAllowed { .. })),
                "{url}"
            );
        }
        // POST alone: not even a GET of the address passes.
        for method in [Method::Get, Method::Put, Method::Delete] {
            assert!(
                matches!(
                    out.check(method, HOOK, &Body::None),
                    Err(OutboundError::HostNotAllowed { .. })
                ),
                "{}",
                method.as_str()
            );
        }
        // A host with a rule is judged by its rule alone: a target there opens nothing.
        let command = "http://192.0.2.61:7979/api/v3/command";
        out.targets()
            .put("channel:2", Target::new("notify:webhook", command).unwrap());
        assert!(matches!(
            post(command),
            Err(OutboundError::NotListed { .. })
        ));
        assert_eq!(
            out.listed("http://192.0.2.61:7979/anything").as_deref(),
            Some("radarr")
        );
        assert_eq!(out.listed(HOOK), None);

        // The call is sent, and every clone shares the list.
        let sent = out
            .send(OutRequest::post(HOOK, Body::Json("{}".into())))
            .await
            .unwrap();
        assert_eq!(sent.status, 200);
        assert_eq!(recorded.urls_seen(), vec![HOOK.to_string()]);
        assert_eq!(out.targets().list().len(), 2);
        out.clone().targets().remove("channel:1");
        assert!(matches!(
            post(HOOK),
            Err(OutboundError::HostNotAllowed { .. })
        ));
        let refused = out
            .send(OutRequest::post(HOOK, Body::Json("{}".into())))
            .await
            .unwrap_err();
        assert!(!refused.to_string().contains("secret-token"), "{refused}");
        assert_eq!(recorded.urls_seen().len(), 1, "a refused call is not sent");

        // Neither the Debug form of a target nor the error of a bad one repeats the address.
        let shown = format!("{:?}", Target::new("notify:slack", HOOK).unwrap());
        assert!(
            shown.contains("hooks.example.com") && !shown.contains("secret-token"),
            "{shown}"
        );
        for bad in ["ftp://example.com/x", "not a url", "file:///etc/passwd", ""] {
            assert_eq!(
                Target::new("notify:webhook", bad).unwrap_err(),
                OutboundError::BadUrl(TARGET_ADDRESS.to_string()),
                "{bad}"
            );
        }
    }

    /// A profile with short pauses, for the tests of the sender.
    fn quick(attempts: u8) -> HttpProfile {
        HttpProfile {
            retry: Retry {
                attempts,
                base: Duration::from_millis(1),
                cap: Duration::from_millis(4),
            },
            breaker: Breaker {
                failures: 5,
                open_for: Duration::from_millis(150),
            },
            ..HttpProfile::DEFAULT
        }
    }

    const RADARR: &str = "http://192.0.2.61:7979/api/v3";

    /// A read that got a 503 is sent again and its second answer comes back. An act that got
    /// one comes back as it is: it was sent once. So was an act that reached no one.
    /// So was a long call.
    #[tokio::test]
    async fn a_read_is_sent_again_after_a_503_and_an_act_never() {
        let recorded = Recorded::new();
        recorded.answer_once(Method::Get, "/api/v3/movie", 503, "busy", &[]);
        recorded.answer(Method::Get, "/api/v3/movie", 200, "[]");
        recorded.answer(Method::Post, "/stage", 503, "busy");
        recorded.answer(Method::Get, "/api/v3/release", 503, "busy");
        let mut allow = integrations();
        allow.set_profile("radarr", quick(3));
        allow.set_profile("bridge", quick(3));
        let switch = allow.switch("bridge").unwrap();
        let out = Outbound::with_transport(allow, recorded.clone());
        let read = out
            .send(OutRequest::get(format!("{RADARR}/movie")))
            .await
            .unwrap();
        assert_eq!((read.status, read.text().as_str()), (200, "[]"));
        assert_eq!(recorded.urls_seen().len(), 2, "the read went twice");
        assert_eq!(recorded.waits().len(), 1, "with one wait between");

        switch.set(Access::Act);
        let stage = || OutRequest::post(format!("{BRIDGE}/stage"), json("{}"));
        assert_eq!(out.send(stage()).await.unwrap().status, 503);
        recorded.fail_once(Method::Post, "/stage", UNREACHABLE);
        assert!(out.send(stage()).await.is_err());
        let posts = recorded
            .methods_seen()
            .into_iter()
            .filter(|m| *m == Method::Post)
            .count();
        assert_eq!(posts, 2, "each act went once");

        let search = OutRequest::get(format!("{RADARR}/release?movieId=1"))
            .timeout(Duration::from_secs(150));
        assert_eq!(out.send(search).await.unwrap().status, 503);
        let searches = recorded
            .urls_seen()
            .iter()
            .filter(|u| u.contains("/release"))
            .count();
        assert_eq!(searches, 1, "a long call goes once");
    }

    /// A 429 defers the next call by its `Retry-After`: the read waits it out and is answered.
    /// Every call to the host waits for the same turn.
    #[tokio::test]
    async fn a_429_defers_the_calls_by_its_retry_after() {
        let recorded = Recorded::new();
        recorded.answer_once(
            Method::Get,
            "/api/v3/queue",
            429,
            "slow down",
            &[("retry-after", "7")],
        );
        recorded.answer(Method::Get, "/api/v3/queue", 200, "[]");
        let mut allow = allow();
        allow.set_profile("radarr", quick(3));
        let out = Outbound::with_transport(allow, recorded.clone());
        let res = out
            .send(OutRequest::get(format!("{RADARR}/queue")))
            .await
            .unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(recorded.urls_seen().len(), 2);
        let waits = recorded.waits();
        assert_eq!(waits.len(), 1, "{waits:?}");
        assert!(
            waits[0] > Duration::from_secs(6) && waits[0] <= Duration::from_secs(7),
            "{waits:?}"
        );
    }

    /// Five calls that fail in a row pause the host: the sixth is refused before anything is
    /// sent. After the pause one trial goes. Its success ends the pause.
    #[tokio::test]
    async fn five_failures_pause_the_calls_and_a_trial_ends_the_pause() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/api/v3/tag", 503, "busy");
        let mut allow = allow();
        allow.set_profile("radarr", quick(1));
        let out = Outbound::with_transport(allow, recorded.clone());
        let tag = || OutRequest::get(format!("{RADARR}/tag"));
        for _ in 0..5 {
            assert_eq!(out.send(tag()).await.unwrap().status, 503);
        }
        let paused = out.send(tag()).await.unwrap_err();
        assert!(
            matches!(paused, OutboundError::CircuitOpen { .. }),
            "{paused:?}"
        );
        assert_eq!(recorded.urls_seen().len(), 5, "the sixth was not sent");
        assert_eq!(paused.taxonomy().class(), "circuit_open");
        assert_eq!(
            paused.to_string(),
            "radarr is paused for 1 s after repeated failures: 192.0.2.61 answered 503"
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
        recorded.answer(Method::Get, "/api/v3/tag", 200, "[]");
        assert_eq!(out.send(tag()).await.unwrap().status, 200, "the trial");
        assert_eq!(
            out.send(tag()).await.unwrap().status,
            200,
            "the pause is over"
        );
        assert_eq!(recorded.urls_seen().len(), 7);

        // A trial that fails starts another pause at once.
        recorded.answer(Method::Get, "/api/v3/tag", 503, "busy");
        for _ in 0..5 {
            let _ = out.send(tag()).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            out.send(tag()).await.unwrap().status,
            503,
            "the trial failed"
        );
        assert!(matches!(
            out.send(tag()).await,
            Err(OutboundError::CircuitOpen { .. })
        ));
    }

    /// A command a service takes on GET is an act: refused while the access is `read`, sent
    /// while it is `act`. The GETs that read go whatever the access.
    #[tokio::test]
    async fn a_get_act_is_refused_under_read_and_sent_under_act() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/library/sections", 200, "{}");
        let mut allow = Allowlist::default();
        allow
            .allow_integration(
                "plex",
                "http://192.0.2.40:32400",
                Calls::default().get("/library/sections").act(
                    "scan",
                    Method::Get,
                    "/library/sections/{id}/refresh",
                ),
            )
            .unwrap();
        let switch = allow.switch("plex").unwrap();
        let out = Outbound::with_transport(allow, recorded.clone());
        let scan = || OutRequest::get("http://192.0.2.40:32400/library/sections/3/refresh");
        assert!(matches!(
            out.send(scan()).await,
            Err(OutboundError::NotAllowed { .. })
        ));
        assert!(recorded.urls_seen().is_empty(), "nothing was sent");
        let read = OutRequest::get("http://192.0.2.40:32400/library/sections");
        assert_eq!(out.send(read).await.unwrap().status, 200);
        switch.set(Access::Act);
        assert_eq!(out.send(scan()).await.unwrap().status, 200);
        assert_eq!(recorded.urls_seen().len(), 2);
    }

    /// An answer larger than the profile allows fails. It is not sent again.
    #[tokio::test]
    async fn an_answer_larger_than_the_profile_allows_fails() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/api/v3/movie", 200, "0123456789abcdef");
        let mut allow = allow();
        allow.set_profile(
            "radarr",
            HttpProfile {
                max_body: 8,
                ..quick(3)
            },
        );
        let out = Outbound::with_transport(allow, recorded.clone());
        let large = out
            .send(OutRequest::get(format!("{RADARR}/movie")))
            .await
            .unwrap_err();
        assert!(
            matches!(large, OutboundError::TooLarge { limit: 8, .. }),
            "{large:?}"
        );
        assert_eq!(recorded.urls_seen().len(), 1);
    }

    /// Answers 503 to the first call. The second gets no answer. The rest get 200.
    struct Stalls(std::sync::atomic::AtomicUsize);

    #[async_trait]
    impl Transport for Stalls {
        async fn send(&self, _url: Url, _req: &OutRequest) -> Result<OutResponse, String> {
            let answer = |status: u16| OutResponse {
                status,
                headers: Vec::new(),
                body: Vec::new(),
            };
            match self.0.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(answer(503)),
                1 => std::future::pending().await,
                _ => Ok(answer(200)),
            }
        }
    }

    /// A trial whose caller gives up before its answer does not hold the pause: the next call
    /// is the trial. It is sent.
    #[tokio::test]
    async fn a_trial_its_caller_dropped_lets_the_next_call_be_the_trial() {
        let mut allow = allow();
        allow.set_profile(
            "radarr",
            HttpProfile {
                breaker: Breaker {
                    failures: 1,
                    open_for: Duration::from_millis(30),
                },
                ..quick(1)
            },
        );
        let stalls = Arc::new(Stalls(std::sync::atomic::AtomicUsize::new(0)));
        let out = Outbound::with_transport(allow, stalls.clone());
        let tag = || OutRequest::get(format!("{RADARR}/tag"));
        assert_eq!(out.send(tag()).await.unwrap().status, 503);
        assert!(matches!(
            out.send(tag()).await,
            Err(OutboundError::CircuitOpen { .. })
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let dropped = tokio::time::timeout(Duration::from_millis(50), out.send(tag())).await;
        assert!(dropped.is_err(), "the trial got no answer and was dropped");
        assert_eq!(out.send(tag()).await.unwrap().status, 200);
        assert_eq!(stalls.0.load(Ordering::SeqCst), 3);
    }

    /// Only a lost connection, a read with no answer in time and a 502 to 504 count toward a
    /// pause. A 500, a broken connection, an answer too large and an act with no answer in time
    /// leave the count as it is.
    #[tokio::test]
    async fn only_lost_connections_read_timeouts_and_502_to_504_count_toward_a_pause() {
        let recorded = Recorded::new();
        recorded.answer_once(Method::Get, "/api/v3/tag", 503, "", &[]);
        recorded.answer_once(Method::Get, "/api/v3/tag", 500, "", &[]);
        recorded.answer_once(Method::Get, "/api/v3/tag", 501, "", &[]);
        recorded.answer_once(Method::Get, "/api/v3/tag", 200, "0123456789abcdef", &[]);
        recorded.fail_once(Method::Get, "/api/v3/tag", "connection reset by peer");
        recorded.fail_once(Method::Get, "/api/v3/tag", UNREACHABLE);
        recorded.answer(Method::Get, "/api/v3/tag", 200, "[]");
        let mut allow = integrations();
        let counted = HttpProfile {
            max_body: 8,
            breaker: Breaker {
                failures: 2,
                open_for: Duration::from_secs(60),
            },
            ..quick(1)
        };
        allow.set_profile("radarr", counted);
        allow.set_profile("bridge", counted);
        let switch = allow.switch("bridge").unwrap();
        let out = Outbound::with_transport(allow, recorded.clone());
        let tag = || OutRequest::get(format!("{RADARR}/tag"));
        assert_eq!(out.send(tag()).await.unwrap().status, 503, "one");
        assert_eq!(out.send(tag()).await.unwrap().status, 500);
        assert_eq!(out.send(tag()).await.unwrap().status, 501);
        assert!(matches!(
            out.send(tag()).await,
            Err(OutboundError::TooLarge { .. })
        ));
        assert!(out.send(tag()).await.is_err(), "a broken connection");
        assert!(out.send(tag()).await.is_err(), "two: no connection");
        let paused = out.send(tag()).await.unwrap_err();
        let said = paused.to_string();
        assert!(
            said.starts_with("radarr is paused for ") && said.contains("did not answer"),
            "{said}"
        );
        assert_eq!(
            recorded.urls_seen().len(),
            6,
            "the paused call was not sent"
        );

        // An act that gets no answer in time may have taken place: it counts as neither.
        switch.set(Access::Act);
        let stage = || OutRequest::post(format!("{BRIDGE}/stage"), json("{}"));
        recorded.fail_once(Method::Post, "/stage", TIMED_OUT);
        recorded.fail_once(Method::Post, "/stage", TIMED_OUT);
        recorded.answer(Method::Post, "/stage", 201, "");
        assert!(out.send(stage()).await.unwrap_err().timed_out());
        assert!(out.send(stage()).await.unwrap_err().timed_out());
        assert_eq!(out.send(stage()).await.unwrap().status, 201);
    }

    /// The answer of the health probe is asked once, within the probe's own time limit.
    #[tokio::test]
    async fn the_health_probe_is_sent_once() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/readyz", 503, "not ready");
        let mut allow = Allowlist::default();
        allow
            .allow_integration(
                "singularrd",
                "http://127.0.0.1:3000",
                Calls::default().get("/readyz"),
            )
            .unwrap();
        let out = Outbound::with_transport(allow, recorded.clone());
        let probe = OutRequest::get("http://127.0.0.1:3000/readyz").timeout(Duration::from_secs(4));
        assert_eq!(out.send(probe).await.unwrap().status, 503);
        assert_eq!(recorded.urls_seen().len(), 1);
    }
}
