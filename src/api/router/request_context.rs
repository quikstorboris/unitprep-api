//! Identifiers attached to every request's log lines and Sentry events, so
//! a Sentry issue and the Grafana log lines for the same request can be
//! matched up, and so "who was doing what" is answerable from IDs alone.
//!
//! IDs only -- never a name, email or phone. `request_id` comes from the
//! request-id layer; `client_id` / `facility_id` are read off the URL
//! path (`/clients/{id}/facilities/{id}/...`), so no handler has to
//! remember to log them. The signed-in user's ID and roles are added by
//! the `AuthenticatedUser` extractor once a session resolves (see
//! `auth::authenticated_user::record_request_identity`).

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use tower_http::request_id::RequestId;
use tracing::Span;

/// The `client_id` / `facility_id` a request path names, when it names one.
#[derive(Debug, Default, PartialEq)]
pub(super) struct PathIds<'a> {
    pub(super) client_id: Option<&'a str>,
    pub(super) facility_id: Option<&'a str>,
}

/// A canonical 8-4-4-4-12 hex UUID -- the only shape accepted, so route
/// words that follow `clients` (`search`, `new`, `staff`) and anything
/// attacker-supplied can never end up in a label or field.
fn is_uuid(segment: &str) -> bool {
    let groups: Vec<&str> = segment.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, len)| g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub(super) fn path_ids(path: &str) -> PathIds<'_> {
    let mut ids = PathIds::default();
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    while let Some(segment) = segments.next() {
        let slot = match segment {
            "clients" => &mut ids.client_id,
            "facilities" => &mut ids.facility_id,
            _ => continue,
        };
        if let Some(next) = segments.next().filter(|s| is_uuid(s)) {
            *slot = Some(next);
        }
    }
    ids
}

fn request_id(request: &Request) -> &str {
    request
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .unwrap_or("unknown")
}

/// The span every handler and layer below `TraceLayer` runs inside; every
/// `tracing::` call made while handling the request inherits its fields.
/// `user_id` and `roles` start empty and are filled in by the auth
/// extractor.
pub(super) fn make_span(request: &Request) -> Span {
    let ids = path_ids(request.uri().path());
    let span = tracing::info_span!(
        "http_request",
        method = %request.method(),
        path = %request.uri().path(),
        request_id = %request_id(request),
        user_id = tracing::field::Empty,
        roles = tracing::field::Empty,
        client_id = tracing::field::Empty,
        facility_id = tracing::field::Empty,
    );
    if let Some(client_id) = ids.client_id {
        span.record("client_id", client_id);
    }
    if let Some(facility_id) = ids.facility_id {
        span.record("facility_id", facility_id);
    }
    span
}

/// Tags the request's Sentry scope with the same IDs. Must sit inside
/// `NewSentryLayer` (which gives each request its own scope) and inside
/// `SetRequestIdLayer` (which assigns the id read here).
pub(super) async fn sentry_request_context(request: Request, next: Next) -> Response {
    let ids = path_ids(request.uri().path());
    sentry::configure_scope(|scope| {
        scope.set_tag("request_id", request_id(&request));
        if let Some(client_id) = ids.client_id {
            scope.set_tag("client_id", client_id);
        }
        if let Some(facility_id) = ids.facility_id {
            scope.set_tag("facility_id", facility_id);
        }
    });
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
    const B: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

    #[test]
    fn reads_client_and_facility_ids_off_the_path() {
        let path = format!("/clients/{A}/facilities/{B}/clickup/run-tasks");
        assert_eq!(
            path_ids(&path),
            PathIds {
                client_id: Some(A),
                facility_id: Some(B)
            }
        );
    }

    #[test]
    fn a_client_only_path_has_no_facility() {
        let path = format!("/clients/{A}/info");
        assert_eq!(
            path_ids(&path),
            PathIds {
                client_id: Some(A),
                facility_id: None
            }
        );
    }

    #[test]
    fn route_words_and_non_uuids_are_never_taken_as_ids() {
        for path in [
            "/clients/search",
            "/clients/new",
            "/clients/staff",
            "/clients/123/info",
            "/clients/not-a-uuid-at-all-really/info",
            "/dedup/abc",
            "/health",
            "/",
        ] {
            assert_eq!(path_ids(path), PathIds::default(), "{path}");
        }
    }

    #[test]
    fn a_uuid_with_non_hex_characters_is_rejected() {
        let path = "/clients/0f8fad5b-d9cb-469f-a165-70867728950g/info";
        assert_eq!(path_ids(path), PathIds::default());
    }
}
