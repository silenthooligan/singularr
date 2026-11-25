//! The web client: the interface, as static files under `SINGULARR_WEB_DIR` (`web/` in the
//! repository). It is a client of `/api/v1` like any other caller; the server only hands out
//! its files.
//!
//! Pages answer at clean addresses (`/movies` is `movies.html`), an address ending in `.html`
//! moves to the clean one, and the addresses of the earlier pages move to their place in
//! this interface. Only files with a known type are served, only from inside the directory,
//! and never a hidden name or the data tools. Text of some length leaves compressed
//! (`api::compress`), and so does a TrueType font; artwork and WOFF fonts are compressed
//! already and leave as they are.

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

/// What a page may load: itself, artwork from the metadata providers' image hosts, and a
/// trailer frame. Styles may be inline because progress bars and meters set their width in
/// a style attribute; scripts may not.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: https://image.tmdb.org https://artworks.thetvdb.com https://i.ytimg.com; font-src 'self'; \
connect-src 'self'; frame-src https://www.youtube-nocookie.com https://www.youtube.com; \
object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'";

#[derive(Clone)]
struct Client {
    root: Arc<PathBuf>,
}

/// Answers every address the API does not. Use it as the application's fallback.
pub fn router(dir: impl Into<PathBuf>) -> Router {
    Router::new()
        .fallback(serve)
        // Pages, scripts, styles and data leave compressed for a browser that takes that.
        .layer(crate::api::compress::layer())
        .with_state(Client {
            root: Arc::new(dir.into()),
        })
}

/// One file, read.
struct File {
    bytes: Vec<u8>,
    content_type: &'static str,
    tag: String,
}

enum Found {
    File(File),
    /// The address names a directory and lacks the closing slash.
    Directory,
    Nothing,
}

async fn serve(
    State(client): State<Client>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD")],
            "Method not allowed",
        )
            .into_response();
    }
    let Some(path) = decode(uri.path()) else {
        return not_found(&client, &method).await;
    };
    if let Some(to) = moved(&path) {
        return redirect(StatusCode::FOUND, to, uri.query());
    }
    if let Some(to) = clean(&path) {
        return redirect(StatusCode::PERMANENT_REDIRECT, &to, uri.query());
    }
    match read_in_background(&client, path.clone()).await {
        Found::File(file) => respond(file, &method, &headers, StatusCode::OK),
        Found::Directory => redirect(
            StatusCode::PERMANENT_REDIRECT,
            &format!("{path}/"),
            uri.query(),
        ),
        Found::Nothing => not_found(&client, &method).await,
    }
}

async fn read_in_background(client: &Client, path: String) -> Found {
    let root = client.root.clone();
    tokio::task::spawn_blocking(move || read(&root, &path))
        .await
        .unwrap_or(Found::Nothing)
}

/// The page of an address that leads nowhere: the client's own `404.html` when it has one.
async fn not_found(client: &Client, method: &Method) -> Response {
    match read_in_background(client, "/404".to_string()).await {
        Found::File(file) => respond(file, method, &HeaderMap::new(), StatusCode::NOT_FOUND),
        _ => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "Page not found",
        )
            .into_response(),
    }
}

fn redirect(status: StatusCode, to: &str, query: Option<&str>) -> Response {
    let location = match query.filter(|q| !q.is_empty()) {
        Some(q) => format!("{to}?{q}"),
        None => to.to_string(),
    };
    match HeaderValue::from_str(&location) {
        Ok(value) => (status, [(header::LOCATION, value)]).into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

fn respond(file: File, method: &Method, asked: &HeaderMap, status: StatusCode) -> Response {
    let unchanged = status == StatusCode::OK
        && asked
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',')
                    .any(|t| t.trim().trim_start_matches("W/") == file.tag)
            });
    let page = file.content_type.starts_with("text/html");
    // Pages, scripts, styles and data are checked against the server on every load, so a new
    // build of the interface shows at once; artwork and fonts are kept by the browser.
    let lasting = file.content_type.starts_with("image/") || file.content_type.starts_with("font/");
    let mut res = if unchanged {
        StatusCode::NOT_MODIFIED.into_response()
    } else if *method == Method::HEAD {
        status.into_response()
    } else {
        (status, Body::from(file.bytes)).into_response()
    };
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(file.content_type),
    );
    if let Ok(tag) = HeaderValue::from_str(&file.tag) {
        h.insert(header::ETAG, tag);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if lasting {
            "public, max-age=604800"
        } else {
            "no-cache"
        }),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if page {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        );
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        h.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        );
    }
    res
}

