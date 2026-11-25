//! The message each type of channel is sent for a notice: Slack blocks, a Discord embed, an
//! email with a text and an HTML part, the webhook's JSON document, and the forms ntfy,
//! Telegram, Pushover and Gotify take. Pure: the service sends what is built here.
//!
//! The webhook's document is the event as `GET /api/v1/journal` gives it (`journal`), with
//! the title card, the facts, the link and the words of the event beside it. It comes with
//! the headers `Singularr-Event` (the event), `Singularr-Delivery` (the delivery's id, when it
//! has one) and, when the channel has a secret, `Authorization: Bearer <secret>`.

use super::model::{
    ChannelType, Checked, EVENTS, Notice, Tone, group_of, label_of, target, tone_of,
};
use crate::core::smtp::{Mail, Mailbox};
use chrono::SecondsFormat;
use serde_json::{Value, json};

/// The words of a link to a title's page.
const OPEN: &str = "Open title";

/// What one channel is sent.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A POST of a JSON document to the channel's one address.
    Post {
        url: String,
        headers: Vec<(String, String)>,
        body: Value,
    },
    Mail(Mail),
}

/// At most `max` characters, with `...` where it was cut.
pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

/// The title with its year, or what the notice is about.
fn about(n: &Notice) -> String {
    match &n.title {
        Some(t) => match t.year {
            Some(year) => format!("{} ({year})", t.title),
            None => t.title.clone(),
        },
        None => n.subject.clone(),
    }
}

/// The first line of every message: `File upgraded: Breaking Bad (2008)`.
pub fn headline(n: &Notice) -> String {
    let label = label_of(&n.event);
    let about = about(n);
    if about.is_empty() {
        label.to_string()
    } else {
        format!("{label}: {about}")
    }
}

/// The body of a message: what happened, else what it is about, else the headline.
fn text_of(n: &Notice) -> String {
    if !n.summary.trim().is_empty() {
        return n.summary.clone();
    }
    let about = about(n);
    if about.is_empty() { headline(n) } else { about }
}

/// The poster's address, when it is one a destination can fetch.
fn poster(n: &Notice) -> Option<&str> {
    n.title
        .as_ref()
        .and_then(|t| t.poster.as_deref())
        .filter(|p| p.starts_with("https://") || p.starts_with("http://"))
}

/// The time in words every destination reads alike.
fn when(n: &Notice) -> String {
    n.at.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// The group's id of an event, for the webhook's document.
fn group_id(event: &str) -> &'static str {
    EVENTS
        .iter()
        .find(|e| e.id == event)
        .map(|e| e.group)
        .unwrap_or("test")
}

/// Text for HTML: `&`, `<`, `>` and quotes escaped.
pub fn html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Text for Slack's mrkdwn: its three control characters escaped.
fn slack_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Text for markdown (Discord, Gotify): every character that marks something, escaped.
fn markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '#' | '[' | ']'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn slack(n: &Notice) -> Value {
    let about = about(n);
    let mut lines: Vec<String> = Vec::new();
    if !about.is_empty() {
        lines.push(match &n.link {
            Some(link) => format!("*<{}|{}>*", slack_text(link), slack_text(&about)),
            None => format!("*{}*", slack_text(&about)),
        });
    }
    if !n.summary.trim().is_empty() {
        lines.push(slack_text(&n.summary));
    }
    if lines.is_empty() {
        lines.push(slack_text(&headline(n)));
    }
    let mut section = json!({
        "type": "section",
        "text": {"type": "mrkdwn", "text": cut(&lines.join("\n"), 3000)},
    });
    if let Some(poster) = poster(n) {
        section["accessory"] =
            json!({"type": "image", "image_url": poster, "alt_text": cut(&about, 2000)});
    }
    let mut blocks = vec![
        json!({"type": "header", "text": {"type": "plain_text", "text": cut(label_of(&n.event), 150)}}),
        section,
    ];
    if !n.facts.is_empty() {
        let fields: Vec<Value> = n
            .facts
            .iter()
            .take(10)
            .map(|f| {
                json!({
                    "type": "mrkdwn",
                    "text": cut(&format!("*{}*\n{}", slack_text(&f.label), slack_text(&f.value)), 2000),
                })
            })
            .collect();
        blocks.push(json!({"type": "section", "fields": fields}));
    }
    blocks.push(json!({
        "type": "context",
        "elements": [{
            "type": "mrkdwn",
            "text": format!(
                "{} · <!date^{}^{{date_short_pretty}} {{time}}|{}>",
                group_of(&n.event),
                n.at.timestamp(),
                when(n)
            ),
        }],
    }));
    json!({"text": cut(&headline(n), 3000), "blocks": blocks})
}

