use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Request},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Json, Router,
};

use tower_governor::{governor::GovernorConfigBuilder, GovernorError, GovernorLayer};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use super::health::{health, health_db, whoami};
use super::{
    acknowledge_group_warnings, analyze, auth_audit_logs, auth_audit_logs_export,
    auth_configuration, auth_invites, auth_login, auth_logout, auth_passkey_reverify,
    auth_register, auth_roles, auth_totp, auth_user_role, auth_user_status, auth_users,
    cancel_session, client_ops_activity_logs, client_ops_activity_logs_export,
    client_ops_qms_tags, clients_companies, clients_create, clients_detail, clients_elavon,
    clients_preview, clients_resync, clients_search, clients_sync, correct,
    correct_group, dedup, discover, dropbox_browse,
    exclude_group, exclude_groups, exempt, export, group_file_confirm, group_file_upload,
    process_street_settings, resolve_unit_format, select_group_file, select_unit_file, tagger,
    unit_file_upload, upload, validate,
};
use super::{internal_error, ApiErrorBody, AppState};

/// Ceiling for `/tagger/check`'s upload specifically, well under the
/// router-wide `DefaultBodyLimit` below -- a `.docx` template is XML plus
/// occasional embedded media, not a bulk data export, so 10MB comfortably
/// covers a real template while bounding a pathological upload much
/// tighter than the general 100MB ceiling meant for other endpoints.
const TAGGER_CHECK_BODY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// Origins allowed to call this API. Defaults to the frontend dev servers
/// so local development needs no configuration; set
/// `CORS_ALLOWED_ORIGINS` (comma-separated) to add real deployed
/// frontend origins instead of hardcoding them here.
fn allowed_origins() -> Vec<axum::http::HeaderValue> {
    match std::env::var("CORS_ALLOWED_ORIGINS") {
        Ok(value) if !value.trim().is_empty() => value
            .split(',')
            .map(|origin| origin.trim())
            .filter(|origin| !origin.is_empty())
            .filter_map(|origin| origin.parse().ok())
            .collect(),

        _ => vec![
            "http://localhost:3000".parse().unwrap(),
            "http://localhost:5173".parse().unwrap(),
        ],
    }
}

