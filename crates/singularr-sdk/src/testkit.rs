//! A transport that answers from a table and remembers what it was asked. For tests in every
//! layer: adapters are exercised against recorded JSON, through the real allowlist check.

use crate::http::{Body, Method, OutRequest, OutResponse, Transport};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use url::Url;

/// One recorded answer, and the calls it is for.
struct Answer {
    method: Method,
    url_part: String,
    /// Text the body of the call holds, for calls that differ by their body alone.
    body_part: Option<String>,
    /// The answer, or what the transport says of a call that gets none.
    response: Result<OutResponse, String>,
}

#[derive(Default)]
pub struct Recorded {
    answers: Mutex<Vec<Answer>>,
    /// Answers given once each, in the order they were recorded, before any standing one.
    once: Mutex<Vec<Answer>>,
    pub seen: Mutex<Vec<(Method, String)>>,
    /// Every call as it was sent, in order.
    pub sent: Mutex<Vec<OutRequest>>,
    /// The waits the sender took, in order. None of them is slept through.
    pub waits: Mutex<Vec<std::time::Duration>>,
}

/// The body of a call as text, to find its answer by.
fn body_text(body: &Body) -> String {
    match body {
        Body::None => String::new(),
        Body::Json(text) => text.clone(),
        Body::Form(pairs) => pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&"),
        Body::Multipart(parts) => parts
            .iter()
            .map(|p| p.name.clone())
            .collect::<Vec<_>>()
            .join("&"),
    }
}

impl Recorded {
    pub fn new() -> Arc<Self> {
        Arc::new(Recorded::default())
    }

    /// Answer `method` on a URL that contains `url_part` with this status and body.
    pub fn answer(&self, method: Method, url_part: &str, status: u16, body: &str) {
        self.answer_with(method, url_part, status, body, &[]);
    }

    pub fn answer_with(
        &self,
        method: Method,
        url_part: &str,
        status: u16,
        body: &str,
        headers: &[(&str, &str)],
    ) {
        self.record(method, url_part, None, status, body, headers);
    }

    /// Answer `method` on a URL that contains `url_part` when the body of the call holds
    /// `body_part`: for a service that takes its questions in the body of a POST.
    pub fn answer_body(
        &self,
        method: Method,
        url_part: &str,
        body_part: &str,
        status: u16,
        body: &str,
    ) {
        self.record(method, url_part, Some(body_part), status, body, &[]);
    }

    /// [`Recorded::answer_body`] with headers: a sign-in a service takes in the body of a
    /// POST, answered with a cookie.
    pub fn answer_body_with(
        &self,
        method: Method,
        url_part: &str,
        body_part: &str,
        status: u16,
        body: &str,
        headers: &[(&str, &str)],
    ) {
        self.record(method, url_part, Some(body_part), status, body, headers);
    }

    /// Answer `method` on a URL that contains `url_part` with bytes that are no text: a
    /// compressed file.
    pub fn answer_bytes(&self, method: Method, url_part: &str, status: u16, body: &[u8]) {
        self.answers.lock().unwrap().push(Answer {
            method,
            url_part: url_part.to_string(),
            body_part: None,
            response: Ok(OutResponse {
                status,
                headers: Vec::new(),
                body: body.to_vec(),
            }),
        });
    }

    fn record(
        &self,
        method: Method,
        url_part: &str,
        body_part: Option<&str>,
        status: u16,
        body: &str,
        headers: &[(&str, &str)],
    ) {
        self.answers.lock().unwrap().push(Answer {
            method,
            url_part: url_part.to_string(),
            body_part: body_part.map(str::to_string),
            response: Ok(OutResponse {
                status,
                headers: headers
                    .iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect(),
                body: body.as_bytes().to_vec(),
            }),
        });
    }

    /// Give `method` on a URL that contains `url_part` no answer: the call fails in the
    /// words a transport says it with ([`crate::error::TIMED_OUT`], or any other).
    pub fn fail(&self, method: Method, url_part: &str, words: &str) {
        self.answers.lock().unwrap().push(Answer {
            method,
            url_part: url_part.to_string(),
            body_part: None,
            response: Err(words.to_string()),
        });
    }

    /// Answer the next call of `method` on a URL that contains `url_part` once, with this
    /// status, body and headers: before any standing answer, in the order recorded.
    pub fn answer_once(
        &self,
        method: Method,
        url_part: &str,
        status: u16,
        body: &str,
        headers: &[(&str, &str)],
    ) {
        self.once.lock().unwrap().push(Answer {
            method,
            url_part: url_part.to_string(),
            body_part: None,
            response: Ok(OutResponse {
                status,
                headers: headers
                    .iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect(),
                body: body.as_bytes().to_vec(),
            }),
        });
    }

    /// Give the next call of `method` on a URL that contains `url_part` no answer, once.
    pub fn fail_once(&self, method: Method, url_part: &str, words: &str) {
        self.once.lock().unwrap().push(Answer {
            method,
            url_part: url_part.to_string(),
            body_part: None,
            response: Err(words.to_string()),
        });
    }

    /// The waits the sender took before its calls, in order.
    pub fn waits(&self) -> Vec<std::time::Duration> {
        self.waits.lock().unwrap().clone()
    }

