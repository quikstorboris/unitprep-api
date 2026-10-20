//! Sentry error reporting: off unless `SENTRY_DSN` is set, so dev and CI
//! behave exactly as before without it.
//!
//! Three things reach Sentry: panics (the SDK's own panic hook, which
//! carries a real stack trace), every `tracing::error!` event (through the
//! layer `logging::init` installs), and the per-request context the tower
//! layers in `api::router` attach. WARN/INFO lines become breadcrumbs on
//! whichever event follows them.
//!
//! PII: this API handles tenant names, emails and phone numbers, so
//! `send_default_pii` stays off and `before_send` strips everything that
//! could carry them out of the request (body, cookies, query string,
//! credential headers). Error text from `tracing` fields is kept -- it is
//! what makes an issue diagnosable -- so never log a tenant value in an
//! `error!` field.

use sentry::protocol::Event;
use sentry::{ClientInitGuard, ClientOptions};

/// Headers that carry credentials or a session; everything else
/// (user-agent, content-type, request id) is harmless and useful.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "x-csrf-token",
    "x-api-key",
    "proxy-authorization",
];

/// Events the panic integration already reports with a stack trace, so
/// the matching `tracing::error!` lines (the panic hook in
/// `logging::init`, and `api::router::handle_panic`) would only duplicate
/// them as stackless issues.
const DUPLICATE_PANIC_MESSAGES: &[&str] = &["panicked", "request handler panicked"];

/// `None` when `SENTRY_DSN` is unset or empty. The returned guard flushes
/// queued events when dropped, so `startup::run` holds it for the life of
/// the process.
pub(super) fn init() -> Option<ClientInitGuard> {
    let dsn = std::env::var("SENTRY_DSN").ok().filter(|d| !d.is_empty())?;

    let environment = std::env::var("SENTRY_ENVIRONMENT")
        .ok()
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "development".to_string());

    let mut options = ClientOptions::new()
        .dsn(dsn.as_str())
        .release(format!("unitprep-api@{}", env!("CARGO_PKG_VERSION")))
        .environment(environment)
        .send_default_pii(false)
        .before_send(scrub);

    // Errors only unless SENTRY_TRACES_SAMPLE_RATE (0.0-1.0, e.g. 0.1) is
    // set; an unparseable or out-of-range value is ignored, not a panic.
    if let Some(rate) = std::env::var("SENTRY_TRACES_SAMPLE_RATE")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|r| (0.0..=1.0).contains(r))
    {
        options = options.traces_sample_rate(rate);
    }

    let guard = sentry::init(options);
    // An unparseable DSN yields a disabled client, not an error.
    guard.is_enabled().then_some(guard)
}

/// Runs on every event just before it leaves the process: removes
/// anything that could carry personal data and drops duplicates.
///
/// Strips the request body, cookies, query string and credential headers,
/// and keeps only the user's ID (never their IP, email or username).
/// Returns `None` to drop the event entirely, which is done for the
/// `tracing` lines that merely echo a panic the SDK already reports.
fn scrub(mut event: Event<'static>) -> Option<Event<'static>> {
    if event
        .message
        .as_deref()
        .is_some_and(|m| DUPLICATE_PANIC_MESSAGES.contains(&m))
    {
        return None;
    }

    if let Some(request) = event.request.as_mut() {
        request.data = None;
        request.cookies = None;
        request.query_string = None;
        request.env.clear();
        request
            .headers
            .retain(|name, _| !SENSITIVE_HEADERS.contains(&name.to_ascii_lowercase().as_str()));
    }
    // Keep only the user's ID (set by `record_request_identity`); drop the
    // IP address, email and username the SDK could otherwise attach.
    event.user = event.user.map(|user| sentry::User {
        id: user.id,
        ..Default::default()
    });
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentry::protocol::Request;

    fn event_with_request() -> Event<'static> {
        let mut request = Request {
            data: Some("{\"email\":\"a@b.c\"}".into()),
            cookies: Some("session=abc".into()),
            query_string: Some("email=a@b.c".into()),
            ..Default::default()
        };
        request
            .headers
            .insert("Authorization".into(), "Bearer x".into());
        request.headers.insert("User-Agent".into(), "curl".into());
        Event {
            request: Some(request),
            ..Default::default()
        }
    }

    #[test]
    fn scrub_removes_request_pii_but_keeps_harmless_headers() {
        let event = scrub(event_with_request()).expect("kept");
        let request = event.request.expect("request");
        assert!(request.data.is_none());
        assert!(request.cookies.is_none());
        assert!(request.query_string.is_none());
        assert!(!request.headers.contains_key("Authorization"));
        assert_eq!(
            request.headers.get("User-Agent").map(String::as_str),
            Some("curl")
        );
    }

    #[test]
    fn scrub_keeps_only_the_user_id() {
        let event = Event {
            user: Some(sentry::User {
                id: Some("u-1".into()),
                email: Some("a@b.c".into()),
                username: Some("boris".into()),
                ip_address: Some(sentry::protocol::IpAddress::Auto),
                ..Default::default()
            }),
            ..Default::default()
        };
        let user = scrub(event).expect("kept").user.expect("user");
        assert_eq!(user.id.as_deref(), Some("u-1"));
        assert!(user.email.is_none() && user.username.is_none() && user.ip_address.is_none());
    }

    #[test]
    fn scrub_drops_events_the_panic_integration_already_reports() {
        for message in DUPLICATE_PANIC_MESSAGES {
            let event = Event {
                message: Some((*message).to_string()),
                ..Default::default()
            };
            assert!(scrub(event).is_none(), "{message}");
        }
        let other = Event {
            message: Some("ClickUp call failed".to_string()),
            ..Default::default()
        };
        assert!(scrub(other).is_some());
    }

    #[test]
    fn init_is_disabled_without_a_dsn() {
        // Only meaningful where the variable is unset (CI); skip otherwise.
        if std::env::var("SENTRY_DSN").is_ok() {
            return;
        }
        assert!(init().is_none());
    }

    /// Manual check that the DSN, release tag and scrubber work end to end:
    /// `SENTRY_DSN=... cargo test sends_a_test_event -- --ignored`, then look
    /// for the issue in Sentry. Ignored so CI never talks to Sentry.
    #[test]
    #[ignore = "sends a real event to Sentry"]
    fn sends_a_test_event() {
        let guard = init().expect("set SENTRY_DSN to run this");
        sentry::capture_message(
            "unitprep-api Sentry setup check (safe to resolve)",
            sentry::Level::Warning,
        );
        assert!(guard.flush(None));
    }
}
