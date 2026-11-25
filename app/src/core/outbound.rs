//! The one way out. Every HTTP call Singularr makes goes through [`Outbound::send`].
//!
//! - A call to a host that is not listed is refused before anything is sent.
//! - A host rule is listed by its address: the origin, and the path below which the integration
//!   answers. A call goes by the rule whose path holds its own, whole segments, the longest such
//!   path first: a media server at `https://host/` and a manager at `https://host/radarr` keep
//!   a rule each. A call at a listed origin that no rule's path holds is refused.
//! - Toward an integration a host rule knows three things. GET passes, unless the adapter
//!   declares its GETs (`gets`): then only those pass, each an exact path, with a condition on
//!   one value of the query where the path alone does not say that the call reads (Tautulli
//!   takes every command at one path). Where a key beside the command turns a read into a
//!   change, the condition also names the keys the query may hold (SABnzbd). A service that
//!   starts work on some GETs (a media server starts a transcode, a scan or a download on GET)
//!   declares the paths it reads. An adapter whose service takes commands on GET and reads
//!   with POST alone takes no GET at all (`no_get`, NZBGet). Its `reads` pass
//!   always: calls that are not GET and only read (a sign-in, a query a service takes with POST),
//!   each a method, a path and, where the path alone does not say that the call only reads, a
//!   condition on the JSON body. Its `acts` pass only while the integration's access is `act`
//!   ([`AccessSwitch`]). Everything else is refused, whatever the access: acting never turns a
//!   host into a free one. The refusal says which it was ([`OutboundError::NotListed`],
//!   [`OutboundError::NotAllowed`]).
//! - The adapters declare the reads and acts of their services ([`Calls`]); nothing else adds
//!   any.
//! - Singularr's own services are the exception: a Meilisearch it was given to keep its search
//!   index in is not an engine, and takes POST, PUT and DELETE (`Allowlist::allow_service`).
//! - A notification channel posts to one address of its own ([`Target`]). A target is no host
//!   rule: POST to its exact address passes and nothing else on its host does, and a host
//!   that has a rule is never reached as a target. Targets change while the server runs
//!   ([`Targets`]); the hosts of the allowlist do not.
//! - Redirects are never followed here; the caller decides, through this module again.
//! - Every call is logged with method, host, path, status and duration. The query string is
//!   not logged, and neither is the path of a target, which may be its secret.

use async_trait::async_trait;
use reqwest::Url;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

pub const TMDB_HOST: &str = "api.themoviedb.org";
pub const TVDB_HOST: &str = "api4.thetvdb.com";
pub const TVMAZE_HOST: &str = "api.tvmaze.com";
/// plex.tv: the Plex sign-in of Singularr Requests (PIN, then the account). The person's browser
/// goes to app.plex.tv to approve the PIN; Singularr itself never calls app.plex.tv.
pub const PLEX_TV_HOST: &str = "plex.tv";
/// The one path Singularr Requests may POST to on plex.tv: a new sign-in PIN.
pub const PLEX_PIN_PATH: &str = "/api/v2/pins";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    None,
    Form(Vec<(String, String)>),
    Json(String),
}

#[derive(Debug, Clone)]
pub struct OutRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
    /// How long this call may take, in place of the client's own limit: for a call known to
    /// take long (a manager's search asks every indexer). `None`: the client's limit.
    pub timeout: Option<Duration>,
}

impl OutRequest {
    pub fn get(url: impl Into<String>) -> Self {
        OutRequest {
            method: Method::Get,
            url: url.into(),
            headers: Vec::new(),
            body: Body::None,
            timeout: None,
        }
    }

    pub fn post(url: impl Into<String>, body: Body) -> Self {
        OutRequest {
            method: Method::Post,
            url: url.into(),
            headers: Vec::new(),
            body,
            timeout: None,
        }
    }

    pub fn put(url: impl Into<String>, body: Body) -> Self {
        OutRequest {
            method: Method::Put,
            url: url.into(),
            headers: Vec::new(),
            body,
            timeout: None,
        }
    }

    pub fn delete(url: impl Into<String>) -> Self {
        OutRequest {
            method: Method::Delete,
            url: url.into(),
            headers: Vec::new(),
            body: Body::None,
            timeout: None,
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    /// The same call with a limit of its own.
    pub fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }
}

#[derive(Debug, Clone)]
pub struct OutResponse {
    pub status: u16,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl OutResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, OutboundError> {
        serde_json::from_slice(&self.body).map_err(|e| OutboundError::Decode(e.to_string()))
    }
}

/// What a transport says of a call that got no answer in time.
pub const TIMED_OUT: &str = "timed out";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutboundError {
    #[error("`{0}` is not a URL Singularr can call")]
    BadUrl(String),
    #[error(
        "{host} is not on the outbound allowlist; Singularr calls only its configured engines and metadata providers"
    )]
    HostNotAllowed { host: String },
    /// The call is neither a GET nor one of the calls the host's rule lists.
    #[error(
        "{method} {path} on {label} ({host}) is refused: it is not a listed call. An integration is sent GET and the calls its adapter lists, nothing else"
    )]
    NotListed {
        method: &'static str,
        host: String,
        label: String,
        path: String,
    },
    /// The call is a listed act, and the integration's access is `read`.
    #[error(
        "{method} {path} on {label} ({host}) is refused: Singularr may only read {label}, not act on it"
    )]
    NotAllowed {
        method: &'static str,
        host: String,
        label: String,
        path: String,
    },
    #[error("{host} did not answer: {message}")]
    Transport { host: String, message: String },
    #[error("{host} answered {status} for {path}")]
    Status {
        host: String,
        path: String,
        status: u16,
    },
    #[error("the answer could not be read: {0}")]
    Decode(String),
}

impl OutboundError {
    /// The call went out and no answer came in time. That is not a refusal: whatever was
    /// asked for may still happen.
    pub fn timed_out(&self) -> bool {
        matches!(self, OutboundError::Transport { message, .. } if message == TIMED_OUT)
    }
}

/// What Singularr may do with an integration: read it, or read it and act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Act,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Act => "act",
        }
    }

    /// `read` or `act`, in any case. `None` for any other word.
    pub fn parse(text: &str) -> Option<Access> {
        match text.trim().to_ascii_lowercase().as_str() {
            "read" => Some(Access::Read),
            "act" => Some(Access::Act),
            _ => None,
        }
    }
}

/// The access of one integration, as its host rule enforces it. The rule and whoever may
/// change the access hold the same switch, so a change takes effect with the next call.
/// A new switch says `read`.
#[derive(Debug, Clone, Default)]
pub struct AccessSwitch(Arc<AtomicBool>);

impl AccessSwitch {
    pub fn get(&self) -> Access {
        if self.0.load(Ordering::SeqCst) {
            Access::Act
        } else {
            Access::Read
        }
    }

    pub fn set(&self, access: Access) {
        self.0.store(access == Access::Act, Ordering::SeqCst);
    }
}

impl PartialEq for AccessSwitch {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl Eq for AccessSwitch {}

/// What the value at one place of a JSON body must be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// Text, one of these.
    OneOf(Vec<String>),
    /// Text that is not empty.
    Text,
    /// `true` or `false`.
    Flag,
    /// This value exactly.
    Equals(serde_json::Value),
    /// An object whose members are all among these; with none listed, an empty object.
    Only(Vec<String>),
    /// A list of one to this many texts, none of them empty.
    Texts(usize),
    /// A whole number of at least 1: a record's id.
    Id,
    /// A whole number of at least 0.
    Whole,
    /// A list of one to this many ids, each a whole number of at least 1.
    Ids(usize),
    /// A list of one to this many objects, each holding what `rule` says (its pointers start at
    /// the object).
    Each { max: usize, rule: BodyRule },
    /// A list of at most this many values, none of them a list or an object.
    Scalars(usize),
}

impl Expect {
    fn holds(&self, found: Option<&serde_json::Value>) -> bool {
        use serde_json::Value;
        let Some(found) = found else {
            return false;
        };
        let text = |v: &Value| v.as_str().filter(|s| !s.trim().is_empty()).is_some();
        // A float such as `1.0` is no whole number.
        let id = |v: &Value| v.as_u64().is_some_and(|n| n >= 1);
        let list = |max: usize| {
            found
                .as_array()
                .filter(|items| !items.is_empty() && items.len() <= max)
        };
        match self {
            Expect::OneOf(values) => found
                .as_str()
                .is_some_and(|s| values.iter().any(|v| v == s)),
            Expect::Text => text(found),
            Expect::Flag => found.is_boolean(),
            Expect::Equals(value) => found == value,
            Expect::Only(members) => found
                .as_object()
                .is_some_and(|map| map.keys().all(|k| members.iter().any(|m| m == k))),
            Expect::Texts(max) => list(*max).is_some_and(|items| items.iter().all(text)),
            Expect::Id => id(found),
            Expect::Whole => found.as_u64().is_some(),
            Expect::Ids(max) => list(*max).is_some_and(|items| items.iter().all(id)),
            Expect::Each { max, rule } => list(*max).is_some_and(|items| {
                items
                    .iter()
                    .all(|item| item.is_object() && rule.holds(item))
            }),
            Expect::Scalars(max) => found.as_array().is_some_and(|items| {
                items.len() <= *max
                    && items
                        .iter()
                        .all(|v| !matches!(v, Value::Array(_) | Value::Object(_)))
            }),
        }
    }
}

/// One condition of a body rule: the value at `pointer` (RFC 6901) is what `expect` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyTest {
    pub pointer: String,
    pub expect: Expect,
}

