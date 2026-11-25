//! The pages of Singularr Requests, drawn on the server as plain HTML: no script, forms and
//! links only, so the pages work on any phone and the content security policy can forbid
//! every script. Every value that comes from outside goes through [`esc`].

use crate::engines::seerr::{POSTER_SIZE, image_url};
use crate::metadata::tmdb::TmdbTitle;
use crate::requests::model::{FaceMyRequest, FaceRules, FaceSession};
use crate::requests::rules::seasons_text;
use std::sync::LazyLock;

/// Text for an HTML text node or a quoted attribute.
pub fn esc(text: &str) -> String {
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

/// The stylesheet: the face's tokens (`tokens.css`), then its own rules.
pub static STYLESHEET: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n{}",
        include_str!("tokens.css"),
        include_str!("requests.css")
    )
});

/// Where the page is, for the navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Here {
    Discover,
    Search,
    Title,
    MyRequests,
    Other,
}

/// What every signed-in page carries.
pub struct Shell<'a> {
    pub person: &'a FaceSession,
    /// The owner face, linked for the owner only.
    pub owner_url: Option<&'a str>,
    pub here: Here,
}

/// The words of a title's state. A title with no state is not here.
pub fn state_label(state: &str) -> &'static str {
    match state {
        "available" => "Available",
        "processing" => "Processing",
        "requested" => "Requested",
        _ => "",
    }
}

fn state_badge(state: Option<&str>) -> String {
    match state.map(|s| (s, state_label(s))) {
        Some((s, label)) if !label.is_empty() => {
            format!(
                r#" <span class="rq-state" data-state="{}">{label}</span>"#,
                esc(s)
            )
        }
        _ => String::new(),
    }
}

fn document(title: &str, nav: &str, main: &str) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex, nofollow">
<meta name="referrer" content="no-referrer">
<title>{} · Singularr Requests</title>
<meta name="theme-color" content="#0e0d15">
<link rel="icon" href="/brand/favicon.ico" sizes="48x48">
<link rel="icon" href="/brand/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/brand/apple-touch-icon.png">
<link rel="manifest" href="/manifest.webmanifest">
<link rel="stylesheet" href="/assets/requests.css">
</head>
<body>
<header class="rq-top"><a class="rq-brand" href="/"><img src="/brand/requests-dark.svg" alt="Singularr Requests" width="1232" height="178"></a>{nav}</header>
<main class="rq-main" id="main">
{main}
</main>
</body>
</html>
"##,
        esc(title)
    )
}