fn discord(n: &Notice) -> Value {
    let mut embed = json!({
        "title": cut(&headline(n), 256),
        "description": cut(&markdown(&text_of(n)), 4096),
        "color": tone_of(&n.event).color(),
        "timestamp": n.at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "footer": {"text": group_of(&n.event)},
    });
    if let Some(link) = &n.link {
        embed["url"] = json!(link);
    }
    if let Some(poster) = poster(n) {
        embed["thumbnail"] = json!({"url": poster});
    }
    if !n.facts.is_empty() {
        embed["fields"] = Value::Array(
            n.facts
                .iter()
                .take(25)
                .map(|f| {
                    json!({
                        "name": cut(&f.label, 256),
                        "value": cut(&markdown(&f.value), 1024),
                        "inline": true,
                    })
                })
                .collect(),
        );
    }
    // No text of a message may ping anyone.
    json!({"embeds": [embed], "allowed_mentions": {"parse": []}})
}

/// The webhook's document.
pub fn webhook(n: &Notice) -> Value {
    json!({
        "event": n.event,
        "label": label_of(&n.event),
        "group": group_id(&n.event),
        "test": n.test,
        "at": n.at,
        "kind": n.kind,
        "summary": n.summary,
        "subject": n.subject,
        "title": n.title,
        "facts": n.facts,
        "link": n.link,
        "journal": n.journal,
    })
}

/// The text of a message where a destination shows plain text: what happened, then the
/// facts, a line each.
fn plain_text(n: &Notice) -> String {
    let mut text = text_of(n);
    for f in &n.facts {
        text.push_str(&format!("\n{}: {}", f.label, f.value));
    }
    text
}

fn ntfy(n: &Notice, topic: &str) -> Value {
    let mut body = json!({
        "topic": topic,
        "title": cut(&headline(n), 250),
        "message": cut(&plain_text(n), 4096),
        "priority": if tone_of(&n.event) == Tone::Bad { 4 } else { 3 },
    });
    if let Some(link) = &n.link {
        body["click"] = json!(link);
    }
    if let Some(poster) = poster(n) {
        body["icon"] = json!(poster);
    }
    body
}

fn telegram(n: &Notice, chat: &str) -> Value {
    let mut text = format!("<b>{}</b>", html(&headline(n)));
    if !n.summary.trim().is_empty() {
        text.push('\n');
        text.push_str(&html(&n.summary));
    }
    if !n.facts.is_empty() {
        text.push('\n');
        for f in &n.facts {
            text.push_str(&format!("\n<b>{}</b>: {}", html(&f.label), html(&f.value)));
        }
    }
    if let Some(link) = &n.link {
        text.push_str(&format!("\n\n<a href=\"{}\">{OPEN}</a>", html(link)));
    }
    let preview = match poster(n) {
        Some(poster) => json!({"url": poster, "prefer_small_media": true}),
        None => json!({"is_disabled": true}),
    };
    json!({
        "chat_id": chat,
        "text": text,
        "parse_mode": "HTML",
        "link_preview_options": preview,
    })
}