/// What the JSON body of a call must say: every test holds. A body that is not JSON, or no
/// body, says nothing and passes no rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BodyRule {
    pub tests: Vec<BodyTest>,
    /// Texts the body's text holds exactly once each (the whole body's, in a rule of its own).
    pub once: Vec<String>,
    /// The body's text holds no backslash (the whole body's, in a rule of its own).
    pub plain: bool,
}

/// A JSON value read strictly: an object that names one member twice is refused. The services
/// differ on which of the two counts. Transmission's parser keeps the first. NZBGet searches
/// its text for the first. serde_json keeps the last.
struct Strict(serde_json::Value);

impl<'de> serde::Deserialize<'de> for Strict {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Strict, D::Error> {
        d.deserialize_any(StrictVisitor).map(Strict)
    }
}

struct StrictVisitor;

impl<'de> serde::de::Visitor<'de> for StrictVisitor {
    type Value = serde_json::Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(v))
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
        Ok(v.into())
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(v.into())
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        Ok(serde_json::Number::from_f64(v)
            .map_or(serde_json::Value::Null, serde_json::Value::Number))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(v.to_string()))
    }

    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(v))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(serde_json::Value::Array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut members = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let Strict(value) = map.next_value()?;
            if members.insert(key, value).is_some() {
                return Err(serde::de::Error::custom("a member is written twice"));
            }
        }
        Ok(serde_json::Value::Object(members))
    }
}

impl BodyRule {
    pub fn new() -> BodyRule {
        BodyRule::default()
    }

    fn test(mut self, pointer: &str, expect: Expect) -> BodyRule {
        self.tests.push(BodyTest {
            pointer: pointer.to_string(),
            expect,
        });
        self
    }

    /// The text at `pointer` is one of `values`.
    pub fn one_of(self, pointer: &str, values: &[&str]) -> BodyRule {
        let values = values.iter().map(|v| v.to_string()).collect();
        self.test(pointer, Expect::OneOf(values))
    }

    /// The value at `pointer` is text that is not empty.
    pub fn text(self, pointer: &str) -> BodyRule {
        self.test(pointer, Expect::Text)
    }

    /// The value at `pointer` is `true` or `false`.
    pub fn flag(self, pointer: &str) -> BodyRule {
        self.test(pointer, Expect::Flag)
    }

    /// The value at `pointer` is `value` exactly.
    pub fn equals(self, pointer: &str, value: serde_json::Value) -> BodyRule {
        self.test(pointer, Expect::Equals(value))
    }

    /// The value at `pointer` is an object with no member but these.
    pub fn only(self, pointer: &str, members: &[&str]) -> BodyRule {
        let members = members.iter().map(|m| m.to_string()).collect();
        self.test(pointer, Expect::Only(members))
    }

    /// The value at `pointer` is a list of one to `max` texts.
    pub fn texts(self, pointer: &str, max: usize) -> BodyRule {
        self.test(pointer, Expect::Texts(max))
    }

    /// The value at `pointer` is a whole number of at least 1.
    pub fn id(self, pointer: &str) -> BodyRule {
        self.test(pointer, Expect::Id)
    }

    /// The value at `pointer` is a whole number of at least 0.
    pub fn whole(self, pointer: &str) -> BodyRule {
        self.test(pointer, Expect::Whole)
    }

    /// The value at `pointer` is a list of one to `max` ids.
    pub fn ids(self, pointer: &str, max: usize) -> BodyRule {
        self.test(pointer, Expect::Ids(max))
    }

    /// The value at `pointer` is a list of one to `max` objects, each holding what `rule`
    /// says, its pointers read from the object (`""` is the object itself).
    pub fn each(self, pointer: &str, max: usize, rule: BodyRule) -> BodyRule {
        self.test(pointer, Expect::Each { max, rule })
    }

    /// The value at `pointer` is a list of at most `max` values, none a list or an object.
    pub fn scalars(self, pointer: &str, max: usize) -> BodyRule {
        self.test(pointer, Expect::Scalars(max))
    }

    /// The body's text holds `text` exactly once. A service that finds a member by searching
    /// its text (NZBGet finds `"method"` so) then reads the member the tests read.
    pub fn written_once(mut self, text: &str) -> BodyRule {
        self.once.push(text.to_string());
        self
    }

    /// The body's text holds no backslash. An escape cannot then hide a name from a service
    /// that searches the text.
    pub fn plain(mut self) -> BodyRule {
        self.plain = true;
        self
    }

    /// Every test holds in this value.
    fn holds(&self, json: &serde_json::Value) -> bool {
        self.tests
            .iter()
            .all(|t| t.expect.holds(json.pointer(&t.pointer)))
    }

    /// A body that is JSON and holds what every test says. A body with an object that names
    /// one member twice is refused ([`Strict`]).
    fn allows(&self, body: &Body) -> bool {
        let Body::Json(text) = body else {
            return false;
        };
        if self.plain && text.contains('\\') {
            return false;
        }
        if !self
            .once
            .iter()
            .all(|t| text.matches(t.as_str()).count() == 1)
        {
            return false;
        }
        serde_json::from_str::<Strict>(text)
            .ok()
            .is_some_and(|Strict(json)| self.holds(&json))
    }
}

/// Whether `path` is the path `declared` names. A declared segment `{id}` stands for one
/// segment of one to ten digits (a record's number), `{any}` for any one segment that is not
/// empty; a segment with `{month}` in it for the same text around a month written `YYYY-MM`
/// (`dbip-city-lite-{month}.mmdb.gz`). Every other segment is itself, exactly.
fn path_is(declared: &str, path: &str) -> bool {
    if !declared.contains('{') {
        return declared == path;
    }
    let (mut want, mut got) = (declared.split('/'), path.split('/'));
    loop {
        match (want.next(), got.next()) {
            (None, None) => return true,
            (Some(w), Some(g)) if segment_is(w, g) => {}
            _ => return false,
        }
    }
}

/// Whether `path` lies below the path `declared` names: its first segments are those, and
/// at least one more follows.
fn path_under(declared: &str, path: &str) -> bool {
    let mut got = path.split('/');
    declared
        .split('/')
        .all(|w| got.next().is_some_and(|g| segment_is(w, g)))
        && got.next().is_some()
}

fn segment_is(declared: &str, segment: &str) -> bool {
    match declared {
        "{id}" => (1..=10).contains(&segment.len()) && segment.bytes().all(|b| b.is_ascii_digit()),
        "{any}" => !segment.is_empty(),
        with_month if with_month.contains("{month}") => month_segment(with_month, segment),
        exact => exact == segment,
    }
}

/// Whether a segment is the declared one with a month (`YYYY-MM`, 01 to 12) where it says
/// `{month}`.
fn month_segment(declared: &str, segment: &str) -> bool {
    let Some((before, after)) = declared.split_once("{month}") else {
        return false;
    };
    let Some(month) = segment
        .strip_prefix(before)
        .and_then(|rest| rest.strip_suffix(after))
    else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match month.split_once('-') {
        Some((year, number)) => {
            year.len() == 4
                && digits(year)
                && number.len() == 2
                && digits(number)
                && number.parse::<u8>().is_ok_and(|n| (1..=12).contains(&n))
        }
        None => false,
    }
}

/// A condition on the query of a GET: the parameter `key` is written once, with one of
/// `values`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryRule {
    pub key: String,
    pub values: Vec<String>,
}

impl QueryRule {
    /// A query that names the key once, with one of the values. Named twice or not at all, it
    /// is refused: a service may read either one, or take no value as a value.
    fn allows(&self, url: &Url) -> bool {
        let mut found = url.query_pairs().filter(|(k, _)| k == self.key.as_str());
        match (found.next(), found.next()) {
            (Some((_, value)), None) => self.values.iter().any(|v| *v == value),
            _ => false,
        }
    }
}

/// A GET a host takes once its adapter declares its GETs: the exact path, and what its query
/// must say. `query: None`: the path alone says that the call reads. `others: None`: the query
/// may name any key beside the rule's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetCall {
    pub path: String,
    pub query: Option<QueryRule>,
    /// The keys the query may name beside the rule's own. `None`: any key.
    pub others: Option<Vec<String>>,
}

impl GetCall {
    /// The query holds what the rule says, and no key but the rule's own and `others`.
    fn allows(&self, url: &Url) -> bool {
        self.query.as_ref().is_none_or(|q| q.allows(url))
            && self.others.as_ref().is_none_or(|others| {
                url.query_pairs().all(|(key, _)| {
                    self.query.as_ref().is_some_and(|q| key == q.key.as_str())
                        || others.iter().any(|o| *o == key)
                })
            })
    }
}

/// A call that is not a GET and only reads. It passes whatever the access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadCall {
    pub method: Method,
    /// The exact path.
    pub path: String,
    /// What the body must say for the call to be one that only reads. `None`: the path alone
    /// says so.
    pub body: Option<BodyRule>,
}

/// A call that changes something in the service. It passes only while the access is `act`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActCall {
    pub method: Method,
    /// The exact path, where a segment `{id}` stands for a record's number.
    pub path: String,
    /// What the body must say for the call to be this act, where one path does more than one
    /// thing. `None`: the path alone says what it does.
    pub body: Option<BodyRule>,
    /// What the query must say, for an act that takes its choices there: every listed key
    /// once, with one of its values, and no other key. `None`: the query is not read.
    pub query: Option<Vec<QueryRule>>,
}

impl ActCall {
    fn matches(&self, method: Method, url: &Url, body: &Body) -> bool {
        self.method == method
            && path_is(&self.path, url.path())
            && self.body.as_ref().is_none_or(|said| said.allows(body))
            && self.query.as_ref().is_none_or(|rules| {
                url.query_pairs()
                    .all(|(key, _)| rules.iter().any(|r| r.key == key))
                    && rules.iter().all(|r| r.allows(url))
            })
    }
}