/// One id per request, threaded through every log line emitted while
/// handling it (via the `TraceLayer` span below) and echoed back on the
/// response so a user reporting an issue can quote the exact request --
/// answering "what happened for this click" without cross-referencing
/// timestamps across possibly-concurrent requests. `x-request-id` is the
/// de facto standard header name for this.
static REQUEST_ID_HEADER: header::HeaderName = header::HeaderName::from_static("x-request-id");

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins()))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
        ])
        .allow_headers([axum::http::header::CONTENT_TYPE])
        // The frontend's shared hooks (useSessionPost/useSessionAction)
        // now send `credentials: "include"` on every request, ahead of
        // auth actually issuing a session cookie -- per the Fetch/CORS
        // spec, a credentialed request's response is invisible to the
        // browser unless the server explicitly echoes this header, even
        // before any real cookie exists to send. `allow_origin` above is
        // already a specific list (never `*`), which credentialed CORS
        // requires regardless.
        .allow_credentials(true)
        // Content-Disposition is not a CORS-safelisted response header,
        // so without this, every file-download endpoint's
        // `response.headers.get("Content-Disposition")` on the frontend
        // (dedup/audit-log/user export, tagger apply -- every one of
        // downloadBlob's callers) silently reads null and falls back to
        // its hardcoded default filename, even though the real header
        // is present on the wire. Same class of gap as the PUT/PATCH
        // CORS fix above: a browser-only restriction with no server-side
        // symptom, so it's invisible unless a download's real filename
        // is deliberately checked against something other than its own
        // fallback.
        .expose_headers([axum::http::header::CONTENT_DISPOSITION]);

    // Rate limit for the endpoints an anonymous caller can reach without
    // ever having a valid session: passkey registration (both the
    // invite-redemption and add-a-second-key paths), passkey login, and
    // the TOTP fallback login. One shared bucket across all of them,
    // keyed by peer IP -- deliberately not one bucket per route, so a
    // script cannot get five times the budget just by spreading its
    // attempts across five endpoints instead of one.
    //
    // Ten requests answered immediately, one more every three seconds
    // after that (~20/min sustained). Generous enough that a real person
    // retrying a cancelled Windows Hello prompt or fumbling a TOTP code a
    // few times in a row never notices this exists, while bounding how
    // fast an anonymous caller can iterate through addresses or guess
    // codes against these endpoints.
    //
    // Keying is by the TCP peer address (`tower_governor`'s default
    // `PeerIpKeyExtractor`), never a client-supplied header -- this
    // deliberately does not attempt to trust `X-Forwarded-For`, since no
    // trusted-reverse-proxy policy exists yet (see the `ip_address` NULL
    // comments in auth_register.rs / auth_login.rs for the same open
    // question). Once real client IPs need trusting for any reason, this
    // and that NULL should be revisited together, not separately -- they
    // are the same unresolved question in two places. Until then, behind
    // a reverse proxy that does not preserve the original TCP peer, this
    // still limits correctly, just coarsely: every client behind that
    // proxy shares one bucket rather than getting one each, which is
    // strictly more restrictive than intended, never less.
    let auth_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(3)
            .burst_size(10)
            .finish()
            .expect("auth rate-limit config: burst size and period are both non-zero constants"),
    );

    // A separate, more generous bucket for invite creation: authenticated
    // and admin-only already, so this is bounding accidental or scripted
    // hammering by a trusted caller, not probing by an anonymous one.
    let invite_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(20)
            .finish()
            .expect("invite rate-limit config: burst size and period are both non-zero constants"),
    );

    // The keyed limiter accumulates one entry per distinct peer IP it has
    // ever seen and nothing prunes that on its own -- `retain_recent()` is
    // `governor`'s own answer, and it has to be called from somewhere.
    // Mirrors `InMemorySessionStore::start_cleanup_task`: a background
    // tick that must keep running even if one iteration panics, since the
    // alternative is the rate limiter quietly becoming a slow memory leak
    // for the life of the process.
    {
        let auth_limiter = auth_rate_limit.limiter().clone();
        let invite_limiter = invite_rate_limit.limiter().clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));

            loop {
                interval.tick().await;

                // catch_unwind, not just calling these directly: the doc
                // comment above has always claimed this loop survives a
                // panicking tick, but nothing enforced that -- a real
                // panic inside retain_recent() would kill this spawned
                // task silently and permanently, quietly resuming the
                // exact memory leak this task exists to prevent, with no
                // log line anywhere saying so.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    auth_limiter.retain_recent();
                    invite_limiter.retain_recent();
                }));

                if let Err(panic) = result {
                    let message = panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("unknown panic payload");

                    tracing::error!(
                        panic = %message,
                        "rate-limit cleanup tick panicked; will retry on the next tick"
                    );
                }
            }
        });
    }

    // Split out as their own routers purely so the rate-limit layer
    // applies to exactly these paths and nothing else -- merged back into
    // the main router below while it is still `Router<AppState>`, since
    // `.merge` requires matching state types and `.with_state` further
    // down converts the main chain to `Router<()>`.
    let auth_routes = Router::new()
        .route("/auth/register/begin", post(auth_register::register_begin))
        .route(
            "/auth/register/finish",
            post(auth_register::register_finish),
        )
        .route("/auth/login/begin", post(auth_login::login_begin))
        .route("/auth/login/finish", post(auth_login::login_finish))
        .layer(
            GovernorLayer::new(auth_rate_limit)
                .error_handler(rate_limit_exceeded_with_audit("auth", state.db.clone())),
        );

    let invite_routes = Router::new()
        // Admin-only. Authorization is the `AuthenticatedUser` extractor in
        // the handler plus the admin-only RLS policies underneath it, not a
        // route-level guard -- there is no middleware layer that could be
        // reordered away from this path.
        .route("/auth/invites", post(auth_invites::create_invite))
        // Account recovery shares this bucket rather than the anonymous
        // auth_routes one above -- same trust level as invite creation
        // (authenticated admin), same "bound accidental/scripted
        // hammering by a trusted caller" rationale.
        .route("/auth/invites/recover", post(auth_invites::recover_account))
        .layer(
            GovernorLayer::new(invite_rate_limit)
                .error_handler(rate_limit_exceeded_with_audit("invite", state.db.clone())),
        );

    // Tighter than the router-wide DefaultBodyLimit near the bottom of
    // this function -- see TAGGER_CHECK_BODY_LIMIT_BYTES's own doc
    // comment. Split into its own router purely so the layer applies to
    // this one route, same "split for layer scoping" pattern as
    // auth_routes/invite_routes above.
    let tagger_check_route = Router::new()
        .route("/tagger/check", post(tagger::check))
        .layer(DefaultBodyLimit::max(TAGGER_CHECK_BODY_LIMIT_BYTES));

    Router::new()
        .route("/health", get(health))
        .route("/health/db", get(health_db))
        .route("/health/whoami", get(whoami))
        // Deliberately NOT behind the AuthenticatedUser extractor: signing
        // out must succeed with a stale or missing cookie, or the one case
        // where a user most needs to clear it is the case that 401s. See
        // auth_logout's module docs.
        // TOTP is authenticated-only end to end (the extractor is in every
        // handler below) -- there is no unauthenticated TOTP path any
        // more. See auth_totp.rs's module docs for why: it's a step-up
        // check for an already-signed-in session, not a way to log in.
        .route("/auth/totp/enroll/begin", post(auth_totp::enroll_begin))
        .route("/auth/totp/enroll/confirm", post(auth_totp::enroll_confirm))
        .route("/auth/totp/step-up", post(auth_totp::step_up))
        // Passkey-based step-up gating self-service TOTP re-enrolment --
        // the mirror of TOTP step-up gating add_passkey. See
        // auth_passkey_reverify.rs's module docs.
        .route(
            "/auth/reverify/begin",
            post(auth_passkey_reverify::reverify_begin),
        )
        .route(
            "/auth/reverify/finish",
            post(auth_passkey_reverify::reverify_finish),
        )
        // Admin-only, read-only -- no dedicated rate limit bucket the way
        // /auth/invites has, since a GET hit by an ordinary page load
        // isn't the "trusted caller hammering a write" case that
        // reasoning exists for.
        .route("/auth/users", get(auth_users::list_users))
        .route("/auth/users/export", get(auth_users::export_users))
        .route(
            "/auth/users/{id}/deactivate",
            post(auth_user_status::deactivate_user),
        )
        .route(
            "/auth/users/{id}/reactivate",
            post(auth_user_status::reactivate_user),
        )
        .route("/auth/users/{id}/roles", post(auth_user_role::grant_role))
        .route(
            "/auth/users/{id}/roles/{role_key}",
            delete(auth_user_role::revoke_role),
        )
        // No dedicated rate-limit bucket -- read-only catalog data any
        // authenticated caller can already reach under RLS.
        .route("/auth/roles", get(auth_roles::list_roles))
        .route(
            "/auth/configuration",
            get(auth_configuration::get_configuration)
                .put(auth_configuration::update_configuration),
        )
        // Read: any authenticated caller, same reasoning as /auth/roles
        // above. Writes: gated on client_ops.manage_tags inside each
        // handler (admin, onboarding_manager, department_manager all
        // hold it) — see client_ops_qms_tags's module doc.
        .route(
            "/client-ops/qms-tags",
            get(client_ops_qms_tags::list_qms_tags).post(client_ops_qms_tags::create_qms_tag),
        )
        .route(
            "/client-ops/qms-tags/{tag_key}",
            put(client_ops_qms_tags::update_qms_tag),
        )
        .route(
            "/client-ops/qms-tags/{tag_key}/deactivate",
            patch(client_ops_qms_tags::deactivate_qms_tag),
        )
        .route(
            "/client-ops/qms-tags/{tag_key}/reactivate",
            patch(client_ops_qms_tags::reactivate_qms_tag),
        )
        // Activity Logs -- gated on activity_logs.read inside each handler
        // (admin, onboarding_manager, department_manager all hold it),
        // same shape as /auth/audit-logs above but backed by
        // client_ops.audit_log instead of the security audit trail.
        .route(
            "/client-ops/activity-logs",
            get(client_ops_activity_logs::list_activity_logs),
        )
        .route(
            "/client-ops/activity-logs/event-types",
            get(client_ops_activity_logs::list_event_types),
        )
        .route(
            "/client-ops/activity-logs/export",
            post(client_ops_activity_logs_export::export_activity_logs),
        )
        .route(
            "/client-ops/activity-logs/export/preview",
            post(client_ops_activity_logs_export::preview_activity_logs),
        )
        // Any authenticated caller -- read-only discovery data (facility/
        // person names), same reasoning as the qms-tags read above. See
        // clients_search's own module doc for the two searches this runs.
        .route("/clients/search", get(clients_search::search_clients))
        // Read-only, no live PS write -- see clients_preview's own module doc.
        .route("/clients/preview", post(clients_preview::preview_clients))
        // GET: any authenticated caller (every client-scoped tool needs
        // this list to navigate). POST: requires client_ops.perform --
        // see clients_companies's and clients_create's own module docs.
        .route(
            "/clients",
            get(clients_companies::list_companies).post(clients_create::create_client),
        )
        // Requires client_ops.perform -- see clients_companies's own module doc.
        .route("/clients/{company_id}/archive", post(clients_companies::archive_company))
        .route("/clients/{company_id}/unarchive", post(clients_companies::unarchive_company))
        // Requires client_ops.perform -- see clients_resync's own module doc.
        .route(
            "/clients/{company_id}/resync/preview",
            post(clients_resync::preview_resync),
        )
        .route(
            "/clients/{company_id}/resync/apply",
            post(clients_resync::apply_resync),
        )
        // Any authenticated caller -- see clients_detail's own module doc.
        .route("/clients/{company_id}", get(clients_detail::get_company_detail))
        .route(
            "/clients/{company_id}/facilities/{facility_id}",
            get(clients_detail::get_facility_detail),
        )
        .route(
            "/clients/{company_id}/facilities/{facility_id}/policies",
            get(clients_detail::get_facility_policies),
        )
        // Read: any authenticated caller. Link/unlink: client_ops.perform
        // -- see clients_elavon's own module doc.
        .route(
            "/clients/{company_id}/facilities/{facility_id}/elavon",
            get(clients_elavon::get_facility_elavon),
        )
        .route(
            "/clients/{company_id}/facilities/{facility_id}/elavon/link",
            post(clients_elavon::link_facility_elavon).delete(clients_elavon::unlink_facility_elavon),
        )
        // Requires client_ops.perform to start; status read is any
        // authenticated caller -- see clients_sync's own module doc.
        .route("/clients/sync", post(clients_sync::start_sync))
        .route("/clients/sync/status", get(clients_sync::sync_status))
        // Read: any authenticated caller. Write: client_ops.perform --
        // see the migration's own comment on why this follows that gate
        // rather than auth.auth_configuration's admin-only one.
        .route(
            "/integrations/process-street/settings",
            get(process_street_settings::get_settings).put(process_street_settings::update_settings),
        )
        // Any authenticated caller -- folder names only, nothing
        // sensitive, same reasoning as the qms-tags read above. See
        // dropbox_browse's module doc for the root-path enforcement this
        // relies on.
        .route("/dropbox/list", get(dropbox_browse::list_folder))
        // Same reasoning as /dropbox/list above -- see
        // dropbox_browse::search_folders's own doc comment for why no
        // root-boundary check is needed on this one.
        .route("/dropbox/search", get(dropbox_browse::search_folders))
        // Any authenticated caller -- read-only discovery, same reasoning
        // as the two routes above. See dropbox_browse::facility_dropbox_folder's
        // own doc comment for why this takes a facility name (query
        // param), not a facility id path segment.
        .route(
            "/clients/{company_id}/dropbox-folder",
            get(dropbox_browse::facility_dropbox_folder),
        )
        // Admin-only, read-only -- same no-dedicated-bucket reasoning as
        // /auth/users above.
        .route("/auth/audit-logs", get(auth_audit_logs::list_audit_logs))
        .route(
            "/auth/audit-logs/event-types",
            get(auth_audit_logs::list_event_types),
        )
        .route(
            "/auth/audit-logs/export",
            post(auth_audit_logs_export::export_audit_logs),
        )
        .route(
            "/auth/audit-logs/export/preview",
            post(auth_audit_logs_export::preview_audit_logs),
        )
        .route("/auth/logout", post(auth_logout::logout))
        .route(
            "/auth/logout/everywhere",
            post(auth_logout::logout_everywhere),
        )
        .merge(auth_routes)
        .merge(invite_routes)
        .route("/upload", post(upload::upload))
        .route("/discover", post(discover::discover))
        .route("/validate", post(validate::validate))
        .route("/correct", post(correct::correct))
        .route("/correct-group", post(correct_group::correct_group))
        .route("/exempt-dimensions", post(exempt::exempt_dimensions))
        .route("/exclude-group", post(exclude_group::exclude_group))
        .route("/exclude-groups", post(exclude_groups::exclude_groups))
        .route(
            "/acknowledge-group-warnings",
            post(acknowledge_group_warnings::acknowledge_group_warnings),
        )
        .route("/analyze", post(analyze::analyze))
        .route("/export", post(export::export))
        .route(
            "/unit-file/select",
            post(select_unit_file::select_unit_file),
        )
        .route(
            "/unit-file/resolve-format",
            post(resolve_unit_format::resolve_unit_format),
        )
        .route(
            "/unit-file/upload",
            post(unit_file_upload::upload_unit_file),
        )
        .route(
            "/group-file/upload",
            post(group_file_upload::upload_group_file),
        )
        .route(
            "/group-file/confirm",
            post(group_file_confirm::confirm_group_file),
        )
        .route(
            "/group-file/select",
            post(select_group_file::select_group_file),
        )
        .route("/session/cancel", post(cancel_session::cancel_session))
        .route("/dedup/check", post(dedup::check))
        .route("/dedup/detect-vendor", post(dedup::detect_vendor_format))
        .route(
            "/dedup/detect-vendor-dropbox",
            post(dedup::detect_vendor_format_dropbox),
        )
        .route("/dedup/import-dropbox", post(dedup::import_from_dropbox))
        .route("/dedup/report", post(dedup::report))
        .route("/dedup/save-location", post(dedup::save_location))
        .route("/dedup/export", post(dedup::export))
        .route("/dedup/export-dropbox", post(dedup::export_to_dropbox))
        .route("/tagger/import-dropbox", post(tagger::import_from_dropbox))
        .route("/tagger/report", post(tagger::report))
        .route("/tagger/save-location", post(tagger::save_location))
        .route("/tagger/apply", post(tagger::apply))
        .route("/tagger/apply-dropbox", post(tagger::apply_to_dropbox))
        .merge(tagger_check_route)
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .with_state(state)
        .layer(cors)
        // A request that never reaches a handler at all -- malformed
        // JSON, the wrong Content-Type, or a body over DefaultBodyLimit
        // above -- is rejected by axum's own `Json<T>` extractor with a
        // plain-text body, not this project's `ApiErrorBody` shape every
        // handler-level error already uses. Every other error path in
        // this API (`session_not_found`, `stage_conflict`,
        // `internal_error`, and each handler's own structured responses)
        // is `{error, message}` JSON; a client parsing that consistently
        // would mishandle these three plain-text cases. This layer
        // rewrites them to match after the fact rather than changing
        // every handler's extractor type, which would be a much larger,
        // purely mechanical change for the same outcome.
        .layer(middleware::from_fn(normalize_extraction_rejection_body))
        // Catches a panic anywhere in the stack below (routes, cors,
        // body-limit) and turns it into the project's own ApiErrorBody
        // 500 shape instead of silently dropping the connection with no
        // response at all. No longer literally the outermost layer (the
        // three request-id/trace layers below wrap it), but still the
        // outermost of the response-shaping ones.
        .layer(CatchPanicLayer::custom(handle_panic))
        // Copies the id `SetRequestIdLayer` below assigned back onto the
        // response header, once a response exists -- applied here (more
        // inner than TraceLayer) so it runs before TraceLayer's own
        // on_response sees the response, per tower-http's documented
        // request-id composition.
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER.clone()))
        // The span this creates wraps every handler/layer below it, so
        // every `tracing::` call made while handling a request inherits
        // `request_id`/`method`/`path` as span context automatically --
        // no need to thread the id through each handler by hand.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request| {
                    let request_id = request
                        .extensions()
                        .get::<RequestId>()
                        .and_then(|id| id.header_value().to_str().ok())
                        .unwrap_or("unknown")
                        .to_string();

                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        path = %request.uri().path(),
                        request_id = %request_id,
                    )
                })
                .on_response(|response: &Response, latency: Duration, _span: &tracing::Span| {
                    // Read back off the response rather than threading
                    // the id through separately -- PropagateRequestIdLayer
                    // (more inner, so it runs first on the way out) has
                    // already copied it onto this exact response by the
                    // time this fires.
                    let request_id = response
                        .headers()
                        .get(&REQUEST_ID_HEADER)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("unknown");

                    tracing::info!(
                        request_id = %request_id,
                        status = response.status().as_u16(),
                        latency_ms = latency.as_millis(),
                        "request completed"
                    );
                }),
        )
        // Outermost layer overall -- assigns the id before anything else
        // (cors, body-limit, catch-panic, every route) sees the request,
        // so every request gets one regardless of how it's ultimately
        // handled or rejected.
        .layer(SetRequestIdLayer::new(
            REQUEST_ID_HEADER.clone(),
            MakeRequestUuid,
        ))
}