/// Addresses of the earlier pages, and where each one lives now.
fn moved(path: &str) -> Option<&'static str> {
    let rel = path.trim_start_matches('/');
    let (first, rest) = rel.split_once('/').unwrap_or((rel, ""));
    match (first, !rest.is_empty()) {
        ("movies" | "title" | "titles", true) => Some("/movies"),
        ("shows", true) => Some("/shows"),
        ("rules", true) if rest.starts_with("lab") => Some("/lab"),
        ("rules", true) => Some("/rules"),
        ("policy" | "shadow", _) => Some("/rules"),
        ("api", false) => Some("/settings"),
        ("home-movies", _) => Some("/"),
        _ => None,
    }
}

/// The clean form of an address that names a page file: `/x.html` is `/x`, `/index.html` is
/// `/`, `/dir/index.html` is `/dir/`.
fn clean(path: &str) -> Option<String> {
    let stem = path.strip_suffix(".html")?;
    Some(match stem.strip_suffix("index") {
        Some(dir) if dir.ends_with('/') => dir.to_string(),
        _ => stem.to_string(),
    })
}

/// The path with its percent escapes resolved; `None` when they do not spell UTF-8.
fn decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = path.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A relative path made of plain names: no step up, no hidden name, no backslash, no drive,
/// and not a folder that is not the workspace's to hand out: the data tools, and the request
/// portal's pages (the request face serves those; here they would be a second interface on
/// sample data).
fn safe(rel: &str) -> bool {
    !rel.contains(['\\', '\0', ':'])
        && !rel.contains("//")
        && rel.split('/').all(|s| !s.starts_with('.'))
        && !["tools", "portal"]
            .iter()
            .any(|dir| rel == *dir || rel.starts_with(&format!("{dir}/")))
}

fn content_type(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => return None,
    })
}