/// The calls beyond GET an adapter declares for its service, and its GETs when the service
/// takes only some.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Calls {
    pub reads: Vec<ReadCall>,
    pub acts: Vec<ActCall>,
    /// Empty: every GET passes. Otherwise only these.
    pub gets: Vec<GetCall>,
    /// No GET passes at all: the service takes its commands on GET too. Every read the adapter
    /// makes is a listed POST.
    pub no_get: bool,
    /// Paths refused whatever the method, GET included: what a service does on a GET that
    /// is no read. A path that ends with `/` stands for everything below it.
    pub never: Vec<String>,
}

impl Calls {
    /// A GET of `path` that passes while its query names `key` once with one of `values`.
    /// Once an adapter declares one, a GET to its host passes only as one it declares: for a
    /// service whose GETs can change things (Tautulli's API takes every command as a GET).
    pub fn get_when(mut self, path: &str, key: &str, values: &[&str]) -> Calls {
        self.gets.push(GetCall {
            path: path.to_string(),
            query: Some(QueryRule {
                key: key.to_string(),
                values: values.iter().map(|v| v.to_string()).collect(),
            }),
            others: None,
        });
        self
    }

    /// A GET of `path`, whatever its query: a path that only reads. Once an adapter declares one,
    /// a GET to its host passes only as one it declares: for a service that starts work on some
    /// of its GETs (a media server starts a transcode or a library scan on GET). `{id}` and
    /// `{any}` stand for one segment, as in every declared path.
    pub fn get(mut self, path: &str) -> Calls {
        self.gets.push(GetCall {
            path: path.to_string(),
            query: None,
            others: None,
        });
        self
    }

    /// A GET of `path` that passes while its query names `key` once with one of `values`, and
    /// no key but `key` and `others`. For a service where a key beside the command makes the
    /// same command a change. SABnzbd's `mode=queue` lists the queue. With `name=delete` it
    /// deletes from it.
    pub fn get_only(mut self, path: &str, key: &str, values: &[&str], others: &[&str]) -> Calls {
        self.gets.push(GetCall {
            path: path.to_string(),
            query: Some(QueryRule {
                key: key.to_string(),
                values: values.iter().map(|v| v.to_string()).collect(),
            }),
            others: Some(others.iter().map(|o| o.to_string()).collect()),
        });
        self
    }

    /// No GET to the host passes: for a service that takes every command on GET as well as
    /// with POST (NZBGet answers `/jsonrpc/<method>`), whose adapter reads with POST alone.
    pub fn no_get(mut self) -> Calls {
        self.no_get = true;
        self
    }

    /// A call that only reads, by its method and path alone.
    pub fn read(mut self, method: Method, path: &str) -> Calls {
        self.reads.push(ReadCall {
            method,
            path: path.to_string(),
            body: None,
        });
        self
    }

    /// A call that only reads while its JSON body holds one of `values` at `pointer`.
    pub fn read_when(self, method: Method, path: &str, pointer: &str, values: &[&str]) -> Calls {
        self.read_with(method, path, BodyRule::new().one_of(pointer, values))
    }

    /// A call that only reads while its JSON body says what `rule` says.
    pub fn read_with(mut self, method: Method, path: &str, rule: BodyRule) -> Calls {
        self.reads.push(ReadCall {
            method,
            path: path.to_string(),
            body: Some(rule),
        });
        self
    }

    /// A path refused whatever the method: one the service changes something on even when
    /// asked with GET. Ending with `/`: everything below it.
    pub fn never(mut self, path: &str) -> Calls {
        self.never.push(path.to_string());
        self
    }

    /// A call that changes something in the service.
    pub fn act(mut self, method: Method, path: &str) -> Calls {
        self.acts.push(ActCall {
            method,
            path: path.to_string(),
            body: None,
            query: None,
        });
        self
    }

    /// A call that changes something in the service, and is that act only while its JSON body
    /// says what `rule` says: where one path does more than one thing.
    pub fn act_with(mut self, method: Method, path: &str, rule: BodyRule) -> Calls {
        self.acts.push(ActCall {
            method,
            path: path.to_string(),
            body: Some(rule),
            query: None,
        });
        self
    }

    /// A call that changes something in the service and takes its choices in the query: it is
    /// that act only while the query names each of `keys` once, with one of its values, and
    /// nothing else.
    pub fn act_query(mut self, method: Method, path: &str, keys: &[(&str, &[&str])]) -> Calls {
        self.acts.push(ActCall {
            method,
            path: path.to_string(),
            body: None,
            query: Some(
                keys.iter()
                    .map(|(key, values)| QueryRule {
                        key: key.to_string(),
                        values: values.iter().map(|v| v.to_string()).collect(),
                    })
                    .collect(),
            ),
        });
        self
    }
}

/// What one listed host may be asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRule {
    /// Who this is, for logs and errors: the integration's id (`radarr`), or `tmdb`.
    pub label: String,
    /// Only GET passes, plus `reads`, plus `acts` while the access is `act`. False for one of
    /// Singularr's own services, which takes every method.
    pub read_only: bool,
    /// Calls that are not GET and only read.
    pub reads: Vec<ReadCall>,
    /// Calls that change something in the service.
    pub acts: Vec<ActCall>,
    /// The only GETs that pass, when the adapter declares any. Empty: every GET passes.
    pub gets: Vec<GetCall>,
    /// No GET passes.
    pub no_get: bool,
    /// Paths refused whatever the method, GET included.
    pub never: Vec<String>,
    /// What Singularr may do with the integration behind this host.
    pub access: AccessSwitch,
}

impl HostRule {
    fn new(label: &str, read_only: bool, calls: Calls) -> HostRule {
        HostRule {
            label: label.to_string(),
            read_only,
            reads: calls.reads,
            acts: calls.acts,
            gets: calls.gets,
            no_get: calls.no_get,
            never: calls.never,
            access: AccessSwitch::default(),
        }
    }
}

/// `host:port`, lower case. The port is the scheme's default when the URL has none.
fn host_key(url: &Url) -> Option<String> {
    Some(format!(
        "{}:{}",
        url.host_str()?.to_ascii_lowercase(),
        url.port_or_known_default()?
    ))
}

/// `scheme://host:port`, the origin of a URL as the allowlist names it.
fn origin_key(url: &Url) -> Option<String> {
    Some(format!("{}://{}", url.scheme(), host_key(url)?))
}

/// What a rule is listed by: the origin, then the path of the address without its last `/`
/// (nothing for an address at the root).
fn rule_key(url: &Url) -> Option<String> {
    Some(format!(
        "{}{}",
        origin_key(url)?,
        url.path().trim_end_matches('/')
    ))
}

/// Whether the path of a rule's address holds a call's path, whole segments: `/radarr` holds
/// `/radarr` and `/radarr/api`, and not `/radarrx`. The root (nothing) holds every path.
fn path_holds(base: &str, path: &str) -> bool {
    base.is_empty()
        || path
            .strip_prefix(base)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    rules: BTreeMap<String, HostRule>,
}

impl Allowlist {
    /// The metadata providers every installation may reach. Read-only, with the one login.
    pub fn with_metadata_providers() -> Self {
        let mut list = Allowlist::default();
        for (host, label, sign_in) in [
            (TMDB_HOST, "tmdb", None),
            (TVDB_HOST, "tvdb", Some("/v4/login")),
            (TVMAZE_HOST, "tvmaze", None),
        ] {
            list.rules.insert(
                format!("https://{host}:443"),
                HostRule::new(label, true, posts(sign_in.as_slice())),
            );
        }
        list
    }

    /// The hosts Singularr Requests may reach, and nothing else: TMDB (search and title facts,
    /// GET) and plex.tv (GET, plus the POST that makes a sign-in PIN).
    pub fn for_requests_face() -> Self {
        let mut list = Allowlist::default();
        for (host, label, pin) in [
            (TMDB_HOST, "tmdb", None),
            (PLEX_TV_HOST, "plex.tv", Some(PLEX_PIN_PATH)),
        ] {
            list.rules.insert(
                format!("https://{host}:443"),
                HostRule::new(label, true, posts(pin.as_slice())),
            );
        }
        list
    }

    /// List the origin of an engine URL: GET, plus `post_paths`, the exact paths that take a
    /// POST that only reads (the qBittorrent sign-in); an arr has none.
    pub fn allow_engine(
        &mut self,
        label: &str,
        base_url: &str,
        post_paths: &[&str],
    ) -> Result<(), OutboundError> {
        self.allow_integration(label, base_url, posts(post_paths))
    }

    /// List the address of an integration with the calls beyond GET its adapter declares. Its
    /// access is `read` until its switch says otherwise ([`Allowlist::switch`]). The paths of
    /// the calls are the adapter's own, below the address: an integration reached under a
    /// path (`https://host/bridge`) is called there. One address has one rule: of two
    /// integrations at the same address, the one listed last holds it. Integrations at one
    /// origin under different paths have a rule each.
    pub fn allow_integration(
        &mut self,
        label: &str,
        base_url: &str,
        mut calls: Calls,
    ) -> Result<(), OutboundError> {
        let url = Url::parse(base_url).map_err(|_| OutboundError::BadUrl(base_url.to_string()))?;
        let key = rule_key(&url).ok_or_else(|| OutboundError::BadUrl(base_url.to_string()))?;
        let below = url.path().trim_end_matches('/');
        if !below.is_empty() {
            for path in calls
                .reads
                .iter_mut()
                .map(|call| &mut call.path)
                .chain(calls.acts.iter_mut().map(|call| &mut call.path))
                .chain(calls.gets.iter_mut().map(|call| &mut call.path))
                .chain(calls.never.iter_mut())
            {
                *path = format!("{below}{path}");
            }
        }
        self.rules.insert(key, HostRule::new(label, true, calls));
        Ok(())
    }

