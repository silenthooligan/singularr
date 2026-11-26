//! The way out as a connector sees it: a call ([`OutRequest`]), its answer ([`OutResponse`]), the
//! transport that carries it ([`Transport`]) and the rules it passes before anything is sent
//! ([`Allowlist`]). The decision is pure ([`Allowlist::check_call`]). The app sends the calls.
//!
//! - A call to a host that is not listed is refused before anything is sent.
//! - A host rule is listed by its address: the origin, and the path below which the integration
//!   answers. A call goes by the rule whose path holds its own, whole segments, the longest such
//!   path first: a media server at `https://host/` and a manager at `https://host/radarr` keep
//!   a rule each. A call at a listed origin that no rule's path holds is refused.
//! - The acts are matched first, GET acts included: a call that is a declared act passes only
//!   while its action's access is `act`, whatever its method. Each act names its action
//!   (`grab`, `send`): the id its manifest lists.
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

use crate::error::{OutboundError, TOO_LARGE};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;
use url::Url;

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
    /// A `multipart/form-data` body: a torrent or NZB file upload.
    Multipart(Vec<Part>),
}

/// One part of a multipart body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// A multipart body as it goes on the wire: its content type with the boundary, then its bytes.
/// The boundary is the first `singularr-part-<n>` that no part holds.
pub fn multipart(parts: &[Part]) -> (String, Vec<u8>) {
    let holds = |needle: &[u8]| {
        parts.iter().any(|p| {
            [p.bytes.as_slice(), p.name.as_bytes()]
                .into_iter()
                .any(|hay| hay.windows(needle.len()).any(|w| w == needle))
        })
    };
    let boundary = (0u32..)
        .map(|n| format!("singularr-part-{n}"))
        .find(|b| !holds(b.as_bytes()))
        .unwrap_or_default();
    // A name is written between quotes: a quote or a line break in one is left out.
    let quoted = |s: &str| s.replace(['"', '\r', '\n'], "");
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let mut head = format!(
            "Content-Disposition: form-data; name=\"{}\"",
            quoted(&p.name)
        );
        if let Some(file) = &p.file_name {
            head.push_str(&format!("; filename=\"{}\"", quoted(file)));
        }
        head.push_str("\r\n");
        if let Some(kind) = &p.content_type {
            head.push_str(&format!("Content-Type: {}\r\n", quoted(kind)));
        }
        head.push_str("\r\n");
        out.extend_from_slice(head.as_bytes());
        out.extend_from_slice(&p.bytes);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), out)
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

/// The access of one integration, action by action, as its host rule enforces it. The rule and
/// whoever may change the access hold the same switch. A change takes effect with the next
/// call. A new switch says `read` for every action.
#[derive(Debug, Clone, Default)]
pub struct AccessSwitch(Arc<RwLock<Acting>>);

/// The actions that act: every one, or these alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Acting {
    all: bool,
    actions: std::collections::BTreeSet<String>,
}

impl AccessSwitch {
    fn acting(&self) -> std::sync::RwLockReadGuard<'_, Acting> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn acting_mut(&self) -> std::sync::RwLockWriteGuard<'_, Acting> {
        self.0.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// `act` while one action at least acts.
    pub fn get(&self) -> Access {
        let a = self.acting();
        if a.all || !a.actions.is_empty() {
            Access::Act
        } else {
            Access::Read
        }
    }

    /// Every action at once: `act` lets every act pass, `read` none.
    pub fn set(&self, access: Access) {
        let mut a = self.acting_mut();
        a.all = access == Access::Act;
        a.actions.clear();
    }

    /// These actions act. Every other reads.
    pub fn set_actions<'a>(&self, acting: impl IntoIterator<Item = &'a str>) {
        let mut a = self.acting_mut();
        a.all = false;
        a.actions = acting.into_iter().map(str::to_string).collect();
    }

    /// Whether the act `action` passes.
    pub fn allows(&self, action: &str) -> bool {
        let a = self.acting();
        a.all || a.actions.contains(action)
    }
}