fn pushover(n: &Notice, token: &str, user: &str) -> Value {
    let mut message = html(&text_of(n));
    for f in &n.facts {
        message.push_str(&format!("\n<b>{}</b>: {}", html(&f.label), html(&f.value)));
    }
    let mut body = json!({
        "token": token,
        "user": user,
        "title": cut(&headline(n), 250),
        "message": cut(&message, 1024),
        "html": 1,
        "timestamp": n.at.timestamp(),
    });
    if let Some(link) = &n.link {
        body["url"] = json!(link);
        body["url_title"] = json!(OPEN);
    }
    body
}

fn gotify(n: &Notice) -> Value {
    let mut message = markdown(&text_of(n));
    if !n.facts.is_empty() {
        message.push('\n');
        for f in &n.facts {
            message.push_str(&format!(
                "  \n**{}**: {}",
                markdown(&f.label),
                markdown(&f.value)
            ));
        }
    }
    if let Some(link) = &n.link {
        message.push_str(&format!("\n\n[{OPEN}]({link})"));
    }
    let mut shown = json!({});
    if let Some(link) = &n.link {
        shown["click"] = json!({"url": link});
    }
    if let Some(poster) = poster(n) {
        shown["bigImageUrl"] = json!(poster);
    }
    json!({
        "title": cut(&headline(n), 250),
        "message": message,
        "priority": if tone_of(&n.event) == Tone::Bad { 8 } else { 5 },
        "extras": {
            "client::display": {"contentType": "text/markdown"},
            "client::notification": shown,
        },
    })
}

/// The text part of an email.
fn mail_text(n: &Notice) -> String {
    let mut text = format!("{}\n\n{}\n", headline(n), text_of(n));
    if !n.facts.is_empty() {
        text.push('\n');
        for f in &n.facts {
            text.push_str(&format!("{}: {}\n", f.label, f.value));
        }
    }
    if let Some(link) = &n.link {
        text.push_str(&format!("\n{OPEN}: {link}\n"));
    }
    text.push_str(&format!("\n{}, {}\n", group_of(&n.event), when(n)));
    text
}

/// The HTML part of an email: one card, with the poster beside the words when there is one.
fn mail_html(n: &Notice) -> String {
    const MUTED: &str = "color:#7a6a5d";
    let mut facts = String::new();
    for f in &n.facts {
        facts.push_str(&format!(
            "<tr><td style=\"padding:2px 16px 2px 0;{MUTED}\">{}</td><td style=\"padding:2px 0\">{}</td></tr>",
            html(&f.label),
            html(&f.value)
        ));
    }
    let facts = if facts.is_empty() {
        String::new()
    } else {
        format!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" style=\"font-size:13px;line-height:1.5\">{facts}</table>"
        )
    };
    let poster = poster(n)
        .map(|src| {
            format!(
                "<td valign=\"top\" width=\"96\" style=\"padding:0 16px 0 0\"><img src=\"{}\" width=\"80\" alt=\"{}\" style=\"display:block;border-radius:4px\"></td>",
                html(src),
                html(&about(n))
            )
        })
        .unwrap_or_default();
    let link = n
        .link
        .as_ref()
        .map(|href| {
            format!(
                "<p style=\"margin:16px 0 0\"><a href=\"{}\" style=\"display:inline-block;padding:8px 14px;border-radius:4px;background:#5daeff;color:#ffffff;text-decoration:none;font-size:13px\">{OPEN}</a></p>",
                html(href)
            )
        })
        .unwrap_or_default();
    let about = about(n);
    let heading = if about.is_empty() {
        html(label_of(&n.event))
    } else {
        html(&about)
    };
    let summary = if n.summary.trim().is_empty() {
        String::new()
    } else {
        format!(
            "<p style=\"margin:0 0 12px;font-size:14px;line-height:1.5\">{}</p>",
            html(&n.summary)
        )
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
<body style=\"margin:0;padding:24px;background:#f4efe9;color:#2d2421;font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif\">\
<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" width=\"100%\" style=\"max-width:560px;margin:0 auto;background:#ffffff;border:1px solid #e3d9cc;border-radius:6px\"><tr><td style=\"padding:20px\">\
<p style=\"margin:0 0 12px;font-size:12px;{MUTED}\">{group} · {label}</p>\
<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\"><tr>{poster}<td valign=\"top\">\
<h1 style=\"margin:0 0 6px;font-size:18px;line-height:1.3\">{heading}</h1>{summary}{facts}{link}</td></tr></table>\
<p style=\"margin:16px 0 0;font-size:12px;{MUTED}\">{at}</p>\
</td></tr></table></body></html>",
        title = html(&headline(n)),
        group = html(group_of(&n.event)),
        label = html(label_of(&n.event)),
        at = html(&when(n)),
    )
}