/// `tower_governor`'s own default rejection is plain text (e.g. `"Too Many
/// Requests! Wait for 3s"`), which is exactly the inconsistency
/// `normalize_extraction_rejection_body` above already exists to close for
/// a different auto-generated rejection class. Rather than reintroduce a
/// third response shape, this maps a governor rejection onto the same
/// `ApiErrorBody` every handler-level error already uses.
fn rate_limit_exceeded(error: GovernorError) -> Response {
    match error {
        GovernorError::TooManyRequests { wait_time, headers } => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(ApiErrorBody {
                    error: "rate_limited",
                    message: format!("Too many requests. Try again in {wait_time} second(s)."),
                }),
            )
                .into_response();

            if let Some(headers) = headers {
                response.headers_mut().extend(headers);
            }

            response
        }

        // Both are effectively "the rate limiter itself is misconfigured
        // or malfunctioning" rather than anything about the caller's
        // request, so they get the project's own internal_error path
        // instead of inventing a fourth shape for a case that should not
        // occur -- `UnableToExtractKey` cannot happen with the peer-IP
        // extractor used here (it never fails to extract), and `Other` is
        // never constructed by anything in this codebase.
        GovernorError::UnableToExtractKey | GovernorError::Other { .. } => {
            tracing::error!(?error, "rate limiter returned an unexpected error");
            internal_error("Could not process this request")
        }
    }
}

