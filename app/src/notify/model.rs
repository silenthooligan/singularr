//! Channels, what they are told about, and what each type of channel asks for. Pure: no IO.
//!
//! - A **type** of channel describes its own fields ([`ChannelType::fields`]): the panel draws
//!   its form from them, so a new type needs no change in the client. A field that is a
//!   secret (a webhook address, a token, a password) is write-only: an answer says whether it
//!   is set, never what it is.
//! - An **event** is what a channel subscribes to ([`EVENTS`]), in groups ([`GROUPS`]). Each is
//!   one or more kinds of the journal ([`event_of`]).
//! - A channel may narrow by **kind**: movies, shows, or both (no kind named). An event about
//!   neither (an integration, the access) reaches every channel that subscribes to it.

use crate::core::smtp::Mailbox;
use crate::core::units;
use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

// Types and their fields -----------------------------------------------------------------------

/// How the panel draws a field: one of the settings kit's types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Url,
    Text,
    Secret,
    Number,
    Select,
    List,
}

impl Input {
    pub fn as_str(self) -> &'static str {
        match self {
            Input::Url => "url",
            Input::Text => "text",
            Input::Secret => "secret",
            Input::Number => "number",
            Input::Select => "select",
            Input::List => "list",
        }
    }
}

/// How the server reads a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// An `http` or `https` address with a host.
    Url,
    /// A host name or an IP address.
    Host,
    /// A port, 1 to 65535.
    Port,
    /// One of the field's options.
    Choice,
    /// `Name <address>`, or an address.
    Mailbox,
    /// One address or more.
    Mailboxes,
    /// Letters, digits, `-` and `_`, at most 64: an ntfy topic.
    Topic,
    /// Text without spaces: a token, a key, a chat.
    Word,
}

/// A value a field holds when nothing was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    Text(&'static str),
    Int(i64),
}

impl Fallback {
    fn json(self) -> Value {
        match self {
            Fallback::Text(text) => json!(text),
            Fallback::Int(n) => json!(n),
        }
    }
}

/// One field of a type of channel.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub input: Input,
    pub shape: Shape,
    pub required: bool,
    pub placeholder: Option<&'static str>,
    pub help: Option<&'static str>,
    /// For a select: each value with its label.
    pub options: &'static [(&'static str, &'static str)],
    pub default: Option<Fallback>,
}

impl Field {
    const fn new(key: &'static str, label: &'static str, input: Input, shape: Shape) -> Field {
        Field {
            key,
            label,
            input,
            shape,
            required: false,
            placeholder: None,
            help: None,
            options: &[],
            default: None,
        }
    }

    const fn required(mut self) -> Field {
        self.required = true;
        self
    }

    const fn placeholder(mut self, text: &'static str) -> Field {
        self.placeholder = Some(text);
        self
    }

    const fn help(mut self, text: &'static str) -> Field {
        self.help = Some(text);
        self
    }

    const fn options(mut self, options: &'static [(&'static str, &'static str)]) -> Field {
        self.options = options;
        self
    }

    const fn default(mut self, value: Fallback) -> Field {
        self.default = Some(value);
        self
    }

    /// Write-only: never in an answer, a journal event or a log line.
    pub fn is_secret(&self) -> bool {
        self.input == Input::Secret
    }
}

const SECURITY: &[(&str, &str)] = &[("none", "None"), ("starttls", "STARTTLS"), ("tls", "TLS")];

const SLACK: &[Field] = &[Field::new("url", "Webhook URL", Input::Secret, Shape::Url)
    .required()
    .placeholder("https://hooks.slack.com/services/...")];

const DISCORD: &[Field] = &[Field::new("url", "Webhook URL", Input::Secret, Shape::Url)
    .required()
    .placeholder("https://discord.com/api/webhooks/...")];

const EMAIL: &[Field] = &[
    Field::new("host", "SMTP host", Input::Text, Shape::Host)
        .required()
        .placeholder("smtp.example.com"),
    Field::new("port", "Port", Input::Number, Shape::Port)
        .required()
        .default(Fallback::Int(587)),
    Field::new("security", "Security", Input::Select, Shape::Choice)
        .required()
        .options(SECURITY)
        .default(Fallback::Text("starttls")),
    Field::new("user", "User", Input::Text, Shape::Word),
    Field::new("password", "Password", Input::Secret, Shape::Word),
    Field::new("from", "From", Input::Text, Shape::Mailbox)
        .required()
        .placeholder("Singularr <singularr@example.com>"),
    Field::new("to", "Recipients", Input::List, Shape::Mailboxes)
        .required()
        .placeholder("name@example.com"),
];

