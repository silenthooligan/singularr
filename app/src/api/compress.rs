//! Compressed answers. A caller that sends `Accept-Encoding` gets gzip or brotli (brotli when
//! it takes both); a caller that does not gets the bytes as they are. The layer sits on the API
//! router and on the web client's files.

use axum::body::HttpBody;
use axum::http::{Response, header};
use tower_http::compression::{CompressionLayer, Predicate};

/// An answer shorter than this, in bytes, travels as it is: it fits one packet either way.
pub const MIN_BYTES: u64 = 1024;

/// Which answers are compressed: text whose whole length is known, of [`MIN_BYTES`] or more.
///
/// An answer that is written as it happens (the event stream) has no length and is never
/// compressed, whatever its type: an encoder would hold its frames back. Images and WOFF fonts
/// are compressed already; an SVG is text, and a TrueType font shrinks like text.
#[derive(Debug, Clone, Copy, Default)]
pub struct Compressible;

impl Predicate for Compressible {
    fn should_compress<B>(&self, response: &Response<B>) -> bool
    where
        B: HttpBody,
    {
        let whole = response
            .body()
            .size_hint()
            .exact()
            .is_some_and(|bytes| bytes >= MIN_BYTES);
        let kind = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        whole && is_text(kind)
    }
}

/// Whether a content type is text that an encoder shrinks.
fn is_text(content_type: &str) -> bool {
    let kind = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match kind.as_str() {
        "text/event-stream" => false,
        "application/json" | "application/javascript" | "image/svg+xml" | "font/ttf" => true,
        other => {
            other.starts_with("text/")
                || (other.starts_with("application/") && other.ends_with("+json"))
        }
    }
}

/// The layer: gzip and brotli, for what [`Compressible`] allows.
pub fn layer() -> CompressionLayer<Compressible> {
    CompressionLayer::new()
        .no_deflate()
        .no_zstd()
        .compress_when(Compressible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn answer(content_type: &str, body: Body) -> Response<Body> {
        Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(body)
            .unwrap()
    }

    fn long() -> Body {
        Body::from(vec![b'a'; MIN_BYTES as usize])
    }

    #[test]
    fn long_text_is_compressed_and_nothing_else() {
        for kind in [
            "application/json",
            "application/problem+json",
            "application/manifest+json",
            "text/html; charset=utf-8",
            "text/css; charset=utf-8",
            "text/javascript; charset=utf-8",
            "Text/Plain",
            "image/svg+xml",
            "font/ttf",
        ] {
            assert!(
                Compressible.should_compress(&answer(kind, long())),
                "{kind}"
            );
        }
        for kind in [
            "image/jpeg",
            "image/png",
            "image/webp",
            "image/x-icon",
            "font/woff2",
            "text/event-stream",
            "application/octet-stream",
            "",
        ] {
            assert!(
                !Compressible.should_compress(&answer(kind, long())),
                "{kind}"
            );
        }
    }

    #[test]
    fn a_short_answer_and_one_without_a_length_travel_as_they_are() {
        let short = Body::from(vec![b'a'; MIN_BYTES as usize - 1]);
        assert!(!Compressible.should_compress(&answer("application/json", short)));
        assert!(!Compressible.should_compress(&answer("application/json", Body::empty())));
        // A body written as it happens has no length, whatever type it states.
        let frame = vec![b'a'; 4096];
        let frames = futures::stream::iter([Ok::<_, std::convert::Infallible>(frame)]);
        let streamed = answer("application/json", Body::from_stream(frames));
        assert!(!Compressible.should_compress(&streamed));
    }
}