impl PartialEq for AccessSwitch {
    fn eq(&self, other: &Self) -> bool {
        *self.acting() == *other.acting()
    }
}

/// What the environment or the instance file says an integration may do: read, act on every
/// action, or act on these actions alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    Read,
    Act,
    Actions(Vec<String>),
}

impl Grant {
    /// `read`, `act`, or a comma list of action ids, in any case. `None` for an empty word, or
    /// for a list with an id that is no id (a-z, 0-9 and `-`).
    pub fn parse(text: &str) -> Option<Grant> {
        let text = text.trim().to_ascii_lowercase();
        if let Some(access) = Access::parse(&text) {
            return Some(match access {
                Access::Read => Grant::Read,
                Access::Act => Grant::Act,
            });
        }
        let ids: Vec<String> = text
            .split(',')
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect();
        let id_like = |id: &String| {
            id.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        (!ids.is_empty() && ids.iter().all(id_like)).then_some(Grant::Actions(ids))
    }

    /// The access of one action under this grant.
    pub fn of(&self, action: &str) -> Access {
        match self {
            Grant::Act => Access::Act,
            Grant::Actions(ids) if ids.iter().any(|id| id == action) => Access::Act,
            _ => Access::Read,
        }
    }

    /// The words of the grant, as a variable takes them: `read`, `act`, `grab,search`.
    pub fn as_text(&self) -> String {
        match self {
            Grant::Read => "read".to_string(),
            Grant::Act => "act".to_string(),
            Grant::Actions(ids) => ids.join(","),
        }
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
    /// The values it may take, for a rule of listed values.
    pub values: Vec<String>,
    /// What kind of value it takes.
    pub kind: ValueKind,
}

/// What kind of value a query rule takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// One of the rule's values.
    Listed,
    /// Text that is not empty.
    Text,
    /// A whole number of at least 0, in digits alone.
    Number,
    /// `true`, `false`, `1` or `0`.
    Flag,
}

/// What one value of an act's query must be, as an adapter declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryValue {
    OneOf(&'static [&'static str]),
    Text,
    Number,
    Flag,
}

impl QueryRule {
    /// A rule of one of `values`.
    fn listed(key: &str, values: &[&str]) -> QueryRule {
        QueryRule {
            key: key.to_string(),
            values: values.iter().map(|v| v.to_string()).collect(),
            kind: ValueKind::Listed,
        }
    }

    fn of(key: &str, value: QueryValue) -> QueryRule {
        let kind = match value {
            QueryValue::OneOf(values) => return QueryRule::listed(key, values),
            QueryValue::Text => ValueKind::Text,
            QueryValue::Number => ValueKind::Number,
            QueryValue::Flag => ValueKind::Flag,
        };
        QueryRule {
            key: key.to_string(),
            values: Vec::new(),
            kind,
        }
    }