const WEBHOOK: &[Field] = &[
    Field::new("url", "URL", Input::Secret, Shape::Url)
        .required()
        .placeholder("https://example.com/hooks/singularr"),
    Field::new("secret", "Secret", Input::Secret, Shape::Word)
        .help("Sent as a bearer token in the Authorization header"),
];

const NTFY: &[Field] = &[
    Field::new("server", "Server", Input::Url, Shape::Url)
        .required()
        .default(Fallback::Text("https://ntfy.sh")),
    Field::new("topic", "Topic", Input::Text, Shape::Topic).required(),
    Field::new("token", "Access token", Input::Secret, Shape::Word),
];

const TELEGRAM: &[Field] = &[
    Field::new("token", "Bot token", Input::Secret, Shape::Word).required(),
    Field::new("chat", "Chat ID", Input::Text, Shape::Word)
        .required()
        .placeholder("-1001234567890"),
];

const PUSHOVER: &[Field] = &[
    Field::new("token", "Application token", Input::Secret, Shape::Word).required(),
    Field::new("user", "User key", Input::Secret, Shape::Word).required(),
];

const GOTIFY: &[Field] = &[
    Field::new("server", "Server", Input::Url, Shape::Url)
        .required()
        .placeholder("https://gotify.example.com"),
    Field::new("token", "Application token", Input::Secret, Shape::Word).required(),
];

/// Where Telegram's bots post.
pub const TELEGRAM_API: &str = "https://api.telegram.org";
/// Where Pushover takes a message.
pub const PUSHOVER_MESSAGES: &str = "https://api.pushover.net/1/messages.json";

/// A type of channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChannelType {
    Slack,
    Discord,
    Email,
    Webhook,
    Ntfy,
    Telegram,
    Pushover,
    Gotify,
}

impl ChannelType {
    /// In the order the panel lists them.
    pub const ALL: [ChannelType; 8] = [
        ChannelType::Slack,
        ChannelType::Discord,
        ChannelType::Email,
        ChannelType::Webhook,
        ChannelType::Ntfy,
        ChannelType::Telegram,
        ChannelType::Pushover,
        ChannelType::Gotify,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ChannelType::Slack => "slack",
            ChannelType::Discord => "discord",
            ChannelType::Email => "email",
            ChannelType::Webhook => "webhook",
            ChannelType::Ntfy => "ntfy",
            ChannelType::Telegram => "telegram",
            ChannelType::Pushover => "pushover",
            ChannelType::Gotify => "gotify",
        }
    }

    pub fn parse(text: &str) -> Option<ChannelType> {
        let text = text.trim();
        ChannelType::ALL
            .into_iter()
            .find(|t| t.as_str().eq_ignore_ascii_case(text))
    }

    pub fn label(self) -> &'static str {
        match self {
            ChannelType::Slack => "Slack",
            ChannelType::Discord => "Discord",
            ChannelType::Email => "Email",
            ChannelType::Webhook => "Webhook",
            ChannelType::Ntfy => "ntfy",
            ChannelType::Telegram => "Telegram",
            ChannelType::Pushover => "Pushover",
            ChannelType::Gotify => "Gotify",
        }
    }

    pub fn fields(self) -> &'static [Field] {
        match self {
            ChannelType::Slack => SLACK,
            ChannelType::Discord => DISCORD,
            ChannelType::Email => EMAIL,
            ChannelType::Webhook => WEBHOOK,
            ChannelType::Ntfy => NTFY,
            ChannelType::Telegram => TELEGRAM,
            ChannelType::Pushover => PUSHOVER,
            ChannelType::Gotify => GOTIFY,
        }
    }
}

// Events ---------------------------------------------------------------------------------------

/// How an event reads: the color a message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Info,
    Warn,
    Bad,
    Proc,
}