/// Wraps `rate_limit_exceeded` with an audit row for the one case that is
/// actually about the caller -- `TooManyRequests`. `tower_governor`'s
/// `error_handler` only receives the `GovernorError`, not the original
/// request, so there is no `ConnectInfo` to bind here; `bucket` (`"auth"`
/// or `"invite"`) is what distinguishes which limiter tripped.
///
/// The handler itself stays synchronous (that is what `error_handler`
/// requires), so the write is fire-and-forget on a spawned task rather
/// than awaited in place -- the same "must not affect the response"
/// property `audit_log::record` already has, just reached a different way
/// here since this function cannot itself be `async`.
fn rate_limit_exceeded_with_audit(
    bucket: &'static str,
    db: sqlx::PgPool,
) -> impl Fn(GovernorError) -> Response + Clone + Send + Sync + 'static {
    move |error: GovernorError| {
        if matches!(error, GovernorError::TooManyRequests { .. }) {
            let db = db.clone();
            tokio::spawn(async move {
                crate::auth::audit_log::record(
                    &db,
                    crate::auth::audit_log::event::RATE_LIMIT_REJECTED,
                    crate::auth::audit_log::Subjects::anonymous(),
                    None,
                    None,
                    crate::auth::audit_log::Change::none(),
                    serde_json::json!({ "bucket": bucket }),
                )
                .await;
            });
        }

        rate_limit_exceeded(error)
    }
}