    /// A query that names the key once, with a value of the rule's kind. Named twice or not at
    /// all, it is refused: a service may read either one, or take no value as a value.
    fn allows(&self, url: &Url) -> bool {
        let mut found = url.query_pairs().filter(|(k, _)| k == self.key.as_str());
        match (found.next(), found.next()) {
            (Some((_, value)), None) => match self.kind {
                ValueKind::Listed => self.values.iter().any(|v| *v == value),
                ValueKind::Text => !value.trim().is_empty(),
                ValueKind::Number => {
                    (1..=18).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
                }
                ValueKind::Flag => matches!(value.as_ref(), "true" | "false" | "1" | "0"),
            },
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
    /// The action it is, by the id its manifest lists: `grab`, `send`.
    pub action: &'static str,
    pub method: Method,
    /// The exact path, where a segment `{id}` stands for a record's number.
    pub path: String,
    /// What the body must say for the call to be this act, where one path does more than one
    /// thing. `None`: the path alone says what it does.
    pub body: Option<BodyRule>,
    /// What the query must say, for an act that takes its choices there: every listed key
    /// once, with one of its values, and no other key. `None`: the query is not read.
    pub query: Option<Vec<QueryRule>>,
    /// The parts of a multipart body, by name: each once and no other. `None`: the body is no
    /// multipart body.
    pub parts: Option<Vec<String>>,
}

impl ActCall {
    fn matches(&self, method: Method, url: &Url, body: &Body) -> bool {
        let parts_hold = match (&self.parts, body) {
            (None, Body::Multipart(_)) => false,
            (None, _) => true,
            (Some(names), Body::Multipart(parts)) => {
                parts.len() == names.len()
                    && names
                        .iter()
                        .all(|n| parts.iter().filter(|p| p.name == *n).count() == 1)
            }
            (Some(_), _) => false,
        };
        self.method == method
            && path_is(&self.path, url.path())
            && parts_hold
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
            query: Some(QueryRule::listed(key, values)),
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
            query: Some(QueryRule::listed(key, values)),
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

    /// A call that changes something in the service: the act `action`. Any method, GET
    /// included: a service that takes a command on GET declares its GETs that read beside it.
    pub fn act(mut self, action: &'static str, method: Method, path: &str) -> Calls {
        self.acts.push(ActCall {
            action,
            method,
            path: path.to_string(),
            body: None,
            query: None,
            parts: None,
        });
        self
    }

    /// The act `action`. It is that act only while its JSON body says what `rule` says: for a
    /// path that does more than one thing.
    pub fn act_with(
        mut self,
        action: &'static str,
        method: Method,
        path: &str,
        rule: BodyRule,
    ) -> Calls {
        self.acts.push(ActCall {
            action,
            method,
            path: path.to_string(),
            body: Some(rule),
            query: None,
            parts: None,
        });
        self
    }

    /// The act `action` with its choices in the query. It is that act only while the query
    /// names each of `keys` once with a value of its kind. It names no other key.
    pub fn act_query(
        mut self,
        action: &'static str,
        method: Method,
        path: &str,
        keys: &[(&str, QueryValue)],
    ) -> Calls {
        self.acts.push(ActCall {
            action,
            method,
            path: path.to_string(),
            body: None,
            query: Some(
                keys.iter()
                    .map(|(key, value)| QueryRule::of(key, *value))
                    .collect(),
            ),
            parts: None,
        });
        self
    }

    /// The act `action` with a multipart body of these parts, each once and no other: a
    /// torrent or NZB file upload.
    pub fn act_parts(
        mut self,
        action: &'static str,
        method: Method,
        path: &str,
        parts: &[&str],
    ) -> Calls {
        self.acts.push(ActCall {
            action,
            method,
            path: path.to_string(),
            body: None,
            query: None,
            parts: Some(parts.iter().map(|p| p.to_string()).collect()),
        });
        self
    }
}

/// How an integration is called: the largest answer, the calls at once and per period, the reads
/// sent again, the pause after failures. The time limits stay with the client and the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpProfile {
    /// The largest answer read whole, in bytes. A larger one fails.
    pub max_body: usize,
    /// Calls at once to the integration. Each further call waits its turn.
    pub in_flight: u16,
    /// Calls per period to the integration. `None`: no limit.
    pub rate: Option<Rate>,
    /// For reads alone.
    pub retry: Retry,
    pub breaker: Breaker,
}

/// So many calls per period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub calls: u32,
    pub per: Duration,
}

/// A read that failed (no connection, a broken one, 429 or 502 to 504) is sent again. It goes
/// `attempts` times in all. The wait before each is random: up to `base` doubled for each
/// attempt, at most `cap`. A `Retry-After` the service sends is waited out instead, up to
/// [`RETRY_AFTER_MAX`]. A call with a time limit of its own is a long call. It is sent once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub attempts: u8,
    pub base: Duration,
    pub cap: Duration,
}