/// The file an address names: the file itself, the page of that name, or a directory's
/// `index.html`. Every candidate is resolved (links included) and must stay inside `root`.
fn read(root: &Path, path: &str) -> Found {
    let rel = path.trim_start_matches('/');
    if !safe(rel) {
        return Found::Nothing;
    }
    let Ok(root) = std::fs::canonicalize(root) else {
        return Found::Nothing;
    };
    let candidates = if rel.is_empty() || rel.ends_with('/') {
        vec![format!("{rel}index.html")]
    } else {
        vec![rel.to_string(), format!("{rel}.html")]
    };
    for candidate in candidates {
        let Ok(full) = std::fs::canonicalize(root.join(&candidate)) else {
            continue;
        };
        if !full.starts_with(&root) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&full) else {
            continue;
        };
        if meta.is_dir() {
            return Found::Directory;
        }
        let Some(content_type) = content_type(&full) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&full) else {
            continue;
        };
        let changed = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        return Found::File(File {
            tag: format!("\"{:x}-{changed:x}\"", bytes.len()),
            bytes,
            content_type,
        });
    }
    Found::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    /// A client directory of its own under the system's temporary directory.
    fn site(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("singularr-client-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["css", "guide", "portal", "tools", ".cache"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let write = |rel: &str, text: &str| std::fs::write(dir.join(rel), text).unwrap();
        write("index.html", "<title>Home</title>");
        write("movies.html", "<title>Movies</title>");
        write("404.html", "<title>Not here</title>");
        write("css/singularr.css", "body{}");
        write("portal/index.html", "<title>Portal</title>");
        write("guide/index.html", "<title>Guide</title>");
        write("tools/build_data.py", "print()");
        write(".cache/tmdb.json", "{}");
        write("NOTES.md", "notes");
        dir
    }

    async fn get(dir: &Path, uri: &str, headers: &[(&str, &str)]) -> Response {
        let mut req = Request::builder().uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        router(dir.to_path_buf())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn bytes(res: Response) -> Vec<u8> {
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    async fn text(res: Response) -> String {
        String::from_utf8(bytes(res).await).unwrap()
    }

    #[test]
    fn page_files_move_to_their_clean_address() {
        assert_eq!(clean("/movies.html").as_deref(), Some("/movies"));
        assert_eq!(clean("/index.html").as_deref(), Some("/"));
        assert_eq!(clean("/portal/index.html").as_deref(), Some("/portal/"));
        assert_eq!(clean("/reindex.html").as_deref(), Some("/reindex"));
        assert_eq!(clean("/movies"), None);
        assert_eq!(clean("/css/singularr.css"), None);
    }

    #[test]
    fn earlier_addresses_move_to_this_interface() {
        assert_eq!(moved("/movies/42"), Some("/movies"));
        assert_eq!(moved("/shows/7"), Some("/shows"));
        assert_eq!(moved("/rules/lab"), Some("/lab"));
        assert_eq!(moved("/rules/shadow"), Some("/rules"));
        assert_eq!(moved("/policy"), Some("/rules"));
        assert_eq!(moved("/home-movies"), Some("/"));
        assert_eq!(moved("/movies"), None);
        assert_eq!(moved("/rules"), None);
        assert_eq!(moved("/"), None);
    }

    #[test]
    fn only_plain_names_are_safe() {
        assert!(safe("movies"));
        assert!(safe("css/singularr.css"));
        assert!(safe("guide/"));
        assert!(!safe("../secret"));
        assert!(!safe("css/../../secret"));
        assert!(!safe(".cache/tmdb.json"));
        assert!(!safe("css/.hidden"));
        assert!(!safe("tools/build_data.py"));
        assert!(!safe("portal/index.html"));
        assert!(!safe("portal"));
        assert!(safe("portals"));
        assert!(!safe("c:/windows"));
        assert!(!safe("a\\b"));
        assert!(!safe("a//b"));
    }

    #[test]
    fn escapes_are_resolved_before_the_checks() {
        assert_eq!(decode("/a%20b").as_deref(), Some("/a b"));
        assert_eq!(decode("/%2e%2e/secret").as_deref(), Some("/../secret"));
        assert_eq!(decode("/%zz"), None);
        assert_eq!(decode("/%f"), None);
        assert_eq!(decode("/%ff"), None);
    }

    #[tokio::test]
    async fn pages_answer_at_clean_addresses() {
        let dir = site("pages");
        let res = get(&dir, "/", &[]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_SECURITY_POLICY], CSP);
        assert_eq!(res.headers()[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
        assert!(text(res).await.contains("Home"));
        let res = get(&dir, "/movies", &[]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(text(res).await.contains("Movies"));
        let res = get(&dir, "/movies.html?sort=year", &[]).await;
        assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(res.headers()[header::LOCATION], "/movies?sort=year");
        let res = get(&dir, "/guide", &[]).await;
        assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(res.headers()[header::LOCATION], "/guide/");
        let res = get(&dir, "/guide/", &[]).await;
        assert!(text(res).await.contains("Guide"));
        // The request portal's pages are the request face's to serve, never the workspace's.
        let res = get(&dir, "/portal/", &[]).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let res = get(&dir, "/css/singularr.css", &[]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(
            res.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/css")
        );
        assert!(!res.headers().contains_key(header::CONTENT_SECURITY_POLICY));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn long_text_leaves_compressed_and_artwork_as_it_is() {
        use std::io::Read;
        let dir = site("compressed");
        let css = ".row { display: flex; gap: 8px }".repeat(200);
        std::fs::write(dir.join("css/long.css"), &css).unwrap();
        std::fs::write(dir.join("poster.png"), vec![7u8; 8192]).unwrap();
        let takes_both = [("accept-encoding", "gzip, deflate, br")];

        // Brotli for a browser that takes both.
        let res = get(&dir, "/css/long.css", &takes_both).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_ENCODING], "br");
        assert!(
            res.headers()[header::VARY]
                .to_str()
                .unwrap()
                .eq_ignore_ascii_case("accept-encoding")
        );
        assert!(res.headers().contains_key(header::ETAG));
        let packed = bytes(res).await;
        assert!(packed.len() * 4 < css.len(), "{} bytes", packed.len());
        let mut back = String::new();
        brotli::Decompressor::new(&packed[..], 4096)
            .read_to_string(&mut back)
            .unwrap();
        assert_eq!(back, css);

        // gzip for one that takes only that.
        let res = get(&dir, "/css/long.css", &[("accept-encoding", "gzip")]).await;
        assert_eq!(res.headers()[header::CONTENT_ENCODING], "gzip");
        let packed = bytes(res).await;
        let mut back = String::new();
        flate2::read::GzDecoder::new(&packed[..])
            .read_to_string(&mut back)
            .unwrap();
        assert_eq!(back, css);

        // The bytes as they are for a caller that does not ask.
        let res = get(&dir, "/css/long.css", &[]).await;
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(text(res).await, css);

        // Artwork is compressed already, and a short file gains nothing.
        let res = get(&dir, "/poster.png", &takes_both).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(bytes(res).await.len(), 8192);
        let res = get(&dir, "/css/singularr.css", &takes_both).await;
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(text(res).await, "body{}");

        // An unchanged file is still not sent again.
        let res = get(&dir, "/css/long.css", &takes_both).await;
        let tag = res.headers()[header::ETAG].to_str().unwrap().to_string();
        let res = get(
            &dir,
            "/css/long.css",
            &[("accept-encoding", "br"), ("if-none-match", &tag)],
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert!(bytes(res).await.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn nothing_outside_the_client_is_served() {
        let dir = site("confined");
        std::fs::write(
            dir.parent().unwrap().join("singularr-client-secret.html"),
            "secret",
        )
        .unwrap();
        for uri in [
            "/../singularr-client-secret",
            "/%2e%2e/singularr-client-secret",
            "/css/..%2f..%2fsingularr-client-secret",
            "/.cache/tmdb.json",
            "/tools/build_data.py",
            "/NOTES.md",
            "/nothing",
        ] {
            let res = get(&dir, uri, &[]).await;
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(text(res).await.contains("Not here"), "{uri}");
        }
        let _ = std::fs::remove_file(dir.parent().unwrap().join("singularr-client-secret.html"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_unchanged_file_is_not_sent_again_and_writes_are_refused() {
        let dir = site("tags");
        let res = get(&dir, "/movies", &[]).await;
        let tag = res.headers()[header::ETAG].to_str().unwrap().to_string();
        let res = get(&dir, "/movies", &[("if-none-match", &tag)]).await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert!(text(res).await.is_empty());
        let res = router(dir.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/movies")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
        let res = get(&dir, "/movies/42", &[]).await;
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(res.headers()[header::LOCATION], "/movies");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_identity_s_files_have_their_types() {
        for (file, kind) in [
            ("css/fonts/DMSans.ttf", "font/ttf"),
            ("css/fonts/DMSans-OFL.txt", "text/plain; charset=utf-8"),
            ("brand/favicon.ico", "image/x-icon"),
            ("brand/horizon-dark.svg", "image/svg+xml"),
            ("brand/icon-512.png", "image/png"),
            ("singularr.webmanifest", "application/manifest+json"),
        ] {
            assert_eq!(content_type(Path::new(file)), Some(kind), "{file}");
        }
        assert_eq!(content_type(Path::new("web/tools/build_data.py")), None);
    }
}
