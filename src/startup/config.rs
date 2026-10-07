//! Environment-driven settings the server reads once at startup. Each is
//! a plain function over `std::env` so `main` never parses env vars
//! inline; integration credentials (Dropbox, Process Street) are NOT here
//! -- those are DB-first with their own loaders in `state`.

use std::time::Duration;

/// Idle lifetime of the three tool session stores (unit-group, dedup,
/// tagger). Overridable per deployment without a code change -- defaults
/// to 10 minutes if unset or unparseable.
pub(super) fn session_timeout() -> Duration {
    let secs = std::env::var("SESSION_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60 * 10);
    Duration::from_secs(secs)
}

/// How long a WebAuthn ceremony may take between `/begin` and `/finish`.
/// Fixed, not env-overridable like [`session_timeout`] -- a ceremony is
/// one request/response round trip through the browser's own
/// `navigator.credentials` call, not a tunable operational parameter the
/// way a login session's lifetime is.
pub(super) const CEREMONY_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Relying-party identity for WebAuthn. `rp_id` must be a valid domain
/// suffix of `rp_origin` (e.g. "example.com" with
/// "https://app.example.com"); defaults match local frontend dev, same as
/// CORS_ALLOWED_ORIGINS.
pub(super) struct WebauthnSettings {
    pub rp_id: String,
    pub rp_origin: String,
}

pub(super) fn webauthn_settings() -> WebauthnSettings {
    WebauthnSettings {
        rp_id: std::env::var("WEBAUTHN_RP_ID").unwrap_or_else(|_| "localhost".to_string()),
        rp_origin: std::env::var("WEBAUTHN_RP_ORIGIN")
            .unwrap_or_else(|_| "http://localhost:3000".to_string()),
    }
}

/// Host and port to bind. Defaults to 0.0.0.0 (all interfaces), not
/// 127.0.0.1 -- a container runtime's proxy (Fly.io, Docker, etc.)
/// connects over the container's network interface, not loopback, so
/// binding to 127.0.0.1 would make the app unreachable from outside the
/// container despite running fine locally. HOST/PORT are the de-facto
/// standard env vars most hosting platforms inject; both are overridable
/// for local conflicts.
pub(super) fn listen_host_and_port() -> (String, String) {
    (
        std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
        std::env::var("PORT").unwrap_or_else(|_| "8080".to_string()),
    )
}