impl Tone {
    /// The theme's color of the tone, as a number: Discord's embed color.
    pub fn color(self) -> u32 {
        match self {
            Tone::Ok => 0xa9b665,
            Tone::Info => 0x5daeff,
            Tone::Warn => 0xd8a657,
            Tone::Bad => 0xe66e6e,
            Tone::Proc => 0xb4a0dc,
        }
    }
}

/// One event a channel can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSpec {
    /// The journal's page word where it has one: `file.upgraded`.
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub tone: Tone,
}

const fn ev(id: &'static str, group: &'static str, label: &'static str, tone: Tone) -> EventSpec {
    EventSpec {
        id,
        group,
        label,
        tone,
    }
}

/// The groups of events, in the panel's order.
pub const GROUPS: &[(&str, &str)] = &[
    ("requests", "Requests"),
    ("downloads", "Downloads"),
    ("library", "Library"),
    ("processing", "Processing"),
    ("system", "System"),
];

/// Every event a channel can subscribe to, by group.
pub const EVENTS: &[EventSpec] = &[
    ev("request.created", "requests", "Request created", Tone::Info),
    ev(
        "request.approved",
        "requests",
        "Request approved",
        Tone::Info,
    ),
    ev(
        "request.declined",
        "requests",
        "Request declined",
        Tone::Warn,
    ),
    ev(
        "request.available",
        "requests",
        "Request available",
        Tone::Ok,
    ),
    ev("request.failed", "requests", "Request failed", Tone::Bad),
    ev("download.added", "downloads", "Download added", Tone::Info),
    ev(
        "download.completed",
        "downloads",
        "Download completed",
        Tone::Ok,
    ),
    ev(
        "download.attention",
        "downloads",
        "Download needs attention",
        Tone::Bad,
    ),
    ev(
        "download.orphaned",
        "downloads",
        "Download orphaned",
        Tone::Warn,
    ),
    ev("title.added", "library", "Title added", Tone::Ok),
    ev("file.added", "library", "File imported", Tone::Ok),
    ev("file.upgraded", "library", "File upgraded", Tone::Ok),
    ev("file.removed", "library", "File removed", Tone::Warn),
    ev("processing.sent", "processing", "Run sent", Tone::Proc),
    ev(
        "processing.finished",
        "processing",
        "Run finished",
        Tone::Proc,
    ),
    ev(
        "processing.failed",
        "processing",
        "Run failed or refused",
        Tone::Bad,
    ),
    ev("processing.stuck", "processing", "Run stuck", Tone::Bad),
    ev(
        "integration.down",
        "system",
        "Integration offline",
        Tone::Bad,
    ),
    ev(
        "integration.up",
        "system",
        "Integration back online",
        Tone::Ok,
    ),
    ev("integration.access", "system", "Access changed", Tone::Warn),
];

/// The event of a test message: no channel subscribes to it.
pub const TEST: &str = "test";

/// The kinds a channel may narrow to.
pub const KINDS: &[(&str, &str)] = &[("movie", "Movies"), ("show", "Shows")];

pub fn event(id: &str) -> Option<&'static EventSpec> {
    EVENTS.iter().find(|e| e.id == id)
}

/// The words of an event: its label, or `Test message`.
pub fn label_of(id: &str) -> &'static str {
    event(id).map(|e| e.label).unwrap_or("Test message")
}

/// The label of the group an event is in.
pub fn group_of(id: &str) -> &'static str {
    event(id)
        .and_then(|e| GROUPS.iter().find(|(g, _)| *g == e.group))
        .map(|(_, label)| *label)
        .unwrap_or("Test")
}

pub fn tone_of(id: &str) -> Tone {
    event(id).map(|e| e.tone).unwrap_or(Tone::Info)
}