    pub fn methods_seen(&self) -> Vec<Method> {
        self.seen.lock().unwrap().iter().map(|(m, _)| *m).collect()
    }

    pub fn urls_seen(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, u)| u.clone())
            .collect()
    }

    /// Every call as it was sent, in order: its method, address, headers and body.
    pub fn calls_sent(&self) -> Vec<OutRequest> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl Transport for Recorded {
    fn sleeps(&self) -> bool {
        false
    }

    fn waited(&self, wait: std::time::Duration) {
        self.waits.lock().unwrap().push(wait);
    }

    async fn send(&self, url: Url, req: &OutRequest) -> Result<OutResponse, String> {
        self.seen
            .lock()
            .unwrap()
            .push((req.method, url.to_string()));
        self.sent.lock().unwrap().push(OutRequest {
            url: url.to_string(),
            ..req.clone()
        });
        let text = body_text(&req.body);
        {
            let mut once = self.once.lock().unwrap();
            if let Some(at) = once
                .iter()
                .position(|a| a.method == req.method && url.as_str().contains(a.url_part.as_str()))
            {
                return once.remove(at).response;
            }
        }
        let answers = self.answers.lock().unwrap();
        // The longest matching part wins, so `/movie` and `/movie/7` can both be listed;
        // then an answer for the body of the call over one for any body; then the answer
        // recorded last.
        answers
            .iter()
            .filter(|a| a.method == req.method && url.as_str().contains(a.url_part.as_str()))
            .filter(|a| {
                a.body_part
                    .as_deref()
                    .is_none_or(|part| text.contains(part))
            })
            .max_by_key(|a| {
                (
                    a.url_part.len(),
                    a.body_part.as_deref().map(str::len).unwrap_or(0),
                )
            })
            .map(|a| a.response.clone())
            .unwrap_or_else(|| {
                Err(format!(
                    "no recorded answer for {} {}",
                    req.method.as_str(),
                    url
                ))
            })
    }
}

/// Whether a manifest states its facts in the forms the app reads them in: an id, names and
/// words that are not empty, variable prefixes the environment can hold, an address with its
/// scheme, ids that are unique, and passes for a type that is read and none for one that is not.
pub fn check_manifest(m: &crate::manifest::Manifest) -> Result<(), String> {
    let id = m.type_id;
    let lower = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if !(1..=32).contains(&id.len()) || id.starts_with('-') || !lower(id) {
        return Err(format!(
            "the type id `{id}` is not 1 to 32 of a-z, 0-9 and -"
        ));
    }
    let words = [
        ("name", m.name),
        ("label", m.label),
        ("role", m.role),
        ("description", m.description),
        ("secret key", m.secret.key),
        ("secret label", m.secret.label),
    ];
    let reads = m.reads.iter().map(|r| ("read", *r));
    let actions = m
        .actions
        .iter()
        .flat_map(|a| [("action id", a.id), ("action label", a.label)]);
    let passes = m.passes.iter().flat_map(|p| {
        [
            ("pass field", p.field),
            ("pass label", p.label),
            ("pass setting", p.setting),
        ]
    });
    let counts = m.counts.iter().flat_map(|c| {
        [
            ("count key", c.key),
            ("count word", c.one),
            ("count word", c.many),
        ]
    });
    for (what, text) in words
        .into_iter()
        .chain(reads)
        .chain(actions)
        .chain(passes)
        .chain(counts)
    {
        if text.is_empty() || text.trim() != text {
            return Err(format!(
                "a {what} is empty or starts or ends with a space: {text:?}"
            ));
        }
        if text.ends_with('.') {
            return Err(format!("a {what} ends with a period: {text:?}"));
        }
    }
    let upper = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    };
    if m.env_prefixes.is_empty() || !m.env_prefixes.iter().all(|p| upper(p)) {
        return Err(format!(
            "the variable prefixes {:?} are not A-Z, 0-9 and _",
            m.env_prefixes
        ));
    }
    if !upper(m.secret.env_suffix) {
        return Err(format!(
            "the secret's variable ending {:?} is not A-Z, 0-9 and _",
            m.secret.env_suffix
        ));
    }
    if !(m.default_address.starts_with("http://") || m.default_address.starts_with("https://")) {
        return Err(format!(
            "the address {:?} has no http:// or https://",
            m.default_address
        ));
    }
    if m.kinds.is_empty() {
        return Err("the type fills no kind".to_string());
    }
    fn twice(keys: Vec<&'static str>) -> Option<&'static str> {
        let mut seen = std::collections::BTreeSet::new();
        keys.into_iter().find(|k| !seen.insert(*k))
    }
    for (what, twice) in [
        ("action", twice(m.actions.iter().map(|a| a.id).collect())),
        ("pass", twice(m.passes.iter().map(|p| p.field).collect())),
        ("count", twice(m.counts.iter().map(|c| c.key).collect())),
    ] {
        if let Some(key) = twice {
            return Err(format!("the {what} `{key}` is listed twice"));
        }
    }
    match (m.read(), m.passes.is_empty()) {
        (true, true) => Err("the type is read and has no pass".to_string()),
        (false, false) => Err("the type is not read and has passes".to_string()),
        _ => Ok(()),
    }
}

/// The check of declared calls, at its place in the test kit.
pub use crate::http::check_calls;