    /// List the origin of one of Singularr's own services (the Meilisearch that holds its search
    /// index): every method passes. Never used for an engine.
    pub fn allow_service(&mut self, label: &str, base_url: &str) -> Result<(), OutboundError> {
        let url = Url::parse(base_url).map_err(|_| OutboundError::BadUrl(base_url.to_string()))?;
        let key = rule_key(&url).ok_or_else(|| OutboundError::BadUrl(base_url.to_string()))?;
        self.rules
            .insert(key, HostRule::new(label, false, Calls::default()));
        Ok(())
    }

    /// Every listed address (the origin, then the path of an integration reached under one) with
    /// its rule, for the startup log and the doctor.
    pub fn hosts(&self) -> impl Iterator<Item = (&String, &HostRule)> {
        self.rules.iter()
    }

    /// The rule a call to `origin` at `path` goes by: of the rules listed at the origin, the
    /// one whose path holds the call's, the longest such path first. `Err(Some(rule))`: the
    /// origin has rules and none holds the path, with the first of them to name in the refusal.
    /// `Err(None)`: the origin has no rule.
    fn rule_at(&self, origin: &str, path: &str) -> Result<&HostRule, Option<&HostRule>> {
        let mut first = None;
        let mut best: Option<(usize, &HostRule)> = None;
        for (key, rule) in &self.rules {
            let Some(base) = key.strip_prefix(origin) else {
                continue;
            };
            if !(base.is_empty() || base.starts_with('/')) {
                continue;
            }
            first = first.or(Some(rule));
            if path_holds(base, path) && best.is_none_or(|(len, _)| base.len() > len) {
                best = Some((base.len(), rule));
            }
        }
        best.map(|(_, rule)| rule).ok_or(first)
    }

    /// The access switch of the host listed under this label (an integration's id), when the
    /// host has an act the switch could let through. `None` for every other label: a host
    /// without an act, an integration that is not asked, or one whose address another
    /// integration listed after it.
    pub fn switch(&self, label: &str) -> Option<AccessSwitch> {
        self.rules
            .values()
            .find(|rule| rule.label == label && !rule.acts.is_empty())
            .map(|rule| rule.access.clone())
    }

    /// May this method go to this URL without a body. See [`Allowlist::check_call`].
    pub fn check(&self, method: Method, url: &str) -> Result<(Url, &HostRule), OutboundError> {
        self.check_call(method, url, &Body::None)
    }

    /// The whole decision, pure: may this call be sent.
    pub fn check_call(
        &self,
        method: Method,
        url: &str,
        body: &Body,
    ) -> Result<(Url, &HostRule), OutboundError> {
        let parsed = Url::parse(url).map_err(|_| OutboundError::BadUrl(without_query(url)))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(OutboundError::BadUrl(without_query(url)));
        }
        let key = origin_key(&parsed).ok_or_else(|| OutboundError::BadUrl(without_query(url)))?;
        let path = parsed.path();
        let rule = match self.rule_at(&key, path) {
            Ok(rule) => rule,
            Err(Some(near)) => {
                return Err(OutboundError::NotListed {
                    method: method.as_str(),
                    host: key,
                    label: near.label.clone(),
                    path: path.to_string(),
                });
            }
            Err(None) => return Err(OutboundError::HostNotAllowed { host: key }),
        };
        let barred = rule
            .never
            .iter()
            .any(|never| match never.strip_suffix('/') {
                Some(below) => path_under(below, path),
                None => path_is(never, path),
            });
        if barred {
            return Err(OutboundError::NotListed {
                method: method.as_str(),
                host: key,
                label: rule.label.clone(),
                path: path.to_string(),
            });
        }
        if !rule.read_only {
            return Ok((parsed, rule));
        }
        if method == Method::Get {
            // A host whose adapter declares its GETs takes those alone. One that takes no GET
            // takes none. The refusal names the path and never the query, which may hold a
            // key.
            let declared = rule.gets.is_empty()
                || rule
                    .gets
                    .iter()
                    .any(|call| path_is(&call.path, path) && call.allows(&parsed));
            if declared && !rule.no_get {
                return Ok((parsed, rule));
            }
            return Err(OutboundError::NotListed {
                method: Method::Get.as_str(),
                host: key,
                label: rule.label.clone(),
                path: path.to_string(),
            });
        }
        let reads = rule.reads.iter().any(|call| {
            call.method == method
                && path_is(&call.path, path)
                && call.body.as_ref().is_none_or(|said| said.allows(body))
        });
        if reads {
            return Ok((parsed, rule));
        }
        let acts = rule
            .acts
            .iter()
            .any(|call| call.matches(method, &parsed, body));
        if acts && rule.access.get() == Access::Act {
            return Ok((parsed, rule));
        }
        let (method, label, path) = (method.as_str(), rule.label.clone(), path.to_string());
        Err(if acts {
            OutboundError::NotAllowed {
                method,
                host: key,
                label,
                path,
            }
        } else {
            OutboundError::NotListed {
                method,
                host: key,
                label,
                path,
            }
        })
    }
}

/// A URL as an error may show it: without its query, which can hold a key (Tautulli takes its
/// key there), and without a fragment.
fn without_query(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or_default().to_string()
}

/// The POSTs that only read by their path alone (a sign-in), as calls.
fn posts(paths: &[&str]) -> Calls {
    paths.iter().fold(Calls::default(), |calls, path| {
        calls.read(Method::Post, path)
    })
}

/// What a log line and an error say in place of the address of a target: the path of a
/// webhook is its secret.
pub const TARGET_ADDRESS: &str = "(channel address)";

/// One address a notification channel posts to: the exact URL, with its scheme, host, port,
/// path and query. POST to it passes; nothing else passes because of it.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    /// Who posts there, for logs and metrics: `notify:slack`.
    pub label: String,
    url: Url,
}

impl Target {
    /// A target at an `http://` or `https://` address. The error does not repeat the address.
    pub fn new(label: &str, url: &str) -> Result<Target, OutboundError> {
        let bad = || OutboundError::BadUrl(TARGET_ADDRESS.to_string());
        let mut parsed = Url::parse(url.trim()).map_err(|_| bad())?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(bad());
        }
        parsed.set_fragment(None);
        Ok(Target {
            label: label.to_string(),
            url: parsed,
        })
    }

    /// `host:port`, for the startup log. The rest of the address is not shown.
    pub fn host(&self) -> String {
        host_key(&self.url).unwrap_or_default()
    }

    fn takes(&self, method: Method, url: &Url) -> bool {
        method == Method::Post && self.url == *url
    }
}

/// A target shows its label and its host, never its address.
impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("label", &self.label)
            .field("host", &self.host())
            .finish_non_exhaustive()
    }
}

/// The targets, each under the key of who listed it (`channel:7`). Every clone of an
/// [`Outbound`] shares them, and they change while the server runs: a channel is added,
/// changed and removed in the interface.
#[derive(Clone, Default)]
pub struct Targets(Arc<RwLock<BTreeMap<String, Target>>>);

impl Targets {
    /// List a target under `key`, in place of what the key listed before.
    pub fn put(&self, key: &str, target: Target) {
        self.0
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_string(), target);
    }

    pub fn remove(&self, key: &str) {
        self.0
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }

    /// Every target with its key, for the startup log.
    pub fn list(&self) -> Vec<(String, Target)> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(key, target)| (key.clone(), target.clone()))
            .collect()
    }

    /// The target that takes this call.
    fn find(&self, method: Method, url: &Url) -> Option<Target> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .find(|target| target.takes(method, url))
            .cloned()
    }
}

/// A call that may be sent.
#[derive(Debug, Clone)]
pub struct Checked {
    pub url: Url,
    /// Who is called, for logs and metrics: the label of the host rule or of the target.
    pub label: String,
    /// The call passed as a POST to a notification target, not by a host rule.
    pub target: bool,
}

/// What actually puts bytes on the wire. Production uses reqwest; tests use recorded answers.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, url: Url, req: &OutRequest) -> Result<OutResponse, String>;

    /// Send, and write a 2xx answer's body into `sink` piece by piece as it arrives: a file too
    /// large to hold whole. The answer comes back without that body. By default the body is
    /// read whole first, then written.
    async fn send_into(
        &self,
        url: Url,
        req: &OutRequest,
        sink: &mut (dyn std::io::Write + Send),
    ) -> Result<OutResponse, String> {
        let mut res = self.send(url, req).await?;
        if (200..300).contains(&res.status) {
            sink.write_all(&res.body).map_err(|e| e.to_string())?;
            res.body = Vec::new();
        }
        Ok(res)
    }
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
}

/// A reqwest error without its URL (the URL can carry a query that is not for logs).
fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return TIMED_OUT.to_string();
    }
    if e.is_connect() {
        return "connection refused or unreachable".to_string();
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
}