/// The event a journal event is, by its stored kind and its body. `None` for a journal event
/// no channel is told about.
pub fn event_of(kind: &str, body: &Value) -> Option<&'static str> {
    let text = |key: &str| body.get(key).and_then(Value::as_str);
    let flag = |key: &str| body.get(key).and_then(Value::as_bool);
    Some(match kind {
        "request_added" => "request.created",
        "request_status_changed" => match text("to")? {
            "approved" => "request.approved",
            "declined" => "request.declined",
            "available" => "request.available",
            "failed" => "request.failed",
            _ => return None,
        },
        "download_added" => "download.added",
        "download_completed" => "download.completed",
        "download_stuck" => "download.attention",
        "download_orphaned" => "download.orphaned",
        // A title one more integration lists is no new title.
        "title_added" if flag("new_to_catalog") != Some(false) => "title.added",
        "copy_added" => "file.added",
        "copy_replaced"
            if body
                .get("score_delta")
                .and_then(Value::as_i64)
                .is_some_and(|d| d > 0) =>
        {
            "file.upgraded"
        }
        "copy_removed" => "file.removed",
        "processing_sent" => "processing.sent",
        "processing_finished" => "processing.finished",
        "processing_failed" => "processing.failed",
        "processing_stuck" => "processing.stuck",
        "engine_down" => "integration.down",
        // An integration that answers for the first time was never away.
        "engine_up" if flag("first_contact") != Some(true) => "integration.up",
        "integration_access_changed" => "integration.access",
        _ => return None,
    })
}

// What a message is made from ------------------------------------------------------------------

/// A title as a message shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Card {
    /// The pages' id: `movie-603`, `show-1396`.
    pub id: Option<String>,
    /// `movie` or `show`.
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
    /// The poster's whole address.
    pub poster: Option<String>,
}

/// One fact a message names: `Quality`, `WEBDL-1080p`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Fact {
    pub label: String,
    pub value: String,
}

impl Fact {
    fn new(label: &str, value: impl Into<String>) -> Fact {
        Fact {
            label: label.to_string(),
            value: value.into(),
        }
    }
}

/// What every type's message is made from: one event, described. It holds no secret, so it
/// is kept with each delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    /// The event ([`EVENTS`]), or [`TEST`].
    pub event: String,
    /// The journal event's number. `None` for a test.
    #[serde(default)]
    pub seq: Option<i64>,
    pub at: DateTime<Utc>,
    /// `movie` or `show`, when the event is about one.
    #[serde(default)]
    pub kind: Option<String>,
    /// What happened, in one line: the journal's summary.
    pub summary: String,
    /// What it is about: `Breaking Bad S01E03`, an integration's name.
    pub subject: String,
    #[serde(default)]
    pub title: Option<Card>,
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// The title's page, when the workspace address is set.
    #[serde(default)]
    pub link: Option<String>,
    /// The event as `GET /api/v1/journal` gives it. Null for a test.
    #[serde(default)]
    pub journal: Value,
    #[serde(default)]
    pub test: bool,
}

/// The notice of a test message to a channel.
pub fn test_notice(channel: &str, kind: ChannelType, at: DateTime<Utc>) -> Notice {
    Notice {
        event: TEST.to_string(),
        seq: None,
        at,
        kind: None,
        summary: String::new(),
        subject: channel.to_string(),
        title: None,
        facts: vec![
            Fact::new("Channel", channel),
            Fact::new("Type", kind.label()),
        ],
        link: None,
        journal: Value::Null,
        test: true,
    }
}

/// A length of time in seconds as the interface writes it: `11 min`, `30 h`, `8 d`.
fn span(seconds: i64) -> String {
    units::span(chrono::Duration::seconds(seconds.max(0)))
}

