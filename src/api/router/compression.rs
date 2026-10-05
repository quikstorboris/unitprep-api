//! Response compression (gzip) for the API.
//!
//! The big payloads here are JSON (the dedup report view, client and
//! facility detail, search results, audit-log pages) and CSV exports --
//! all highly repetitive text that gzip typically shrinks by 80-90%, which
//! is most of the transfer time for a user on an ordinary connection.
//!
//! `tower-http`'s default predicate already skips bodies under 32 bytes,
//! images, gRPC and server-sent events. This adds the formats that are
//! ALREADY compressed containers, where gzip would burn CPU for no gain
//! and can slightly grow the body: ZIP, the OOXML formats (the XLSX dedup
//! exports and the DOCX tagger output are ZIP archives inside), and PDF.
//! A response that already carries a `Content-Encoding` is left alone by
//! the layer itself, so a reverse proxy that compresses in front of this
//! service does not get double-compressed output.

use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::compression::CompressionLayer;

pub(super) fn compression_layer() -> CompressionLayer<impl Predicate> {
    CompressionLayer::new().compress_when(
        DefaultPredicate::new()
            .and(NotForContentType::const_new("application/zip"))
            // Prefix match: covers .xlsx and .docx.
            .and(NotForContentType::const_new(
                "application/vnd.openxmlformats-officedocument",
            ))
            .and(NotForContentType::const_new("application/pdf")),
    )
}

#[cfg(test)]
mod tests {
    use axum::http::header;
    use axum::routing::get;
    use axum::Router;

    use super::compression_layer;

    const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
    const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

    /// Compressible filler well over the 32-byte floor.
    fn big_text() -> String {
        "{\"unit\":\"A-101\",\"tenant\":\"Jane Doe\",\"status\":\"occupied\"},".repeat(400)
    }

    async fn serve() -> String {
        let app = Router::new()
            .route(
                "/json",
                get(|| async { ([(header::CONTENT_TYPE, "application/json")], big_text()) }),
            )
            .route(
                "/csv",
                get(|| async { ([(header::CONTENT_TYPE, "text/csv")], big_text()) }),
            )
            .route(
                "/zip",
                get(|| async { ([(header::CONTENT_TYPE, "application/zip")], big_text()) }),
            )
            .route(
                "/xlsx",
                get(|| async { ([(header::CONTENT_TYPE, XLSX)], big_text()) }),
            )
            .route(
                "/docx",
                get(|| async { ([(header::CONTENT_TYPE, DOCX)], big_text()) }),
            )
            .route(
                "/pdf",
                get(|| async { ([(header::CONTENT_TYPE, "application/pdf")], big_text()) }),
            )
            .route(
                "/tiny",
                get(|| async { ([(header::CONTENT_TYPE, "application/json")], "{}") }),
            )
            .route(
                "/preencoded",
                get(|| async {
                    (
                        [
                            (header::CONTENT_TYPE, "application/json"),
                            (header::CONTENT_ENCODING, "identity-marker"),
                        ],
                        big_text(),
                    )
                }),
            )
            .layer(compression_layer());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    /// (content-encoding header, body bytes) for a request that accepts gzip.
    /// This test build of reqwest has no decompression features, so the
    /// bytes are exactly what went over the wire.
    async fn fetch(base: &str, path: &str, accept_gzip: bool) -> (Option<String>, Vec<u8>) {
        let mut request = reqwest::Client::new().get(format!("{base}{path}"));
        if accept_gzip {
            request = request.header(reqwest::header::ACCEPT_ENCODING, "gzip");
        }
        let response = request.send().await.unwrap();
        let encoding = response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .map(|v| v.to_str().unwrap().to_string());
        (encoding, response.bytes().await.unwrap().to_vec())
    }

    #[tokio::test]
    async fn json_and_csv_are_gzipped_when_the_client_accepts_it() {
        let base = serve().await;
        let original = big_text().len();

        for path in ["/json", "/csv"] {
            let (encoding, body) = fetch(&base, path, true).await;
            assert_eq!(encoding.as_deref(), Some("gzip"), "{path} must be gzipped");
            assert_eq!(
                &body[..2],
                &[0x1f, 0x8b],
                "{path} must be a real gzip stream"
            );
            assert!(
                body.len() < original / 5,
                "{path}: {} bytes on the wire vs {original} original -- repetitive text should shrink a lot",
                body.len()
            );
        }
    }

    #[tokio::test]
    async fn nothing_is_compressed_for_a_client_that_does_not_ask() {
        let base = serve().await;
        let (encoding, body) = fetch(&base, "/json", false).await;

        assert_eq!(encoding, None);
        assert_eq!(body.len(), big_text().len());
    }

    #[tokio::test]
    async fn already_compressed_container_formats_are_left_alone() {
        let base = serve().await;

        for path in ["/zip", "/xlsx", "/docx", "/pdf"] {
            let (encoding, body) = fetch(&base, path, true).await;
            assert_eq!(encoding, None, "{path} must not be re-compressed");
            assert_eq!(
                body.len(),
                big_text().len(),
                "{path} body must be untouched"
            );
        }
    }

    #[tokio::test]
    async fn tiny_bodies_are_not_worth_compressing() {
        let base = serve().await;
        let (encoding, body) = fetch(&base, "/tiny", true).await;

        assert_eq!(encoding, None);
        assert_eq!(body, b"{}");
    }

    #[tokio::test]
    async fn a_response_that_already_declares_an_encoding_is_not_double_compressed() {
        let base = serve().await;
        let (encoding, body) = fetch(&base, "/preencoded", true).await;

        assert_eq!(encoding.as_deref(), Some("identity-marker"));
        assert_eq!(body.len(), big_text().len());
    }
}
