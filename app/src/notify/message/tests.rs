use super::*;
use crate::notify::model::{Card, Fact, check_settings, test_notice};

const POSTER: &str = "https://image.tmdb.org/t/p/w342/ggFHVNu6YYI5L9pCfOacjizRGt.jpg";
const LINK: &str = "https://singularr.example.org/title?id=show-1396";
const SUMMARY: &str = "File upgraded: now WEBDL-1080p by GRP, 3.00 GB, score 120, was HDTV-1080p by OLD, 3.00 GB, score 40";

fn fact(label: &str, value: &str) -> Fact {
    Fact {
        label: label.to_string(),
        value: value.to_string(),
    }
}

/// A file of Breaking Bad upgraded: the event every type's message is checked with.
fn notice() -> Notice {
    Notice {
        event: "file.upgraded".to_string(),
        seq: Some(42),
        at: "2026-11-23T18:00:00Z".parse().unwrap(),
        kind: Some("show".to_string()),
        summary: SUMMARY.to_string(),
        subject: "Breaking Bad S01E03".to_string(),
        title: Some(Card {
            id: Some("show-1396".to_string()),
            kind: "show".to_string(),
            title: "Breaking Bad".to_string(),
            year: Some(2008),
            poster: Some(POSTER.to_string()),
        }),
        facts: vec![
            fact("Episode", "S01E03"),
            fact("Quality", "WEBDL-1080p"),
            fact("Size", "3.00 GB"),
        ],
        link: Some(LINK.to_string()),
        journal: json!({"id": "e-42", "seq": 42, "kind": "file.upgraded", "code": "copy_replaced",
                        "summary": SUMMARY, "body": {"title_id": 7, "score_delta": 80}}),
        test: false,
    }
}

fn settings(kind: ChannelType, value: Value) -> Checked {
    let Value::Object(map) = value else {
        panic!("not an object")
    };
    check_settings(kind, Some(&map), None).unwrap()
}

fn post(
    kind: ChannelType,
    value: Value,
    delivery: Option<i64>,
) -> (String, Vec<(String, String)>, Value) {
    match compose(kind, &settings(kind, value), &notice(), delivery).unwrap() {
        Message::Post { url, headers, body } => (url, headers, body),
        Message::Mail(_) => panic!("{} is no mail", kind.as_str()),
    }
}