/// `1`, `1 and 2`, `1, 2 and 3`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The facts a message names for a journal event, from its body, most telling first: the
/// episode, what a file is and its size, who asked, why a download needs attention, what a
/// run did, why an integration is offline.
pub fn facts(kind: &str, body: &Value) -> Vec<Fact> {
    let text = |b: &Value, key: &str| {
        b.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let int = |b: &Value, key: &str| b.get(key).and_then(Value::as_i64);
    let mut out = Vec::new();
    // The episode or the season, for an event about one.
    if let Some(season) = int(body, "season") {
        let episodes: Vec<i64> = body
            .get("episodes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        match episodes.as_slice() {
            [] => out.push(Fact::new("Season", season.to_string())),
            [one] => out.push(Fact::new("Episode", format!("S{season:02}E{one:02}"))),
            [first, .., last] => out.push(Fact::new(
                "Episodes",
                format!("S{season:02}E{first:02}-E{last:02}"),
            )),
        }
    }
    let file = |out: &mut Vec<Fact>, c: &Value| {
        if let Some(q) = text(c, "quality").or_else(|| text(c, "arr_quality")) {
            out.push(Fact::new("Quality", q));
        }
        if let Some(bytes) = int(c, "size_bytes").filter(|b| *b > 0) {
            out.push(Fact::new("Size", units::size(bytes)));
        }
        if let Some(group) = text(c, "group") {
            out.push(Fact::new("Release group", group));
        }
        if let Some(edition) = text(c, "edition") {
            out.push(Fact::new("Edition", edition));
        }
    };
    match kind {
        "copy_added" | "copy_removed" => file(&mut out, body),
        "copy_replaced" => {
            let new = body.get("new").cloned().unwrap_or(Value::Null);
            file(&mut out, &new);
            let old = body.get("old").cloned().unwrap_or(Value::Null);
            if let Some(q) = text(&old, "quality").or_else(|| text(&old, "arr_quality")) {
                out.push(Fact::new("Replaced", q));
            }
        }
        "request_added" | "request_status_changed" => {
            if let Some(by) = text(body, "requested_by") {
                out.push(Fact::new("Requested by", by));
            }
            let seasons: Vec<String> = body
                .get("seasons")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_i64)
                        .map(|n| n.to_string())
                        .collect()
                })
                .unwrap_or_default();
            match seasons.len() {
                0 => {}
                1 => out.push(Fact::new("Season", and_list(&seasons))),
                _ => out.push(Fact::new("Seasons", and_list(&seasons))),
            }
            if body.get("is_4k").and_then(Value::as_bool) == Some(true) {
                out.push(Fact::new("Quality", "4K"));
            }
        }
        "download_added" | "download_completed" | "download_stuck" | "download_orphaned" => {
            if let Some(reason) = text(body, "reason") {
                out.push(Fact::new("Reason", reason));
            }
            if let Some(name) = text(body, "name") {
                out.push(Fact::new("Release", name));
            }
            if let Some(bytes) = int(body, "size_bytes").filter(|b| *b > 0) {
                out.push(Fact::new("Size", units::size(bytes)));
            }
        }
        "processing_sent" | "processing_finished" | "processing_failed" | "processing_stuck" => {
            if let Some(flow) = text(body, "flow_label").or_else(|| text(body, "flow")) {
                out.push(Fact::new("Flow", flow));
            }
            if let Some(file) = text(body, "file") {
                out.push(Fact::new("File", file));
            }
            if let Some(detail) = text(body, "detail") {
                out.push(Fact::new("Reason", detail));
            }
            if let Some(saved) = int(body, "saved").filter(|s| *s > 0) {
                out.push(Fact::new("Saved", units::size(saved)));
            }
            if let Some(took) = int(body, "took") {
                out.push(Fact::new("Took", span(took)));
            }
            if let Some(open) = int(body, "open_for") {
                out.push(Fact::new("Open for", span(open)));
            }
        }
        "engine_down" => {
            if let Some(error) = text(body, "error") {
                out.push(Fact::new("Error", error));
            }
        }
        "engine_up" => {
            if let Some(version) = text(body, "version") {
                out.push(Fact::new("Version", version));
            }
        }
        "integration_access_changed" => {
            if let Some(label) = text(body, "label") {
                out.push(Fact::new("Action", label));
            }
            let to = text(body, "to");
            out.push(Fact::new(
                "Access",
                if to.as_deref() == Some("act") {
                    "Write access"
                } else {
                    "Read-only"
                },
            ));
        }
        _ => {}
    }
    out
}

// Checking what a channel is given -------------------------------------------------------------

/// Why a channel was not taken, by key: `type`, `name`, `events`, `kinds`, `settings.<field>`.
pub type Errors = BTreeMap<String, String>;

/// A channel's settings once checked: the values an answer may show, and the secrets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checked {
    pub settings: Map<String, Value>,
    pub secrets: Map<String, Value>,
}