/// After `failures` calls in a row that failed, an integration's calls pause for `open_for`. A
/// call fails here with no connection, a read with no answer in time or a 502 to 504. The
/// first call after the pause is a trial. Its success ends the pause. Its failure starts
/// another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Breaker {
    pub failures: u16,
    pub open_for: Duration,
}

/// The longest `Retry-After` waited out.
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(60);

impl HttpProfile {
    pub const DEFAULT: HttpProfile = HttpProfile {
        max_body: 64 << 20,
        in_flight: 8,
        rate: None,
        retry: Retry {
            attempts: 3,
            base: Duration::from_millis(500),
            cap: Duration::from_secs(8),
        },
        breaker: Breaker {
            failures: 5,
            open_for: Duration::from_secs(60),
        },
    };
}

impl Default for HttpProfile {
    fn default() -> Self {
        HttpProfile::DEFAULT
    }
}

impl Retry {
    /// The wait before attempt `attempt` (1 for the second call), for `unit` drawn evenly from
    /// 0 to 1: full jitter.
    pub fn delay(&self, attempt: u32, unit: f64) -> Duration {
        let ceiling = self
            .base
            .saturating_mul(1u32 << attempt.min(16))
            .min(self.cap);
        ceiling.mul_f64(unit.clamp(0.0, 1.0))
    }
}

/// A `Retry-After` header in seconds, at most [`RETRY_AFTER_MAX`]. An HTTP date or any other
/// text is none.
pub fn retry_after(value: Option<&str>) -> Option<Duration> {
    let secs: u64 = value?.trim().parse().ok()?;
    Some(Duration::from_secs(secs).min(RETRY_AFTER_MAX))
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
    /// How it is called.
    pub profile: HttpProfile,
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
            profile: HttpProfile::DEFAULT,
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

    /// Who the host of this URL is listed as: the label of the rule a call there goes by (an
    /// integration, a metadata provider, one of Singularr's own services), else of a rule at its
    /// origin. `None` for an origin without a rule.
    pub fn listed(&self, url: &str) -> Option<String> {
        let parsed = Url::parse(url.trim()).ok()?;
        let key = origin_key(&parsed)?;
        match self.rule_at(&key, parsed.path()) {
            Ok(rule) | Err(Some(rule)) => Some(rule.label.clone()),
            Err(None) => None,
        }
    }

    /// Call the host listed under this label (an integration's id) with this profile.
    pub fn set_profile(&mut self, label: &str, profile: HttpProfile) {
        for rule in self.rules.values_mut().filter(|r| r.label == label) {
            rule.profile = profile;
        }
    }

    /// The profile a host listed under this label is called with. The default one for any
    /// other label.
    pub fn profile_of(&self, label: &str) -> HttpProfile {
        self.rules
            .values()
            .find(|r| r.label == label)
            .map(|r| r.profile)
            .unwrap_or_default()
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
        self.classify(method, url, body)
            .map(|(url, rule, _)| (url, rule))
    }

    /// The whole decision with what the call is: a read, an act with its action, or a call
    /// to one of Singularr's own services. The acts are matched first, GET acts included: a
    /// matched act passes only while the access is `act`.
    pub fn classify(
        &self,
        method: Method,
        url: &str,
        body: &Body,
    ) -> Result<(Url, &HostRule, Class), OutboundError> {
        let parsed = Url::parse(url).map_err(|_| OutboundError::BadUrl(without_query(url)))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(OutboundError::BadUrl(without_query(url)));
        }
        let key = origin_key(&parsed).ok_or_else(|| OutboundError::BadUrl(without_query(url)))?;
        let path = parsed.path().to_string();
        let rule = match self.rule_at(&key, &path) {
            Ok(rule) => rule,
            Err(Some(near)) => {
                return Err(OutboundError::NotListed {
                    method: method.as_str(),
                    host: key,
                    label: near.label.clone(),
                    path,
                });
            }
            Err(None) => return Err(OutboundError::HostNotAllowed { host: key }),
        };
        let barred = rule
            .never
            .iter()
            .any(|never| match never.strip_suffix('/') {
                Some(below) => path_under(below, &path),
                None => path_is(never, &path),
            });
        let refused = |key: String, path: String| OutboundError::NotListed {
            method: method.as_str(),
            host: key,
            label: rule.label.clone(),
            path,
        };
        if barred {
            return Err(refused(key, path));
        }
        if !rule.read_only {
            return Ok((parsed, rule, Class::Own));
        }
        if let Some(act) = rule
            .acts
            .iter()
            .find(|call| call.matches(method, &parsed, body))
        {
            if rule.access.allows(act.action) {
                return Ok((parsed, rule, Class::Act(act.action)));
            }
            return Err(OutboundError::NotAllowed {
                method: method.as_str(),
                host: key,
                label: rule.label.clone(),
                path,
            });
        }
        if method == Method::Get {
            // A host whose adapter declares its GETs takes those alone. One that takes no GET
            // takes none. The refusal names the path and never the query, which may hold a
            // key.
            let declared = rule.gets.is_empty()
                || rule
                    .gets
                    .iter()
                    .any(|call| path_is(&call.path, &path) && call.allows(&parsed));
            if declared && !rule.no_get {
                return Ok((parsed, rule, Class::Read));
            }
            return Err(refused(key, path));
        }
        let reads = rule.reads.iter().any(|call| {
            call.method == method
                && path_is(&call.path, &path)
                && call.body.as_ref().is_none_or(|said| said.allows(body))
        });
        if reads {
            return Ok((parsed, rule, Class::Read));
        }
        Err(refused(key, path))
    }
}