impl Outbound {
    /// The production client: one reqwest client, a connect timeout, a total timeout, no
    /// redirects, and a user agent that names the program.
    pub fn new(allow: Allowlist, timeout: Duration) -> Result<Self, OutboundError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10).min(timeout))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("singularr/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| OutboundError::Transport {
                host: "(client)".to_string(),
                message: e.to_string(),
            })?;
        Ok(Outbound {
            allow: Arc::new(allow),
            targets: Targets::default(),
            transport: Arc::new(ReqwestTransport { client }),
        })
    }

    /// A client over another transport (recorded answers in tests).
    pub fn with_transport(allow: Allowlist, transport: Arc<dyn Transport>) -> Self {
        Outbound {
            allow: Arc::new(allow),
            targets: Targets::default(),
            transport,
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
        let parsed = Url::parse(url.trim()).ok()?;
        let key = origin_key(&parsed)?;
        match self.allow.rule_at(&key, parsed.path()) {
            Ok(rule) | Err(Some(rule)) => Some(rule.label.clone()),
            Err(None) => None,
        }
    }

    /// The whole decision for one call, pure. A host with a rule is judged by its rule alone
    /// ([`Allowlist::check_call`]). A call to any other host passes only as a POST to the
    /// exact address of a target.
    pub fn check(&self, method: Method, url: &str, body: &Body) -> Result<Checked, OutboundError> {
        match self.allow.check_call(method, url, body) {
            Ok((url, rule)) => Ok(Checked {
                url,
                label: rule.label.clone(),
                target: false,
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
                    }),
                    None => Err(OutboundError::HostNotAllowed { host }),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Check, send, log. A status outside 2xx is returned, not turned into an error; use
    /// [`Outbound::get_json`] when only a 2xx answer is useful.
    pub async fn send(&self, req: OutRequest) -> Result<OutResponse, OutboundError> {
        let checked = self.admit(&req)?;
        self.dispatch(&req, checked, None).await
    }

    /// [`Outbound::send`] for a file too large to hold: the same check, log and count. A 2xx
    /// answer's body is written into `sink` as it arrives, and the answer comes back without it.
    pub async fn send_into(
        &self,
        req: OutRequest,
        sink: &mut (dyn std::io::Write + Send),
    ) -> Result<OutResponse, OutboundError> {
        let checked = self.admit(&req)?;
        self.dispatch(&req, checked, Some(sink)).await
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
        checked: Checked,
        sink: Option<&mut (dyn std::io::Write + Send)>,
    ) -> Result<OutResponse, OutboundError> {
        let host = checked.url.host_str().unwrap_or_default().to_string();
        let path = if checked.target {
            TARGET_ADDRESS.to_string()
        } else {
            checked.url.path().to_string()
        };
        let label = checked.label;
        let started = Instant::now();
        let result = match sink {
            Some(sink) => self.transport.send_into(checked.url, req, sink).await,
            None => self.transport.send(checked.url, req).await,
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
                    error = %message,
                    "{label}"
                );
                metrics::counter!(
                    "singularr_outbound_requests_total",
                    "target" => label,
                    "status" => "error"
                )
                .increment(1);
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

/// A transport that answers from a table and remembers what it was asked. For tests in every
/// layer: adapters are exercised against recorded JSON, through the real allowlist check.
pub mod recorded {
    use super::*;
    use std::sync::Mutex;

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
        pub seen: Mutex<Vec<(Method, String)>>,
        /// Every call as it was sent, in order.
        pub sent: Mutex<Vec<OutRequest>>,
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
        /// words a transport says it with ([`TIMED_OUT`], or any other).
        pub fn fail(&self, method: Method, url_part: &str, words: &str) {
            self.answers.lock().unwrap().push(Answer {
                method,
                url_part: url_part.to_string(),
                body_part: None,
                response: Err(words.to_string()),
            });
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

    #[test]
    fn a_get_to_a_listed_engine_passes() {
        let a = allow();
        let (url, rule) = a
            .check(Method::Get, "http://192.0.2.61:7979/api/v3/movie")
            .unwrap();
        assert_eq!(url.path(), "/api/v3/movie");
        assert_eq!(rule.label, "radarr");
    }

    /// The safety guarantee of stage 1: nothing but GET reaches an arr.
    #[test]
    fn anything_but_get_to_an_arr_is_refused() {
        let a = allow();
        for method in [Method::Post, Method::Put, Method::Delete] {
            for url in [
                "http://192.0.2.61:7979/api/v3/command",
                "http://192.0.2.61:7979/api/v3/movie/12",
                "http://192.0.2.61:8989/api/v3/episodefile/9",
                "http://192.0.2.61:8989/api/v3/queue/3",
            ] {
                let err = a.check(method, url).unwrap_err();
                assert!(
                    matches!(err, OutboundError::NotListed { .. }),
                    "{} {url} must be refused, got {err:?}",
                    method.as_str()
                );
            }
        }
    }

    #[test]
    fn qbittorrent_accepts_the_login_post_and_nothing_else() {
        let a = allow();
        assert!(
            a.check(Method::Post, "http://192.0.2.62:8080/api/v2/auth/login")
                .is_ok()
        );
        assert!(
            a.check(Method::Get, "http://192.0.2.62:8080/api/v2/torrents/info")
                .is_ok()
        );
        for url in [
            "http://192.0.2.62:8080/api/v2/torrents/delete",
            "http://192.0.2.62:8080/api/v2/torrents/pause",
            "http://192.0.2.62:8080/api/v2/auth/login/extra",
        ] {
            assert!(matches!(
                a.check(Method::Post, url),
                Err(OutboundError::NotListed { .. })
            ));
        }
        assert!(matches!(
            a.check(Method::Delete, "http://192.0.2.62:8080/api/v2/auth/login"),
            Err(OutboundError::NotListed { .. })
        ));
    }

    #[test]
    fn hosts_that_are_not_listed_are_refused() {
        let a = allow();
        for url in [
            "http://192.0.2.61:9696/api/v1/indexer",
            "https://example.com/",
            "http://192.0.2.61/api/v3/movie",
            "https://192.0.2.61:7979/api/v3/movie",
            "https://skyhook.sonarr.tv/v1/tvdb/shows/en/1",
        ] {
            assert!(
                matches!(
                    a.check(Method::Get, url),
                    Err(OutboundError::HostNotAllowed { .. })
                ),
                "{url} must be refused"
            );
        }
        assert!(matches!(
            a.check(Method::Get, "file:///etc/passwd"),
            Err(OutboundError::BadUrl(_))
        ));
        assert!(matches!(
            a.check(Method::Get, "not a url"),
            Err(OutboundError::BadUrl(_))
        ));
    }

    /// Singularr's own search service takes writes; the engines next to it stay read-only.
    #[test]
    fn the_search_service_is_the_one_host_that_takes_writes() {
        let mut a = allow();
        a.allow_service("meilisearch", "http://10.0.0.9:7700")
            .unwrap();
        for method in [Method::Get, Method::Post, Method::Put, Method::Delete] {
            assert!(
                a.check(
                    method,
                    "http://10.0.0.9:7700/indexes/singularr_library/documents"
                )
                .is_ok(),
                "{}",
                method.as_str()
            );
        }
        assert!(matches!(
            a.check(Method::Post, "http://192.0.2.61:7979/api/v3/command"),
            Err(OutboundError::NotListed { .. })
        ));
        assert!(
            a.check(Method::Put, "http://10.0.0.9:7701/indexes")
                .is_err(),
            "another port is another origin"
        );
    }

    #[test]
    fn metadata_providers_are_listed_read_only() {
        let a = allow();
        assert!(
            a.check(Method::Get, "https://api.themoviedb.org/3/tv/1396")
                .is_ok()
        );
        assert!(
            a.check(
                Method::Get,
                "https://api.tvmaze.com/lookup/shows?thetvdb=81189"
            )
            .is_ok()
        );
        assert!(
            a.check(Method::Post, "https://api4.thetvdb.com/v4/login")
                .is_ok()
        );
        assert!(
            a.check(Method::Post, "https://api.themoviedb.org/3/list")
                .is_err()
        );
        assert!(
            a.check(Method::Delete, "https://api4.thetvdb.com/v4/series/1")
                .is_err()
        );
        // The plain-http form of a provider is another origin and is not listed.
        assert!(
            a.check(Method::Get, "http://api.tvmaze.com/shows/1")
                .is_err()
        );
    }

    #[test]
    fn the_request_face_reaches_tmdb_and_plex_tv_only() {
        let a = Allowlist::for_requests_face();
        assert!(
            a.check(
                Method::Get,
                "https://api.themoviedb.org/3/search/multi?query=x"
            )
            .is_ok()
        );
        assert!(
            a.check(Method::Post, "https://plex.tv/api/v2/pins?strong=true")
                .is_ok()
        );
        assert!(
            a.check(Method::Get, "https://plex.tv/api/v2/pins/42")
                .is_ok()
        );
        assert!(a.check(Method::Get, "https://plex.tv/api/v2/user").is_ok());
        for (method, url) in [
            (Method::Post, "https://plex.tv/api/v2/user"),
            (Method::Delete, "https://plex.tv/api/v2/pins/42"),
            (Method::Get, "https://app.plex.tv/auth"),
            (Method::Get, "http://192.0.2.62:5055/api/v1/request"),
            (Method::Get, "http://192.0.2.61:7979/api/v3/movie"),
            (Method::Get, "https://api4.thetvdb.com/v4/series/1"),
            (Method::Get, "http://plex.tv/api/v2/user"),
        ] {
            assert!(a.check(method, url).is_err(), "{} {url}", method.as_str());
        }
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
            Calls::default().act(Method::Post, "/stage"),
        )
        .unwrap();
        a
    }

    fn json(text: &str) -> Body {
        Body::Json(text.to_string())
    }

    #[test]
    fn a_host_that_declares_its_gets_takes_those_alone() {
        const KEY: &str = "0123456789abcdef0123456789abcdef";
        let mut a = Allowlist::with_metadata_providers();
        a.allow_integration(
            "history",
            "http://10.0.0.50:8181",
            Calls::default().get_when("/api/v2", "cmd", &["get_history", "get_users"]),
        )
        .unwrap();
        a.allow_integration(
            "behind",
            "http://10.0.0.51/tautulli",
            Calls::default().get_when("/api/v2", "cmd", &["get_history"]),
        )
        .unwrap();
        a.allow_engine("radarr", "http://10.0.0.61:7878", &[])
            .unwrap();
        let at = |rest: &str| format!("http://10.0.0.50:8181/api/v2?apikey={KEY}{rest}");
        for url in [
            at("&cmd=get_history&length=10"),
            at("&cmd=get_users"),
            at("&cmd=get%5Fhistory"),
            format!("http://10.0.0.51/tautulli/api/v2?apikey={KEY}&cmd=get_history"),
        ] {
            assert!(a.check(Method::Get, &url).is_ok(), "{url}");
        }
        for url in [
            at("&cmd=delete_history"),
            at("&cmd=terminate_session"),
            at("&cmd=restart"),
            at("&cmd=Get_History"),
            at("&cmd=get_history%00"),
            at(""),
            at("&cmd="),
            at("&cmd=get_history&cmd=delete_history"),
            format!("http://10.0.0.50:8181/api/v3?apikey={KEY}&cmd=get_history"),
            format!("http://10.0.0.50:8181/?apikey={KEY}&cmd=get_history"),
            format!("http://10.0.0.51/api/v2?apikey={KEY}&cmd=get_history"),
        ] {
            let err = a.check(Method::Get, &url).unwrap_err();
            assert!(
                matches!(err, OutboundError::NotListed { .. }),
                "{url} must be refused, got {err:?}"
            );
            let text = err.to_string();
            assert!(!text.contains(KEY) && !text.contains("cmd"), "{text}");
        }
        // Every other host: GET always passes.
        assert!(
            a.check(Method::Get, "http://10.0.0.61:7878/api/v3/movie?x=1")
                .is_ok()
        );
        assert!(
            a.check(Method::Get, "https://api.themoviedb.org/3/movie/603")
                .is_ok()
        );
    }

    /// A media server starts work on some GETs (a transcode, a library scan, a download). Its
    /// adapter declares the paths it reads: a GET of one of them passes with any query. Every
    /// other GET is refused, as is any other method at the same paths.
    #[test]
    fn a_host_that_declares_its_paths_takes_gets_of_those_alone() {
        let mut a = Allowlist::default();
        a.allow_integration(
            "server",
            "http://10.0.0.40:8096/jellyfin",
            Calls::default()
                .get("/Items")
                .get("/Items/{any}/Images/{any}")
                .get("/library/metadata/{id}"),
        )
        .unwrap();
        let at = |rest: &str| format!("http://10.0.0.40:8096/jellyfin{rest}");
        for url in [
            at("/Items?ParentId=abc&Recursive=true&Fields=ProviderIds,Path"),
            at("/Items"),
            at("/Items/0123abcd/Images/Primary?maxWidth=300"),
            at("/library/metadata/109882"),
        ] {
            assert!(a.check(Method::Get, &url).is_ok(), "{url}");
        }
        for url in [
            at("/Items/0123abcd/Download"),
            at("/Videos/0123abcd/stream?static=true"),
            at("/Items/0123abcd/Images"),
            at("/Items/0123abcd/Images/Primary/0"),
            at("/library/metadata/abc"),
            at("/library/sections/1/refresh"),
            "http://10.0.0.40:8096/Items".to_string(),
        ] {
            assert!(
                matches!(
                    a.check(Method::Get, &url),
                    Err(OutboundError::NotListed { .. })
                ),
                "{url} must be refused"
            );
        }
        for method in [Method::Post, Method::Put, Method::Delete] {
            assert!(
                a.check(method, &at("/Items")).is_err(),
                "{}",
                method.as_str()
            );
        }
    }

    /// The label of the rule a GET goes by, or how it was refused.
    fn rule_of(a: &Allowlist, url: &str) -> Result<String, OutboundError> {
        a.check(Method::Get, url)
            .map(|(_, rule)| rule.label.clone())
    }

    /// A media server at the root of a host and a manager under a path of the same host keep a
    /// rule each, in either order: a call goes by the rule whose path holds it, the longest one
    /// first. A path that only begins like another is not below it.
    #[test]
    fn a_rule_is_listed_by_its_address_and_the_longest_path_wins() {
        let server = || {
            Calls::default()
                .get("/library/sections")
                .get("/status/sessions")
        };
        let manager = || Calls::default().act(Method::Post, "/api/v3/command");
        for server_first in [true, false] {
            let mut a = Allowlist::default();
            let mut list = |label: &str, address: &str, calls: Calls| {
                a.allow_integration(label, address, calls).unwrap();
            };
            if server_first {
                list("plex", "https://media.example.org/", server());
                list("radarr", "https://media.example.org/radarr", manager());
            } else {
                list("radarr", "https://media.example.org/radarr/", manager());
                list("plex", "https://media.example.org", server());
            }
            let at = |path: &str| format!("https://media.example.org{path}");
            assert_eq!(rule_of(&a, &at("/library/sections")).unwrap(), "plex");
            assert_eq!(rule_of(&a, &at("/radarr/api/v3/movie")).unwrap(), "radarr");
            assert_eq!(rule_of(&a, &at("/radarr")).unwrap(), "radarr");
            // The server takes its declared GETs alone, the manager every GET and its act only
            // while it may act.
            for path in ["/library/sections/1/refresh", "/radarrx/api", "/"] {
                match a.check(Method::Get, &at(path)) {
                    Err(OutboundError::NotListed { label, .. }) => assert_eq!(label, "plex"),
                    other => panic!("{path}: {:?}", other.map(|(_, r)| r.label.clone())),
                }
            }
            assert!(matches!(
                a.check(Method::Post, &at("/radarr/api/v3/command")),
                Err(OutboundError::NotAllowed { .. })
            ));
            assert!(matches!(
                a.check(Method::Post, &at("/api/v3/command")),
                Err(OutboundError::NotListed { .. })
            ));
            let keys: Vec<&str> = a.hosts().map(|(k, _)| k.as_str()).collect();
            assert_eq!(
                keys,
                [
                    "https://media.example.org:443",
                    "https://media.example.org:443/radarr"
                ]
            );
        }
    }

    /// Two media servers behind one host, each under its own path: each takes its own GETs.
    /// A path under neither is refused. So is every other host.
    #[test]
    fn two_servers_behind_one_host_keep_their_own_calls() {
        let mut a = Allowlist::default();
        a.allow_integration(
            "plex",
            "https://media.example.org/plex",
            Calls::default().get("/library/sections"),
        )
        .unwrap();
        a.allow_integration(
            "jellyfin",
            "https://media.example.org/jellyfin",
            Calls::default().get("/Items").get("/Sessions"),
        )
        .unwrap();
        let at = |path: &str| format!("https://media.example.org{path}");
        assert_eq!(rule_of(&a, &at("/plex/library/sections")).unwrap(), "plex");
        assert_eq!(
            rule_of(&a, &at("/jellyfin/Items?Limit=1")).unwrap(),
            "jellyfin"
        );
        assert_eq!(rule_of(&a, &at("/jellyfin/Sessions")).unwrap(), "jellyfin");
        for (path, label) in [
            ("/plex/Items", "plex"),
            ("/jellyfin/library/sections", "jellyfin"),
            ("/jellyfin/Videos/abc/stream", "jellyfin"),
        ] {
            match a.check(Method::Get, &at(path)) {
                Err(OutboundError::NotListed { label: l, .. }) => assert_eq!(l, label, "{path}"),
                other => panic!("{path}: {:?}", other.map(|(_, r)| r.label.clone())),
            }
        }
        // Under neither path: refused. The origin still counts as listed.
        assert!(matches!(
            a.check(Method::Get, &at("/library/sections")),
            Err(OutboundError::NotListed { .. })
        ));
        let out = Outbound::with_transport(a.clone(), Recorded::new());
        assert_eq!(
            out.listed(&at("/jellyfin/Items")).as_deref(),
            Some("jellyfin")
        );
        assert!(out.listed(&at("/elsewhere")).is_some());
        assert!(matches!(
            a.check(
                Method::Get,
                "https://other.example.org/plex/library/sections"
            ),
            Err(OutboundError::HostNotAllowed { .. })
        ));
    }

    /// A GET whose other keys are listed takes those keys alone. A host that takes no GET takes
    /// none. Its listed POSTs still pass.
    #[test]
    fn a_get_can_be_held_to_its_keys_and_a_host_can_take_no_get() {
        const KEY: &str = "0123456789abcdef0123456789abcdef";
        let mut a = Allowlist::default();
        a.allow_integration(
            "usenet",
            "http://10.0.0.70:8080",
            Calls::default().get_only(
                "/api",
                "mode",
                &["queue", "history"],
                &["output", "apikey", "limit"],
            ),
        )
        .unwrap();
        a.allow_integration(
            "rpc",
            "http://10.0.0.71:6789",
            Calls::default()
                .read_when(Method::Post, "/jsonrpc", "/method", &["listgroups"])
                .no_get(),
        )
        .unwrap();
        let at = |rest: &str| format!("http://10.0.0.70:8080/api?{rest}");
        for rest in [
            format!("mode=queue&output=json&apikey={KEY}"),
            format!("mode=history&output=json&limit=100&apikey={KEY}"),
            "mode=queue".to_string(),
        ] {
            assert!(a.check(Method::Get, &at(&rest)).is_ok(), "{rest}");
        }
        for rest in [
            format!("mode=queue&name=delete&value=all&apikey={KEY}"),
            format!("mode=history&name=delete&value=all&apikey={KEY}"),
            format!("mode=queue&output=json&apikey={KEY}&del_files=1"),
            format!("mode=shutdown&apikey={KEY}"),
            format!("mode=queue&mode=pause&apikey={KEY}"),
            format!("output=json&apikey={KEY}"),
        ] {
            let err = a.check(Method::Get, &at(&rest)).unwrap_err();
            assert!(
                matches!(err, OutboundError::NotListed { .. }),
                "{rest} must be refused, got {err:?}"
            );
            assert!(!err.to_string().contains(KEY), "{err}");
        }
        // The listed POST passes; no GET does, a method in the path included.
        let rpc = "http://10.0.0.71:6789/jsonrpc";
        let listgroups = Body::Json(r#"{"method": "listgroups", "params": [0]}"#.to_string());
        assert!(a.check_call(Method::Post, rpc, &listgroups).is_ok());
        for url in [
            rpc.to_string(),
            format!("{rpc}/shutdown"),
            format!("{rpc}/listgroups"),
            "http://10.0.0.71:6789/".to_string(),
        ] {
            assert!(
                matches!(
                    a.check(Method::Get, &url),
                    Err(OutboundError::NotListed { .. })
                ),
                "{url}"
            );
        }
        assert!(
            a.check_call(
                Method::Post,
                &format!("{rpc}/shutdown"),
                &Body::Json(r#"{"method": "listgroups"}"#.to_string())
            )
            .is_err()
        );
        assert_eq!(basic_auth("fxuser", "fxpass"), "Basic Znh1c2VyOmZ4cGFzcw==");
    }

    #[test]
    fn a_listed_read_passes_only_with_the_body_that_makes_it_a_read() {
        let a = integrations();
        let records = format!("{WORKER}/api/v2/cruddb");
        for body in [
            r#"{"data": {"collection": "LibrarySettingsJSONDB", "mode": "getAll"}}"#,
            r#"{"data": {"collection": "FileJSONDB", "mode": "getById", "docID": "/q/a.mkv"}}"#,
        ] {
            let (url, rule) = a
                .check_call(Method::Post, &records, &json(body))
                .unwrap_or_else(|e| panic!("{body}: {e}"));
            assert_eq!(
                (url.path(), rule.label.as_str()),
                ("/api/v2/cruddb", "worker")
            );
        }
        // The same path with another mode, with the mode somewhere else, with a mode that is
        // no text, with a body that is no JSON, as a form, or with no body at all.
        for body in [
            json(
                r#"{"data": {"collection": "FileJSONDB", "mode": "removeOne", "docID": "/q/a.mkv"}}"#,
            ),
            json(r#"{"data": {"collection": "FileJSONDB", "mode": "update"}}"#),
            json(r#"{"data": {"collection": "FileJSONDB"}}"#),
            json(r#"{"mode": "getAll"}"#),
            json(r#"{"data": {"mode": ["getAll"]}}"#),
            json(r#"{"data": {"mode": "getall"}}"#),
            json(r#"{"data": {"mode": "getAll"}"#),
            json(""),
            Body::Form(vec![("mode".to_string(), "getAll".to_string())]),
            Body::None,
        ] {
            let err = a.check_call(Method::Post, &records, &body).unwrap_err();
            assert!(
                matches!(err, OutboundError::NotListed { .. }),
                "{body:?} must be refused, got {err:?}"
            );
        }
        // A mode written twice is refused, whichever comes first: the services differ on
        // which one counts. So is a member written twice anywhere in the body.
        let twice = |first: &str, last: &str| {
            json(&format!(
                r#"{{"data": {{"mode": "{first}", "collection": "FileJSONDB", "mode": "{last}"}}}}"#
            ))
        };
        for (first, last) in [
            ("getAll", "removeOne"),
            ("removeOne", "getAll"),
            ("getAll", "getAll"),
        ] {
            assert!(
                a.check_call(Method::Post, &records, &twice(first, last))
                    .is_err(),
                "{first} then {last}"
            );
        }
        assert!(
            a.check_call(
                Method::Post,
                &records,
                &json(r#"{"data": {"collection": "LibrarySettingsJSONDB", "mode": "getAll", "x": {"y": 1, "y": 2}}}"#)
            )
            .is_err()
        );
        // A read by its path alone takes any body; the method and the path are exact.
        let tables = format!("{WORKER}/api/v2/client/status-tables");
        assert!(
            a.check_call(Method::Post, &tables, &json(r#"{"data": {"start": 0}}"#))
                .is_ok()
        );
        assert!(a.check(Method::Post, &tables).is_ok());
        for (method, url) in [
            (Method::Put, records.clone()),
            (Method::Delete, records.clone()),
            (Method::Post, format!("{records}/")),
            (Method::Post, format!("{WORKER}/api/v2/cruddb2")),
            (
                Method::Post,
                format!("{WORKER}/api/v2/client/status-tables/x"),
            ),
            (Method::Post, format!("{WORKER}/api/v2/delete-file")),
            (Method::Put, tables.clone()),
        ] {
            let body = json(r#"{"data": {"mode": "getAll"}}"#);
            assert!(
                matches!(
                    a.check_call(method, &url, &body),
                    Err(OutboundError::NotListed { .. })
                ),
                "{} {url}",
                method.as_str()
            );
        }
        // A listed read of one host is none of another's.
        assert!(
            a.check_call(
                Method::Post,
                "http://192.0.2.61:7979/api/v2/cruddb",
                &json(r#"{"data": {"mode": "getAll"}}"#)
            )
            .is_err()
        );
    }

    #[test]
    fn an_act_passes_only_while_the_access_is_act() {
        let a = integrations();
        let stage = format!("{BRIDGE}/stage");
        let body = json(r#"{"path": "/library/a.mkv", "flow": "convert"}"#);
        // The access is `read` until its switch says otherwise.
        let switch = a.switch("bridge").expect("the bridge is listed");
        assert_eq!(switch.get(), Access::Read);
        let err = a.check_call(Method::Post, &stage, &body).unwrap_err();
        assert!(matches!(err, OutboundError::NotAllowed { .. }), "{err:?}");
        assert!(err.to_string().contains("may only read bridge"), "{err}");
        // Reading stays possible either way.
        assert!(
            a.check(Method::Get, &format!("{stage}?path=/library/a.mkv"))
                .is_ok()
        );

        switch.set(Access::Act);
        let (url, rule) = a.check_call(Method::Post, &stage, &body).unwrap();
        assert_eq!((url.path(), rule.label.as_str()), ("/stage", "bridge"));
        assert_eq!(rule.access.get(), Access::Act);
        // Acting never turns a host into a free one: any other call is refused as before, and
        // says that it is not a listed call.
        for (method, url) in [
            (Method::Post, format!("{BRIDGE}/strip-subs")),
            (Method::Post, format!("{BRIDGE}/stage/now")),
            (Method::Put, stage.clone()),
            (Method::Delete, stage.clone()),
            (Method::Delete, format!("{BRIDGE}/jobs")),
        ] {
            let err = a.check_call(method, &url, &body).unwrap_err();
            assert!(
                matches!(err, OutboundError::NotListed { .. }),
                "{} {url}: {err:?}",
                method.as_str()
            );
        }
        // One integration's access is not another's: the worker has no act, and an arr none.
        assert!(a.switch("worker").is_none() && a.switch("radarr").is_none());
        assert!(
            a.check_call(Method::Post, &format!("{WORKER}/stage"), &body)
                .is_err()
        );
        assert!(
            a.check_call(Method::Post, "http://192.0.2.61:7979/api/v3/command", &body)
                .is_err()
        );

        // Taken back, it is refused again with the next call.
        switch.set(Access::Read);
        assert!(matches!(
            a.check_call(Method::Post, &stage, &body),
            Err(OutboundError::NotAllowed { .. })
        ));
        // Only a host with an act has a switch: a provider's label or a service's finds none.
        let mut with_service = integrations();
        with_service
            .allow_service("meilisearch", "http://10.0.0.9:7700")
            .unwrap();
        for label in ["meilisearch", "tmdb", "tvdb", "nothing"] {
            assert!(with_service.switch(label).is_none(), "{label}");
        }
        // An integration reached under a path is called below that path, and nowhere else.
        let mut below = Allowlist::default();
        below
            .allow_integration(
                "bridge",
                "https://media.example.org/bridge/",
                Calls::default()
                    .act(Method::Post, "/stage")
                    .read(Method::Post, "/sign-in"),
            )
            .unwrap();
        below.switch("bridge").unwrap().set(Access::Act);
        for (url, passes) in [
            ("https://media.example.org/bridge/stage", true),
            ("https://media.example.org/bridge/sign-in", true),
            ("https://media.example.org/stage", false),
            ("https://media.example.org/other/stage", false),
            ("https://media.example.org/bridge/stage/x", false),
        ] {
            assert_eq!(below.check(Method::Post, url).is_ok(), passes, "{url}");
        }
        assert_eq!(Access::parse(" Act "), Some(Access::Act));
        assert_eq!(Access::parse("read"), Some(Access::Read));
        assert_eq!(Access::parse("write"), None);
        assert_eq!(
            (Access::Read.as_str(), Access::Act.as_str()),
            ("read", "act")
        );
    }

    /// One path that reads and writes (a record store): its reads pass with their bodies, one
    /// narrow act passes with its own body while the access is `act`, and any other body is
    /// no listed call at all, whatever the access.
    #[test]
    fn an_act_with_a_body_rule_is_that_act_and_nothing_else() {
        let mut a = Allowlist::default();
        a.allow_integration(
            "worker",
            WORKER,
            Calls::default()
                .read_when(Method::Post, "/api/v2/cruddb", "/data/mode", &["getAll"])
                .read_with(
                    Method::Post,
                    "/api/v2/reports",
                    BodyRule::new().text("/data/footprintId"),
                )
                .read_with(
                    Method::Post,
                    "/api/v2/stats",
                    BodyRule::new().only("/data", &[]),
                )
                .act_with(
                    Method::Post,
                    "/api/v2/cruddb",
                    BodyRule::new()
                        .one_of("/data/collection", &["LibrarySettingsJSONDB"])
                        .one_of("/data/mode", &["update"])
                        .text("/data/docID")
                        .only("/data/obj", &["flowId"])
                        .text("/data/obj/flowId"),
                )
                .act_with(
                    Method::Post,
                    "/api/v2/toggle",
                    BodyRule::new()
                        .equals("/data/auto", serde_json::json!(false))
                        .flag("/data/status")
                        .texts("/data/files", 1),
                ),
        )
        .unwrap();
        let at = |path: &str| format!("{WORKER}{path}");
        let assign = r#"{"data": {"collection": "LibrarySettingsJSONDB", "mode": "update", "docID": "lib1", "obj": {"flowId": "f2"}}}"#;
        let switch = a.switch("worker").expect("a host with an act has a switch");
        // Under read access the act is known and not allowed; the reads pass.
        assert!(matches!(
            a.check_call(Method::Post, &at("/api/v2/cruddb"), &json(assign)),
            Err(OutboundError::NotAllowed { .. })
        ));
        for (path, body) in [
            ("/api/v2/cruddb", r#"{"data": {"mode": "getAll"}}"#),
            ("/api/v2/reports", r#"{"data": {"footprintId": "QEdffjm"}}"#),
            ("/api/v2/stats", r#"{"data": {}}"#),
        ] {
            assert!(
                a.check_call(Method::Post, &at(path), &json(body)).is_ok(),
                "{path} {body}"
            );
        }
        switch.set(Access::Act);
        assert!(
            a.check_call(Method::Post, &at("/api/v2/cruddb"), &json(assign))
                .is_ok()
        );
        let toggle = r#"{"data": {"auto": false, "status": true, "files": ["/q/a.mkv"]}}"#;
        assert!(
            a.check_call(Method::Post, &at("/api/v2/toggle"), &json(toggle))
                .is_ok()
        );
        // Every other body on the same paths is no listed call, even under act: another
        // collection, another mode, more than the flow, no document, a flag that is text,
        // `auto` true, two files, an empty footprint, a question with a filter.
        for (path, body) in [
            (
                "/api/v2/cruddb",
                assign.replace("LibrarySettingsJSONDB", "FileJSONDB"),
            ),
            (
                "/api/v2/cruddb",
                assign.replace("\"update\"", "\"removeOne\""),
            ),
            (
                "/api/v2/cruddb",
                assign.replace(
                    "{\"flowId\": \"f2\"}",
                    "{\"flowId\": \"f2\", \"folder\": \"/\"}",
                ),
            ),
            (
                "/api/v2/cruddb",
                assign.replace("\"docID\": \"lib1\", ", ""),
            ),
            ("/api/v2/cruddb", assign.replace("\"f2\"", "\"\"")),
            ("/api/v2/toggle", toggle.replace("true", "\"true\"")),
            ("/api/v2/toggle", toggle.replace("false", "true")),
            (
                "/api/v2/toggle",
                toggle.replace("[\"/q/a.mkv\"]", "[\"/q/a.mkv\", \"/q/b.mkv\"]"),
            ),
            ("/api/v2/toggle", toggle.replace("[\"/q/a.mkv\"]", "[]")),
            (
                "/api/v2/reports",
                r#"{"data": {"footprintId": " "}}"#.to_string(),
            ),
            (
                "/api/v2/reports",
                r#"{"data": {"footprintId": 7}}"#.to_string(),
            ),
            (
                "/api/v2/stats",
                r#"{"data": {"libraryId": "lib1"}}"#.to_string(),
            ),
            ("/api/v2/stats", r#"{"data": []}"#.to_string()),
            ("/api/v2/stats", "{}".to_string()),
        ] {
            let err = a
                .check_call(Method::Post, &at(path), &json(&body))
                .unwrap_err();
            assert!(
                matches!(err, OutboundError::NotListed { .. }),
                "{path} {body}: {err:?}"
            );
        }
        // No body, or a form, passes no rule.
        for body in [Body::None, Body::Form(vec![("data".into(), "{}".into())])] {
            assert!(
                a.check_call(Method::Post, &at("/api/v2/stats"), &body)
                    .is_err()
            );
        }
        switch.set(Access::Read);
        assert!(matches!(
            a.check_call(Method::Post, &at("/api/v2/toggle"), &json(toggle)),
            Err(OutboundError::NotAllowed { .. })
        ));
    }

    /// A declared path names a record by its number (`{id}`) or any one segment (`{any}`); an
    /// act can take its choices in the query, each listed key once and nothing else; a body
    /// rule reads ids, whole numbers and lists of objects.
    #[test]
    fn patterns_queries_and_lists_say_exactly_what_passes() {
        assert!(path_is("/q/{id}", "/q/7") && path_is("/q/{id}", "/q/1234567890"));
        for path in [
            "/q/",
            "/q/x",
            "/q/7/8",
            "/q/12345678901",
            "/q/-7",
            "/q",
            "/q/7a",
        ] {
            assert!(!path_is("/q/{id}", path), "{path}");
        }
        assert!(path_is("/c/{any}/{id}", "/c/naming/3") && !path_is("/c/{any}/{id}", "/c//3"));
        assert!(path_under("/b", "/b/x") && path_under("/b", "/b/"));
        assert!(!path_under("/b", "/b") && !path_under("/b", "/bx/y"));
        assert!(path_under("/m/{id}", "/m/3/folder") && !path_under("/m/{id}", "/m/x/folder"));

        let mut a = Allowlist::default();
        a.allow_integration(
            "m",
            "http://10.0.0.5:7878",
            Calls::default()
                .act_query(
                    Method::Delete,
                    "/q/{id}",
                    &[("a", &["true", "false"][..]), ("b", &["true"][..])],
                )
                .act_with(
                    Method::Post,
                    "/c",
                    BodyRule::new()
                        .only("", &["name", "ids", "files"])
                        .ids("/ids", 2)
                        .each(
                            "/files",
                            2,
                            BodyRule::new()
                                .only("", &["path", "n"])
                                .text("/path")
                                .whole("/n"),
                        ),
                )
                .never("/m/{id}"),
        )
        .unwrap();
        let at = |rest: &str| format!("http://10.0.0.5:7878{rest}");
        let delete = |rest: &str| a.check_call(Method::Delete, &at(rest), &Body::None);
        let post = |body: serde_json::Value| {
            a.check_call(Method::Post, &at("/c"), &Body::Json(body.to_string()))
        };
        let good =
            serde_json::json!({"name": "x", "ids": [1, 2], "files": [{"path": "/a", "n": 0}]});
        // Under read access the acts are known and not allowed.
        assert!(matches!(
            delete("/q/7?a=true&b=true"),
            Err(OutboundError::NotAllowed { .. })
        ));
        assert!(matches!(
            post(good.clone()),
            Err(OutboundError::NotAllowed { .. })
        ));
        a.switch("m").unwrap().set(Access::Act);
        for rest in ["/q/7?a=true&b=true", "/q/12?b=true&a=false"] {
            assert!(delete(rest).is_ok(), "{rest}");
        }
        assert!(post(good).is_ok());
        for rest in [
            "/q/7",
            "/q/7?a=true",
            "/q/7?a=true&b=false",
            "/q/7?a=true&b=true&c=1",
            "/q/7?a=true&a=false&b=true",
            "/q/7?a=TRUE&b=true",
            "/q/x?a=true&b=true",
            "/q?a=true&b=true",
        ] {
            assert!(
                matches!(delete(rest), Err(OutboundError::NotListed { .. })),
                "{rest}"
            );
        }
        for body in [
            serde_json::json!({"ids": [1, 2, 3], "files": [{"path": "/a", "n": 0}]}),
            serde_json::json!({"ids": [0], "files": [{"path": "/a", "n": 0}]}),
            serde_json::json!({"ids": [1.0], "files": [{"path": "/a", "n": 0}]}),
            serde_json::json!({"ids": [], "files": [{"path": "/a", "n": 0}]}),
            serde_json::json!({"ids": [1], "files": []}),
            serde_json::json!({"ids": [1], "files": [{"path": "/a"}]}),
            serde_json::json!({"ids": [1], "files": [{"path": "/a", "n": -1}]}),
            serde_json::json!({"ids": [1], "files": [{"path": "/a", "n": 1, "x": 2}]}),
            serde_json::json!({"ids": [1], "files": [{"path": " ", "n": 1}]}),
            serde_json::json!({"ids": [1], "files": [1]}),
            serde_json::json!({"ids": [1], "files": [{"path": "/a", "n": 0}, {"path": "/b", "n": 0}, {"path": "/c", "n": 0}]}),
            serde_json::json!({"ids": [1], "files": [{"path": "/a", "n": 0}], "more": true}),
        ] {
            assert!(
                matches!(post(body.clone()), Err(OutboundError::NotListed { .. })),
                "{body}"
            );
        }
        // A never path with a number is refused for any method; the list beside it is not.
        for method in [Method::Get, Method::Post, Method::Put, Method::Delete] {
            assert!(matches!(
                a.check(method, &at("/m/7")),
                Err(OutboundError::NotListed { .. })
            ));
        }
        assert!(a.check(Method::Get, &at("/m")).is_ok());
        assert!(a.check(Method::Get, &at("/m/lookup")).is_ok());
        // A call can carry a limit of its own.
        let call = OutRequest::get(at("/m")).timeout(Duration::from_secs(120));
        assert_eq!(call.timeout, Some(Duration::from_secs(120)));
        assert_eq!(OutRequest::get(at("/m")).timeout, None);
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
        // A call that never left is no late answer.
        let refused = out
            .send(OutRequest::post(
                "http://192.0.2.61:7979/api/v3/movie",
                Body::None,
            ))
            .await
            .unwrap_err();
        assert!(matches!(refused, OutboundError::NotListed { .. }) && !refused.timed_out());
        assert_eq!(recorded.methods_seen(), vec![Method::Get; 2]);
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
}