/// A signed-in page.
pub fn page(shell: &Shell<'_>, title: &str, main: &str) -> String {
    let link = |here: Here, href: &str, text: &str| {
        let current = if shell.here == here {
            r#" aria-current="page""#
        } else {
            ""
        };
        format!(r#"<a href="{href}"{current}>{text}</a>"#)
    };
    let owner = match (shell.person.is_owner, shell.owner_url) {
        (true, Some(url)) => format!(r#"<a href="{}">Singularr</a>"#, esc(url)),
        _ => String::new(),
    };
    let nav = format!(
        r#"<nav class="rq-nav" aria-label="Pages">{}{}{}{owner}<form method="post" action="/signout"><button class="rq-link-button" type="submit">Sign out</button></form></nav>"#,
        link(Here::Discover, "/", "Discover"),
        link(Here::Search, "/search", "Search"),
        link(Here::MyRequests, "/requests", "My requests"),
    );
    document(title, &nav, main)
}

/// A page for a person who is not signed in.
pub fn public_page(title: &str, main: &str) -> String {
    document(title, "", main)
}

/// The sign-in page.
pub fn signin(message: Option<&str>) -> String {
    let note = message
        .map(|m| format!(r#"<p class="rq-error" role="alert">{}</p>"#, esc(m)))
        .unwrap_or_default();
    public_page(
        "Sign in",
        &format!(
            r#"<h1>Sign in</h1>
<p>Ask for movies and shows for the Plex server. Sign in with the Plex account the server is shared with.</p>
{note}
<form method="post" action="/signin"><button class="rq-button" type="submit">Sign in with Plex</button></form>
<p class="rq-facts">Plex opens to confirm it is you, then sends you back here. Singularr keeps your Plex name and id, never your Plex password or token.</p>"#
        ),
    )
}

/// The sign-in page with the share card a link preview shows. A shared address of the portal
/// leads anyone not signed in here, so this is the page a preview reads. `public_url` is the
/// portal's own origin: the card's address must be absolute.
pub fn signin_shared(public_url: &str) -> String {
    let card = format!(
        r#"<meta property="og:site_name" content="Singularr Requests">
<meta property="og:title" content="Singularr Requests">
<meta property="og:type" content="website">
<meta property="og:image" content="{0}/brand/share-card.png">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta property="og:image:alt" content="Singularr Requests">
<meta name="twitter:card" content="summary_large_image">
</head>"#,
        esc(public_url.trim_end_matches('/'))
    );
    signin(None).replacen("</head>", &card, 1)
}

/// What a person just asked for, as the confirmation says it: "You requested Severance, season
/// 2". `None` without an intent to name.
pub fn requested(items: &[FaceMyRequest]) -> Option<String> {
    let r = items
        .iter()
        .filter(|r| r.source == "intent")
        .max_by_key(|r| (r.asked_at, r.id))?;
    let name = r.title.as_deref().unwrap_or("a title");
    let scope = if r.kind == "show" && !r.seasons.is_empty() {
        format!(", {}", seasons_text(&r.seasons))
    } else {
        String::new()
    };
    let quality = if r.is_4k { " in 4K" } else { "" };
    Some(format!("You requested {name}{scope}{quality}"))
}

/// While Plex has not confirmed the sign-in yet: the page asks again by itself.
pub fn waiting() -> String {
    public_page(
        "Waiting for Plex",
        r#"<h1>Waiting for Plex</h1>
<p>Confirm the sign-in in the Plex window. This page checks again every few seconds.</p>
<p><a href="/signin/plex">Check now</a> or <a href="/signin">start again</a>.</p>"#,
    )
    .replacen(
        "<meta name=\"referrer\" content=\"no-referrer\">",
        "<meta name=\"referrer\" content=\"no-referrer\">\n<meta http-equiv=\"refresh\" content=\"3\">",
        1,
    )
}

/// One poster card, linked to its title page.
pub struct Card<'a> {
    pub kind: &'a str,
    pub tmdb_id: i64,
    pub title: &'a str,
    pub year: Option<i32>,
    pub poster_path: Option<&'a str>,
    pub state: Option<&'a str>,
}

/// The address of a title page.
pub fn title_href(kind: &str, tmdb_id: i64) -> String {
    let segment = if kind == "show" { "show" } else { "movie" };
    format!("/{segment}/{tmdb_id}")
}

fn poster(path: Option<&str>, alt: &str) -> String {
    match image_url(path, POSTER_SIZE) {
        Some(url) => format!(
            r#"<img class="rq-poster" src="{}" alt="{}" loading="lazy" width="342" height="513" referrerpolicy="no-referrer">"#,
            esc(&url),
            esc(alt)
        ),
        None => r#"<div class="rq-poster-none" aria-hidden="true"></div>"#.to_string(),
    }
}

pub fn cards(items: &[Card<'_>]) -> String {
    if items.is_empty() {
        return r#"<p class="rq-facts">No titles</p>"#.to_string();
    }
    let mut out = String::from(r#"<ul class="rq-grid">"#);
    for c in items {
        let year = c
            .year
            .map(|y| format!(r#"<div class="rq-card-year">{y}</div>"#))
            .unwrap_or_default();
        out.push_str(&format!(
            r#"<li class="rq-card"><a href="{}">{}<div class="rq-card-title">{}{}</div>{year}</a></li>"#,
            title_href(c.kind, c.tmdb_id),
            poster(c.poster_path, ""),
            esc(c.title),
            state_badge(c.state)
        ));
    }
    out.push_str("</ul>");
    out
}

/// Discover: the lists Singularr holds from Seerr.
pub fn discover(shell: &Shell<'_>, lists: &[(&str, Vec<Card<'_>>)]) -> String {
    let mut main = format!(
        r#"<h1>Discover</h1>
<form class="rq-search" method="get" action="/search" role="search"><input type="search" name="q" aria-label="Search movies and shows" placeholder="Search movies and shows"><button class="rq-button" type="submit">Search</button></form>
{}"#,
        TEST_RUN
    );
    if lists.iter().all(|(_, items)| items.is_empty()) {
        main.push_str(r#"<p class="rq-facts">No lists</p>"#);
    }
    for (label, items) in lists.iter().filter(|(_, items)| !items.is_empty()) {
        main.push_str(&format!("<h2>{}</h2>{}", esc(label), cards(items)));
    }
    page(shell, "Discover", &main)
}

/// Search, with the words searched for and what TMDB found.
pub fn search(
    shell: &Shell<'_>,
    q: &str,
    results: Option<&[Card<'_>]>,
    error: Option<&str>,
) -> String {
    let mut main = format!(
        r#"<h1>Search</h1>
<form class="rq-search" method="get" action="/search" role="search"><input type="search" name="q" value="{}" aria-label="Search movies and shows" placeholder="Search movies and shows" autofocus><button class="rq-button" type="submit">Search</button></form>"#,
        esc(q)
    );
    if let Some(e) = error {
        main.push_str(&format!(
            r#"<p class="rq-error" role="alert">{}</p>"#,
            esc(e)
        ));
    }
    if let Some(results) = results {
        main.push_str(&cards(results));
    }
    page(shell, "Search", &main)
}

/// The plain statement on every request: this is a test run.
pub const TEST_RUN: &str =
    r#"<p class="rq-note"><strong>Test run</strong> Requests are recorded, not sent.</p>"#;

/// One season on a show's page, with its state.
pub struct SeasonRow<'a> {
    pub number: i32,
    pub name: Option<&'a str>,
    pub episodes: i32,
    pub state: Option<&'a str>,
}

/// A title page: overview, state, and the Request button (with the season picker and the
/// season gate's statement for a show).
pub fn title(
    shell: &Shell<'_>,
    t: &TmdbTitle,
    state: Option<&str>,
    seasons: &[SeasonRow<'_>],
    rules: Option<&FaceRules>,
    message: Option<&str>,
) -> String {
    let is_show = matches!(t.kind, crate::catalog::model::TitleKind::Show);
    let kind = if is_show { "show" } else { "movie" };
    let mut facts: Vec<String> = Vec::new();
    if let Some(y) = t.year {
        facts.push(y.to_string());
    }
    if let Some(m) = t.runtime {
        facts.push(format!("{} h {} min", m / 60, m % 60));
    }
    if !t.genres.is_empty() {
        facts.push(t.genres.join(", "));
    }
    let facts = esc(&facts.join(" · "));
    let overview = t
        .overview
        .as_deref()
        .map(|o| format!("<p>{}</p>", esc(o)))
        .unwrap_or_default();
    let note = message
        .map(|m| format!(r#"<p class="rq-note" role="status">{}</p>"#, esc(m)))
        .unwrap_or_default();
    let action = format!("{}/request", title_href(kind, t.tmdb_id));
    let request = if is_show {
        let open: Vec<&SeasonRow<'_>> = seasons.iter().filter(|s| s.state.is_none()).collect();
        let rows: String = seasons
            .iter()
            .map(|s| {
                let name = s
                    .name
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Season {}", s.number));
                let episodes = if s.episodes > 0 {
                    format!(r#" <span class="rq-card-year">{} episodes</span>"#, s.episodes)
                } else {
                    String::new()
                };
                let input = if s.state.is_none() {
                    format!(r#"<input type="checkbox" name="seasons" value="{}" id="season-{}">"#, s.number, s.number)
                } else {
                    format!(r#"<input type="checkbox" disabled id="season-{}">"#, s.number)
                };
                format!(
                    r#"<label class="rq-season" for="season-{}">{input}<span class="rq-season-name">{}{episodes}</span>{}</label>"#,
                    s.number,
                    esc(&name),
                    state_badge(s.state)
                )
            })
            .collect();
        let gate = match (shell.person.is_owner, rules) {
            (true, _) => "No season limit".to_string(),
            (false, Some(r)) if r.gated_sites.is_empty() => "No season limit".to_string(),
            (false, Some(r)) => format!("Season limit: {} per show", r.max_seasons),
            (false, None) => "Season limit applies".to_string(),
        };
        if seasons.is_empty() {
            r#"<p class="rq-facts">No seasons listed</p>"#.to_string()
        } else if open.is_empty() {
            r#"<p class="rq-facts">All seasons available or requested</p>"#.to_string()
        } else {
            // The button names the action and its scope: the one season open, or the ones picked.
            let (rows, button) = if open.len() == 1 {
                let only = format!(
                    r#"<input type="checkbox" name="seasons" value="{0}" id="season-{0}">"#,
                    open[0].number
                );
                let picked = format!(
                    r#"<input type="checkbox" name="seasons" value="{0}" id="season-{0}" checked>"#,
                    open[0].number
                );
                (rows.replacen(&only, &picked, 1), "Request this season")
            } else {
                (rows, "Request selected seasons")
            };
            format!(
                r#"<form method="post" action="{action}"><fieldset class="rq-seasons"><legend>Seasons</legend>{rows}</fieldset><p class="rq-facts">{}</p><button class="rq-button" type="submit">{button}</button></form>"#,
                esc(&gate)
            )
        }
    } else {
        match state {
            Some(s) if !state_label(s).is_empty() => format!(
                r#"<p><button class="rq-button" type="button" disabled>{}</button></p>"#,
                state_label(s)
            ),
            _ => format!(
                r#"<form method="post" action="{action}"><button class="rq-button" type="submit">Request this movie</button></form>"#
            ),
        }
    };
    let main = format!(
        r#"<article class="rq-title">{}<div><h1>{}{}</h1><p class="rq-facts">{facts}</p>{overview}{note}{request}{TEST_RUN}</div></article>"#,
        poster(t.poster_path.as_deref(), &t.title),
        esc(&t.title),
        state_badge(state)
    );
    page(shell, &t.title, &main)
}

/// The words of one of the person's requests.
fn request_status(r: &FaceMyRequest) -> String {
    match (r.source.as_str(), r.status.as_str()) {
        ("intent", "new") => "Requested, checking".to_string(),
        ("intent", "shadow") => match r.gate.as_deref() {
            Some("decline") => "Test run: would be declined".to_string(),
            _ => "Test run: recorded, not sent".to_string(),
        },
        ("intent", _) => "Requested, not checked".to_string(),
        (_, status) => match status {
            "pending" => "Requested".to_string(),
            "approved" => "Approved".to_string(),
            "processing" => "Downloading".to_string(),
            "available" => "Available".to_string(),
            "declined" => "Declined".to_string(),
            "failed" => "Failed".to_string(),
            "removed" => "Removed".to_string(),
            other => other.to_string(),
        },
    }
}

/// My requests: the person's own, newest first.
pub fn my_requests(shell: &Shell<'_>, items: &[FaceMyRequest], message: Option<&str>) -> String {
    let note = message
        .map(|m| format!(r#"<p class="rq-note" role="status">{}</p>"#, esc(m)))
        .unwrap_or_default();
    let list = if items.is_empty() {
        r#"<p class="rq-facts">No requests</p>"#.to_string()
    } else {
        let mut out = String::from(r#"<ul class="rq-list">"#);
        for r in items {
            let name = r.title.clone().unwrap_or_else(|| "A title".to_string());
            let year = r.year.map(|y| format!(" ({y})")).unwrap_or_default();
            let seasons = if r.kind == "show" && !r.seasons.is_empty() {
                format!(", {}", seasons_text(&r.seasons))
            } else {
                String::new()
            };
            let quality = if r.is_4k { ", 4K" } else { "" };
            let link = match r.tmdb_id {
                Some(id) => format!(
                    r#"<a href="{}">{}{}</a>"#,
                    title_href(&r.kind, id),
                    esc(&name),
                    esc(&year)
                ),
                None => format!("{}{}", esc(&name), esc(&year)),
            };
            let reason = r
                .gate_reason
                .as_deref()
                .filter(|_| r.source == "intent")
                .map(|g| format!(r#"<span class="rq-facts">{}</span>"#, esc(g)))
                .unwrap_or_default();
            out.push_str(&format!(
                r#"<li>{link}<span class="rq-facts">{}{}{} · {} · {}</span>{reason}</li>"#,
                if r.kind == "show" { "Show" } else { "Movie" },
                esc(&seasons),
                quality,
                esc(&request_status(r)),
                r.asked_at.format("%-d %b %Y")
            ));
        }
        out.push_str("</ul>");
        out
    };
    page(
        shell,
        "My requests",
        &format!("<h1>My requests</h1>{note}{TEST_RUN}{list}"),
    )
}

/// A page that says one thing.
pub fn message(shell: Option<&Shell<'_>>, heading: &str, text: &str) -> String {
    let main = format!(
        r#"<h1>{}</h1><p>{}</p><p><a href="/">Back to Discover</a></p>"#,
        esc(heading),
        esc(text)
    );
    match shell {
        Some(s) => page(s, heading, &main),
        None => public_page(heading, &main),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn person(owner: bool) -> FaceSession {
        FaceSession {
            plex_account_id: 7,
            display_name: "<b>Friend</b>".to_string(),
            site: "plex".to_string(),
            seerr_user_id: 7,
            expires_at: Utc::now(),
            is_owner: owner,
        }
    }

    #[test]
    fn text_from_outside_is_escaped() {
        assert_eq!(
            esc(r#"<script>"x" & 'y'</script>"#),
            "&lt;script&gt;&quot;x&quot; &amp; &#39;y&#39;&lt;/script&gt;"
        );
        let p = person(false);
        let shell = Shell {
            person: &p,
            owner_url: Some("https://singularr.example.org"),
            here: Here::Search,
        };
        let html = search(&shell, "\"><script>alert(1)</script>", None, None);
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(
            !html.contains("singularr.example.org"),
            "a friend sees no link to the owner face"
        );
        let o = person(true);
        let shell = Shell {
            person: &o,
            owner_url: Some("https://singularr.example.org"),
            here: Here::Search,
        };
        assert!(search(&shell, "", None, None).contains(r#"href="https://singularr.example.org""#));
    }

    #[test]
    fn pages_carry_no_script_and_the_stylesheet_has_the_tokens() {
        let p = person(false);
        let shell = Shell {
            person: &p,
            owner_url: None,
            here: Here::Discover,
        };
        for html in [
            discover(&shell, &[]),
            signin(Some("x")),
            waiting(),
            my_requests(&shell, &[], None),
        ] {
            assert!(!html.contains("<script"), "{html}");
            assert!(html.contains("/assets/requests.css"));
        }
        assert!(STYLESHEET.contains(":root {"));
        assert!(STYLESHEET.contains(r#"font-family: "DM Sans""#));
        assert!(STYLESHEET.contains("--mq-accent: #bba4fd"));
        assert!(STYLESHEET.contains(".rq-grid"));
        assert!(waiting().contains(r#"http-equiv="refresh""#));
    }

    fn a_title(kind: crate::catalog::model::TitleKind) -> TmdbTitle {
        TmdbTitle {
            kind,
            tmdb_id: 42,
            title: "Signal Coast".to_string(),
            year: Some(2026),
            overview: None,
            poster_path: None,
            backdrop_path: None,
            genres: Vec::new(),
            runtime: None,
            seasons: Vec::new(),
        }
    }

    #[test]
    fn the_request_buttons_name_what_they_ask_for() {
        use crate::catalog::model::TitleKind;
        let p = person(false);
        let shell = Shell {
            person: &p,
            owner_url: None,
            here: Here::Discover,
        };
        let movie = title(&shell, &a_title(TitleKind::Movie), None, &[], None, None);
        assert!(movie.contains(">Request this movie<"), "{movie}");
        assert!(movie.contains(r#"src="/brand/requests-dark.svg""#));
        assert!(movie.contains(r#"href="/manifest.webmanifest""#));

        let row = |number, state| SeasonRow {
            number,
            name: None,
            episodes: 8,
            state,
        };
        let one = [row(1, Some("available")), row(2, None)];
        let show = title(&shell, &a_title(TitleKind::Show), None, &one, None, None);
        assert!(show.contains(">Request this season<"), "{show}");
        assert!(
            show.contains(r#"value="2" id="season-2" checked>"#),
            "{show}"
        );
        let two = [row(1, None), row(2, None)];
        let show = title(&shell, &a_title(TitleKind::Show), None, &two, None, None);
        assert!(show.contains(">Request selected seasons<"), "{show}");
        assert!(!show.contains("checked"), "{show}");
    }

    #[test]
    fn a_request_is_confirmed_in_the_person_s_words() {
        let ask = |id, kind: &str, seasons: Vec<i32>, minutes: i64| FaceMyRequest {
            source: "intent".to_string(),
            id,
            kind: kind.to_string(),
            tmdb_id: Some(42),
            seasons,
            title: Some("Signal Coast".to_string()),
            year: Some(2026),
            asked_at: Utc::now() - chrono::Duration::minutes(minutes),
            status: "new".to_string(),
            gate: None,
            gate_reason: None,
            poster_path: None,
            is_4k: false,
        };
        assert_eq!(requested(&[]), None);
        let older = ask(1, "movie", vec![], 30);
        let newest = ask(2, "show", vec![2], 1);
        assert_eq!(
            requested(&[older.clone(), newest]).as_deref(),
            Some("You requested Signal Coast, season 2")
        );
        assert_eq!(
            requested(&[older]).as_deref(),
            Some("You requested Signal Coast")
        );
        let two = ask(3, "show", vec![1, 2], 0);
        assert_eq!(
            requested(&[two]).as_deref(),
            Some("You requested Signal Coast, seasons 1 and 2")
        );
    }

    #[test]
    fn a_title_card_links_to_its_page_with_its_state() {
        let html = cards(&[Card {
            kind: "show",
            tmdb_id: 1396,
            title: "A Show",
            year: Some(2008),
            poster_path: Some("/p.jpg"),
            state: Some("requested"),
        }]);
        assert!(html.contains(r#"href="/show/1396""#));
        assert!(html.contains("https://image.tmdb.org/t/p/w342/p.jpg"));
        assert!(html.contains(">Requested<"));
        assert_eq!(state_label("none"), "");
    }
}