/// Whether calls can be held to what they declare: a host that takes a command on GET declares
/// the GETs that read beside it. Otherwise every other GET to it would pass.
pub fn check_calls(calls: &Calls) -> Result<(), String> {
    check_gets(&calls.acts, &calls.gets, calls.no_get)
}

fn check_gets(acts: &[ActCall], gets: &[GetCall], no_get: bool) -> Result<(), String> {
    let get_acts: Vec<&str> = acts
        .iter()
        .filter(|a| a.method == Method::Get)
        .map(|a| a.action)
        .collect();
    if !get_acts.is_empty() && gets.is_empty() && !no_get {
        return Err(format!(
            "the GET acts {get_acts:?} need the GETs that read declared beside them"
        ));
    }
    Ok(())
}

impl HostRule {
    /// [`check_calls`] for the calls of a listed host.
    pub fn check(&self) -> Result<(), String> {
        check_gets(&self.acts, &self.gets, self.no_get)
    }
}

/// What a call that passed is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// It only reads: it may be sent again after a failure.
    Read,
    /// It is this act. It is sent once.
    Act(&'static str),
    /// A call to one of Singularr's own services, or a notification's post. It is sent once.
    Own,
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

/// The targets, each under the key of who listed it (`channel:7`). Every clone shares them.
/// They change while the server runs: a channel is added, changed and removed in the interface.
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
    pub fn find(&self, method: Method, url: &Url) -> Option<Target> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .find(|target| target.takes(method, url))
            .cloned()
    }
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

    /// Send, then read the answer up to `limit` bytes. A larger one fails in the words
    /// [`TOO_LARGE`]. By default the answer is read whole first, then measured.
    async fn send_capped(
        &self,
        url: Url,
        req: &OutRequest,
        limit: usize,
    ) -> Result<OutResponse, String> {
        let res = self.send(url, req).await?;
        if res.body.len() > limit {
            return Err(format!("{TOO_LARGE}: more than {limit} bytes"));
        }
        Ok(res)
    }

    /// Whether the sender sleeps through its waits: a `Retry-After`, a backoff, the rate's
    /// turn. The recorded transport keeps them instead. A test then does not wait.
    fn sleeps(&self) -> bool {
        true
    }

    /// A wait the sender took before a call.
    fn waited(&self, _wait: Duration) {}
}

#[cfg(test)]
mod tests;