fn header(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

#[test]
fn slack_gets_blocks_with_the_title_the_facts_and_the_poster() {
    let url = "https://hooks.slack.com/services/T000/B000/xyz";
    let (to, headers, body) = post(ChannelType::Slack, json!({"url": url}), Some(7));
    assert_eq!(to, url);
    assert!(headers.is_empty());
    assert_eq!(
        body,
        json!({
            "text": "File upgraded: Breaking Bad (2008)",
            "blocks": [
                {"type": "header", "text": {"type": "plain_text", "text": "File upgraded"}},
                {"type": "section",
                 "text": {"type": "mrkdwn", "text": format!("*<{LINK}|Breaking Bad (2008)>*\n{SUMMARY}")},
                 "accessory": {"type": "image", "image_url": POSTER, "alt_text": "Breaking Bad (2008)"}},
                {"type": "section", "fields": [
                    {"type": "mrkdwn", "text": "*Episode*\nS01E03"},
                    {"type": "mrkdwn", "text": "*Quality*\nWEBDL-1080p"},
                    {"type": "mrkdwn", "text": "*Size*\n3.00 GB"}
                ]},
                {"type": "context", "elements": [{"type": "mrkdwn",
                    "text": "Library · <!date^1795456800^{date_short_pretty} {time}|2026-11-23 18:00 UTC>"}]}
            ]
        })
    );
}

#[test]
fn discord_gets_an_embed_that_pings_no_one() {
    let url = "https://discord.com/api/webhooks/1/abc";
    let (to, headers, body) = post(ChannelType::Discord, json!({"url": url}), None);
    assert_eq!(to, url);
    assert!(headers.is_empty());
    assert_eq!(
        body,
        json!({
            "embeds": [{
                "title": "File upgraded: Breaking Bad (2008)",
                "description": SUMMARY,
                "color": 0xa9b665,
                "timestamp": "2026-11-23T18:00:00Z",
                "footer": {"text": "Library"},
                "url": LINK,
                "thumbnail": {"url": POSTER},
                "fields": [
                    {"name": "Episode", "value": "S01E03", "inline": true},
                    {"name": "Quality", "value": "WEBDL-1080p", "inline": true},
                    {"name": "Size", "value": "3.00 GB", "inline": true}
                ]
            }],
            "allowed_mentions": {"parse": []}
        })
    );
}

#[test]
fn a_webhook_gets_the_journal_event_and_the_title_card() {
    let url = "https://example.com/hooks/singularr";
    let (to, headers, body) = post(
        ChannelType::Webhook,
        json!({"url": url, "secret": "s3cret-value"}),
        Some(7),
    );
    assert_eq!(to, url);
    assert_eq!(
        headers,
        [
            header("Singularr-Event", "file.upgraded"),
            header("Singularr-Delivery", "7"),
            header("Authorization", "Bearer s3cret-value"),
        ]
    );
    assert_eq!(
        body,
        json!({
            "event": "file.upgraded",
            "label": "File upgraded",
            "group": "library",
            "test": false,
            "at": "2026-11-23T18:00:00Z",
            "kind": "show",
            "summary": SUMMARY,
            "subject": "Breaking Bad S01E03",
            "title": {"id": "show-1396", "kind": "show", "title": "Breaking Bad", "year": 2008, "poster": POSTER},
            "facts": [
                {"label": "Episode", "value": "S01E03"},
                {"label": "Quality", "value": "WEBDL-1080p"},
                {"label": "Size", "value": "3.00 GB"}
            ],
            "link": LINK,
            "journal": notice().journal,
        })
    );
    // Without a secret there is no Authorization header.
    let (_, headers, _) = post(ChannelType::Webhook, json!({"url": url}), None);
    assert_eq!(headers, [header("Singularr-Event", "file.upgraded")]);
}

#[test]
fn ntfy_telegram_pushover_and_gotify_get_their_own_forms() {
    let (to, headers, body) = post(
        ChannelType::Ntfy,
        json!({"server": "https://ntfy.example.org", "topic": "media", "token": "tk_abc"}),
        None,
    );
    assert_eq!(to, "https://ntfy.example.org");
    assert_eq!(headers, [header("Authorization", "Bearer tk_abc")]);
    assert_eq!(
        body,
        json!({
            "topic": "media",
            "title": "File upgraded: Breaking Bad (2008)",
            "message": format!("{SUMMARY}\nEpisode: S01E03\nQuality: WEBDL-1080p\nSize: 3.00 GB"),
            "priority": 3,
            "click": LINK,
            "icon": POSTER
        })
    );

    let (to, headers, body) = post(
        ChannelType::Telegram,
        json!({"token": "123456:ABC-def", "chat": "-1001234567890"}),
        None,
    );
    assert_eq!(to, "https://api.telegram.org/bot123456:ABC-def/sendMessage");
    assert!(headers.is_empty());
    assert_eq!(
        body,
        json!({
            "chat_id": "-1001234567890",
            "text": format!("<b>File upgraded: Breaking Bad (2008)</b>\n{SUMMARY}\n\n<b>Episode</b>: S01E03\n<b>Quality</b>: WEBDL-1080p\n<b>Size</b>: 3.00 GB\n\n<a href=\"{LINK}\">Open title</a>"),
            "parse_mode": "HTML",
            "link_preview_options": {"url": POSTER, "prefer_small_media": true}
        })
    );

    let (to, headers, body) = post(
        ChannelType::Pushover,
        json!({"token": "azGDORePK8gMaC0QOYAMyEEuzJnyUi", "user": "uQiRzpo4DXghDmr9QzzfQu27cmVRsG"}),
        None,
    );
    assert_eq!(to, "https://api.pushover.net/1/messages.json");
    assert!(headers.is_empty());
    assert_eq!(
        body,
        json!({
            "token": "azGDORePK8gMaC0QOYAMyEEuzJnyUi",
            "user": "uQiRzpo4DXghDmr9QzzfQu27cmVRsG",
            "title": "File upgraded: Breaking Bad (2008)",
            "message": format!("{SUMMARY}\n<b>Episode</b>: S01E03\n<b>Quality</b>: WEBDL-1080p\n<b>Size</b>: 3.00 GB"),
            "html": 1,
            "timestamp": 1_795_456_800_i64,
            "url": LINK,
            "url_title": "Open title"
        })
    );

    let (to, headers, body) = post(
        ChannelType::Gotify,
        json!({"server": "https://gotify.example.org/", "token": "AbCdEf123"}),
        None,
    );
    assert_eq!(to, "https://gotify.example.org/message");
    assert_eq!(headers, [header("X-Gotify-Key", "AbCdEf123")]);
    assert_eq!(
        body,
        json!({
            "title": "File upgraded: Breaking Bad (2008)",
            "message": format!("{SUMMARY}\n  \n**Episode**: S01E03  \n**Quality**: WEBDL-1080p  \n**Size**: 3.00 GB\n\n[Open title]({LINK})"),
            "priority": 5,
            "extras": {
                "client::display": {"contentType": "text/markdown"},
                "client::notification": {"click": {"url": LINK}, "bigImageUrl": POSTER}
            }
        })
    );
}

#[test]
fn an_email_has_a_subject_a_text_part_and_an_html_part() {
    let checked = settings(
        ChannelType::Email,
        json!({"host": "smtp.example.org", "from": "Singularr <singularr@example.org>",
               "to": ["owner@example.org", "Zoë <zoe@example.net>"]}),
    );
    let Message::Mail(mail) = compose(ChannelType::Email, &checked, &notice(), None).unwrap()
    else {
        panic!("an email channel is sent mail");
    };
    assert_eq!(mail.from.address, "singularr@example.org");
    let to: Vec<&str> = mail.to.iter().map(|m| m.address.as_str()).collect();
    assert_eq!(to, ["owner@example.org", "zoe@example.net"]);
    assert_eq!(mail.subject, "File upgraded: Breaking Bad (2008)");
    assert_eq!(
        mail.text,
        format!(
            "File upgraded: Breaking Bad (2008)\n\n{SUMMARY}\n\nEpisode: S01E03\nQuality: WEBDL-1080p\nSize: 3.00 GB\n\nOpen title: {LINK}\n\nLibrary, 2026-11-23 18:00 UTC\n"
        )
    );
    let html = mail.html.unwrap();
    let parts: Vec<String> = vec![
        "<title>File upgraded: Breaking Bad (2008)</title>".to_string(),
        "Library · File upgraded</p>".to_string(),
        format!("<img src=\"{POSTER}\" width=\"80\" alt=\"Breaking Bad (2008)\""),
        ">Breaking Bad (2008)</h1>".to_string(),
        format!(">{SUMMARY}</p>"),
        "<td style=\"padding:2px 0\">WEBDL-1080p</td>".to_string(),
        format!("<a href=\"{LINK}\""),
        ">2026-11-23 18:00 UTC</p>".to_string(),
    ];
    for part in &parts {
        assert!(
            html.contains(part.as_str()),
            "{part} is missing from {html}"
        );
    }
    // What a title or a fact holds is text, never markup.
    let mut odd = notice();
    if let Some(t) = odd.title.as_mut() {
        t.title = "Tom & Jerry <script>".to_string();
    }
    odd.facts = vec![fact("Release", "a<b>\"c\"")];
    let Message::Mail(mail) = compose(ChannelType::Email, &checked, &odd, None).unwrap() else {
        panic!("an email channel is sent mail");
    };
    let html = mail.html.unwrap();
    assert!(
        html.contains("Tom &amp; Jerry &lt;script&gt; (2008)"),
        "{html}"
    );
    assert!(html.contains("a&lt;b&gt;&quot;c&quot;"), "{html}");
    assert!(!html.contains("<script>"));
    // A channel without a sender or recipients cannot be composed for.
    let empty = Checked::default();
    assert_eq!(
        compose(ChannelType::Email, &empty, &notice(), None).unwrap_err(),
        "The channel has no sender"
    );
}

#[test]
fn a_test_message_names_the_channel() {
    let at: chrono::DateTime<chrono::Utc> = "2026-11-23T18:00:00Z".parse().unwrap();
    let n = test_notice("Ops", ChannelType::Slack, at);
    assert_eq!(headline(&n), "Test message: Ops");
    let checked = settings(
        ChannelType::Slack,
        json!({"url": "https://hooks.slack.com/services/T0/B0/x"}),
    );
    let Message::Post { body, .. } = compose(ChannelType::Slack, &checked, &n, None).unwrap()
    else {
        panic!("Slack is posted to");
    };
    assert_eq!(body["text"], "Test message: Ops");
    assert_eq!(body["blocks"][0]["text"]["text"], "Test message");
    assert_eq!(body["blocks"][1]["text"]["text"], "*Ops*");
    assert_eq!(body["blocks"][2]["fields"][1]["text"], "*Type*\nSlack");
    assert!(body["blocks"][1].get("accessory").is_none(), "no poster");
    // Discord's description is never empty.
    let discord = settings(
        ChannelType::Discord,
        json!({"url": "https://discord.com/api/webhooks/1/abc"}),
    );
    let Message::Post { body, .. } = compose(ChannelType::Discord, &discord, &n, None).unwrap()
    else {
        panic!("Discord is posted to");
    };
    assert_eq!(body["embeds"][0]["description"], "Ops");
    assert_eq!(body["embeds"][0]["footer"]["text"], "Test");
}

#[test]
fn a_destination_s_answer_is_quoted_in_its_own_words_without_secrets() {
    assert_eq!(answer_words(b"no_service"), "no_service");
    assert_eq!(
        answer_words(br#"{"message": "Invalid Webhook Token", "code": 50027}"#),
        "Invalid Webhook Token"
    );
    assert_eq!(
        answer_words(
            br#"{"ok": false, "error_code": 400, "description": "Bad Request: chat not found"}"#
        ),
        "Bad Request: chat not found"
    );
    assert_eq!(
        answer_words(
            br#"{"status": 0, "errors": ["application token is invalid", "user key is invalid"]}"#
        ),
        "application token is invalid; user key is invalid"
    );
    assert_eq!(answer_words(b"  two\n  lines  "), "two lines");
    assert_eq!(answer_words(&[b'x'; 400]).len(), 300);
    let secrets = vec!["T000/B000/xyz".to_string(), "ab".to_string()];
    assert_eq!(
        scrub("no hook at /services/T000/B000/xyz, ab", &secrets),
        "no hook at /services/(secret), ab"
    );
    assert_eq!(cut("abcdef", 5), "ab...");
    assert_eq!(markdown("a_b*c"), "a\\_b\\*c");
}