impl Checked {
    /// A value, secret or not, as text.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.settings
            .get(key)
            .or_else(|| self.secrets.get(key))
            .and_then(Value::as_str)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.settings.get(key).and_then(Value::as_i64)
    }

    pub fn list(&self, key: &str) -> Vec<String> {
        self.settings
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Which secrets of the type are set.
    pub fn set(&self, kind: ChannelType) -> BTreeMap<String, bool> {
        kind.fields()
            .iter()
            .filter(|f| f.is_secret())
            .map(|f| (f.key.to_string(), self.secrets.contains_key(f.key)))
            .collect()
    }

    /// The secret values, to keep out of what a destination's answer is quoted with.
    pub fn secret_values(&self) -> Vec<String> {
        self.secrets
            .values()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    }
}

fn plain(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

/// One value, read by its field's shape. The error is a short phrase for the field.
fn read(field: &Field, value: &Value) -> Result<Value, String> {
    let as_text = || match value {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    match field.shape {
        Shape::Url => {
            let text = as_text().ok_or("Not an address")?;
            let url = Url::parse(&text).map_err(|_| "Not an http or https address")?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err("Not an http or https address".to_string());
            }
            Ok(json!(text))
        }
        Shape::Host => {
            let text = as_text().ok_or("Not a host name")?;
            let ok = !text.is_empty()
                && text.len() <= 253
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
            if ok {
                Ok(json!(text))
            } else {
                Err("Not a host name".to_string())
            }
        }
        Shape::Port => {
            let port = match value {
                Value::Number(n) => n.as_u64(),
                Value::String(s) => s.trim().parse::<u64>().ok(),
                _ => None,
            };
            match port {
                Some(p) if (1..=65_535).contains(&p) => Ok(json!(p)),
                _ => Err("A port from 1 to 65535".to_string()),
            }
        }
        Shape::Choice => {
            let text = as_text().unwrap_or_default().to_ascii_lowercase();
            if field.options.iter().any(|(v, _)| *v == text) {
                Ok(json!(text))
            } else {
                Err("Not one of the choices".to_string())
            }
        }
        Shape::Mailbox => {
            let text = as_text().ok_or("Not an email address")?;
            Mailbox::parse(&text).map_err(|_| "Not an email address".to_string())?;
            Ok(json!(text))
        }
        Shape::Mailboxes => {
            let items: Vec<String> = match value {
                Value::Array(items) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                Value::String(s) => s
                    .split([',', ';', '\n'])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                _ => return Err("Not an email address".to_string()),
            };
            if items.is_empty() {
                return Err("Required".to_string());
            }
            for item in &items {
                if Mailbox::parse(item).is_err() {
                    return Err(format!("Not an email address: {item}"));
                }
            }
            Ok(json!(items))
        }
        Shape::Topic => {
            let text = as_text().unwrap_or_default();
            let ok = !text.is_empty()
                && text.len() <= 64
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
            if ok {
                Ok(json!(text))
            } else {
                Err("Letters, digits, - and _ only".to_string())
            }
        }
        Shape::Word => {
            let text = as_text().unwrap_or_default();
            if text.len() > 512 || !plain(&text) || text.contains(char::is_whitespace) {
                Err("No spaces".to_string())
            } else {
                Ok(json!(text))
            }
        }
    }
}

/// The settings of a channel of `kind`, checked. `sent` is what was given: a field it leaves
/// out keeps what is `stored` (or its default), a secret given as empty text keeps the stored
/// secret (a form never shows one), and `null` empties a field. Errors are by key
/// (`settings.<field>`).
pub fn check_settings(
    kind: ChannelType,
    sent: Option<&Map<String, Value>>,
    stored: Option<&Checked>,
) -> Result<Checked, Errors> {
    let fields = kind.fields();
    let mut errors = Errors::new();
    if let Some(sent) = sent {
        for key in sent.keys() {
            if !fields.iter().any(|f| f.key == key.as_str()) {
                errors.insert(
                    format!("settings.{key}"),
                    "Not a field of this type".to_string(),
                );
            }
        }
    }
    let mut out = Checked::default();
    for field in fields {
        let key = field.key;
        let kept = stored.and_then(|s| {
            if field.is_secret() {
                s.secrets.get(key)
            } else {
                s.settings.get(key)
            }
        });
        let value = match sent.and_then(|s| s.get(key)) {
            None => kept.cloned().or_else(|| field.default.map(Fallback::json)),
            Some(Value::Null) => None,
            Some(Value::String(t)) if t.trim().is_empty() && field.is_secret() => kept.cloned(),
            Some(Value::String(t)) if t.trim().is_empty() => None,
            Some(Value::Array(items)) if items.is_empty() => None,
            Some(given) => Some(given.clone()),
        };
        let Some(value) = value else {
            if field.required {
                errors.insert(format!("settings.{key}"), "Required".to_string());
            }
            continue;
        };
        match read(field, &value) {
            Ok(clean) if field.is_secret() => {
                out.secrets.insert(key.to_string(), clean);
            }
            Ok(clean) => {
                out.settings.insert(key.to_string(), clean);
            }
            Err(why) => {
                errors.insert(format!("settings.{key}"), why);
            }
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// A channel's name: one line, 1 to 80 characters.
pub fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        Err("Required".to_string())
    } else if name.chars().count() > 80 {
        Err("At most 80 characters".to_string())
    } else if !plain(name) {
        Err("One line".to_string())
    } else {
        Ok(name.to_string())
    }
}

/// The events a channel subscribes to, each known, in the order of [`EVENTS`].
pub fn check_events(ids: &[String]) -> Result<Vec<String>, String> {
    if let Some(unknown) = ids.iter().find(|id| event(id.trim()).is_none()) {
        return Err(format!("Not an event: {}", unknown.trim()));
    }
    Ok(EVENTS
        .iter()
        .filter(|e| ids.iter().any(|id| id.trim() == e.id))
        .map(|e| e.id.to_string())
        .collect())
}

/// The kinds a channel narrows to, each known. None: every kind.
pub fn check_kinds(kinds: &[String]) -> Result<Vec<String>, String> {
    if let Some(unknown) = kinds
        .iter()
        .find(|k| !KINDS.iter().any(|(id, _)| *id == k.trim()))
    {
        return Err(format!("Not a kind: {}", unknown.trim()));
    }
    Ok(KINDS
        .iter()
        .filter(|(id, _)| kinds.iter().any(|k| k.trim() == *id))
        .map(|(id, _)| id.to_string())
        .collect())
}

/// Whether a channel that subscribes to `events` and narrows to `kinds` is told about a
/// notice.
pub fn wants(events: &[String], kinds: &[String], notice: &Notice) -> bool {
    events.contains(&notice.event)
        && (kinds.is_empty() || notice.kind.as_ref().is_none_or(|kind| kinds.contains(kind)))
}

/// The one address an HTTP channel posts to. `None` for email, and for settings that lack it.
pub fn target(kind: ChannelType, checked: &Checked) -> Option<String> {
    let trimmed = |key: &str| {
        checked
            .text(key)
            .map(|s| s.trim_end_matches('/').to_string())
    };
    match kind {
        ChannelType::Slack | ChannelType::Discord | ChannelType::Webhook => {
            checked.text("url").map(str::to_string)
        }
        ChannelType::Ntfy => checked.text("server").map(str::to_string),
        ChannelType::Telegram => checked
            .text("token")
            .map(|token| format!("{TELEGRAM_API}/bot{token}/sendMessage")),
        ChannelType::Pushover => Some(PUSHOVER_MESSAGES.to_string()),
        ChannelType::Gotify => trimmed("server").map(|server| format!("{server}/message")),
        ChannelType::Email => None,
    }
}

/// Where a channel sends, in words that hold no secret: a host, a topic, the recipients.
pub fn destination(kind: ChannelType, checked: &Checked) -> String {
    let host = |key: &str| {
        checked
            .text(key)
            .and_then(|u| Url::parse(u).ok())
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default()
    };
    match kind {
        ChannelType::Slack | ChannelType::Discord | ChannelType::Webhook => host("url"),
        ChannelType::Ntfy => format!(
            "{}/{}",
            host("server"),
            checked.text("topic").unwrap_or_default()
        ),
        ChannelType::Telegram => format!("Chat {}", checked.text("chat").unwrap_or_default()),
        ChannelType::Pushover => "Pushover".to_string(),
        ChannelType::Gotify => host("server"),
        ChannelType::Email => {
            let to = checked.list("to");
            let first = to.first().cloned().unwrap_or_default();
            match to.len() {
                0 | 1 => first,
                n => format!("{first} and {} more", n - 1),
            }
        }
    }
}

#[cfg(test)]
mod tests;