fn mail(checked: &Checked, n: &Notice) -> Result<Mail, String> {
    let from = Mailbox::parse(checked.text("from").unwrap_or_default())
        .map_err(|_| "The channel has no sender".to_string())?;
    let to: Vec<Mailbox> = checked
        .list("to")
        .iter()
        .filter_map(|t| Mailbox::parse(t).ok())
        .collect();
    if to.is_empty() {
        return Err("The channel has no recipients".to_string());
    }
    Ok(Mail {
        from,
        to,
        subject: headline(n),
        text: mail_text(n),
        html: Some(mail_html(n)),
    })
}

/// The message a channel of `kind` with these settings is sent for a notice. `delivery` is
/// the delivery's id, which a webhook is told.
pub fn compose(
    kind: ChannelType,
    checked: &Checked,
    n: &Notice,
    delivery: Option<i64>,
) -> Result<Message, String> {
    let need = |key: &str| {
        checked
            .text(key)
            .map(str::to_string)
            .ok_or_else(|| format!("The channel has no {key}"))
    };
    if kind == ChannelType::Email {
        return mail(checked, n).map(Message::Mail);
    }
    let url = target(kind, checked).ok_or_else(|| "The channel has no address".to_string())?;
    let mut headers: Vec<(String, String)> = Vec::new();
    let body = match kind {
        ChannelType::Slack => slack(n),
        ChannelType::Discord => discord(n),
        ChannelType::Webhook => {
            headers.push(("Singularr-Event".to_string(), n.event.clone()));
            if let Some(id) = delivery {
                headers.push(("Singularr-Delivery".to_string(), id.to_string()));
            }
            if let Some(secret) = checked.text("secret") {
                headers.push(("Authorization".to_string(), format!("Bearer {secret}")));
            }
            webhook(n)
        }
        ChannelType::Ntfy => {
            if let Some(token) = checked.text("token") {
                headers.push(("Authorization".to_string(), format!("Bearer {token}")));
            }
            ntfy(n, &need("topic")?)
        }
        ChannelType::Telegram => telegram(n, &need("chat")?),
        ChannelType::Pushover => pushover(n, &need("token")?, &need("user")?),
        ChannelType::Gotify => {
            headers.push(("X-Gotify-Key".to_string(), need("token")?));
            gotify(n)
        }
        ChannelType::Email => return mail(checked, n).map(Message::Mail),
    };
    Ok(Message::Post { url, headers, body })
}

/// What a destination answered, in its own words, on one line: the message of a JSON answer
/// (Telegram's `description`, Gotify's `errorDescription`, Discord's `message`, ntfy's
/// `error`, Pushover's `errors`), else its text.
pub fn answer_words(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let from_json = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(o)) => ["description", "errorDescription", "message", "error"]
            .iter()
            .find_map(|k| o.get(*k).and_then(Value::as_str).map(str::to_string))
            .or_else(|| {
                o.get("errors")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .filter(|s| !s.is_empty())
            }),
        _ => None,
    };
    let words = from_json.unwrap_or_else(|| text.into_owned());
    cut(&words.split_whitespace().collect::<Vec<_>>().join(" "), 300)
}

/// `text` with each secret replaced, so a destination that quotes how it was called gives
/// nothing away.
pub fn scrub(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets.iter().filter(|s| s.len() >= 4) {
        out = out.replace(secret.as_str(), "(secret)");
    }
    out
}

#[cfg(test)]
mod tests;