/// See the doc comment on its `.layer(...)` call site in `router` above.
/// Only rewrites a response that (a) has one of the three status codes
/// axum's built-in extractors/body-limit actually produce for this
/// failure class, and (b) isn't already JSON -- a handler's own
/// legitimately-JSON 400 (e.g. `stage_conflict`, `correct_group`'s
/// `unknown_group`) must pass through completely untouched.
async fn normalize_extraction_rejection_body(request: Request, next: Next) -> Response {
    let response = next.run(request).await;

    let status = response.status();

    if !matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::PAYLOAD_TOO_LARGE
    ) {
        return response;
    }

    let already_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));

    if already_json {
        return response;
    }

    let (parts, body) = response.into_parts();

    let message = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => return Response::from_parts(parts, Body::empty()),
    };

    let error = match parts.status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
        StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
        _ => "invalid_request_body",
    };

    (parts.status, Json(ApiErrorBody { error, message })).into_response()
}

/// Turns a caught handler panic into a logged event plus the project's
/// standard `internal_error` response — the real panic detail goes to
/// the server log via `tracing::error!`, never into the response body a
/// client sees.
fn handle_panic(err: Box<dyn std::any::Any + Send + 'static>) -> Response {
    let message = if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    };

    tracing::error!(
        panic_message = %message,
        "request handler panicked"
    );

    internal_error("The server encountered an unexpected error")
}

#[cfg(test)]
mod panic_handler_tests {
    use super::*;

    #[test]
    fn handle_panic_returns_a_500_for_a_str_payload() {
        let response = handle_panic(Box::new("boom"));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn handle_panic_returns_a_500_for_a_string_payload() {
        let response = handle_panic(Box::new(String::from("boom")));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// A panic payload isn't always a &str/String (`std::panic::panic_any`
    /// can carry anything) — the fallback branch must still produce a
    /// clean 500, not panic itself while handling a panic.
    #[test]
    fn handle_panic_returns_a_500_for_an_unrecognized_payload() {
        let response = handle_panic(Box::new(42_i32));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
