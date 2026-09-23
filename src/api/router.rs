use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Request},
    http::{header, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Json, Router,
};

use tower_governor::{governor::GovernorConfigBuilder, GovernorError, GovernorLayer};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{
    MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::trace::TraceLayer;

use super::health::{health, health_db, whoami};
use super::route_access::{GatedRouter, RouteAccess};
use super::{
    acknowledge_group_warnings, analyze, auth_audit_logs, auth_audit_logs_export,
    auth_configuration, auth_invites, auth_login, auth_logout, auth_passkey_reverify,
    auth_register, auth_roles, auth_totp, auth_user_role, auth_user_status, auth_users,
    cancel_session, client_ops_activity_logs, client_ops_activity_logs_export, client_ops_qms_tags,
    clients_companies, clients_create, clients_detail, clients_dropbox_folder, clients_elavon,
    clients_facility_people, clients_facility_policies_edit, clients_filter_options,
    clients_manual_link, clients_onboarding_summary, clients_preview, clients_resync,
    clients_search, clients_sync, correct, correct_group, dedup, discover, dropbox_browse,
    dropbox_settings, exclude_group, exclude_groups, exempt, export, group_file_confirm,
    group_file_upload, process_street_settings, resolve_unit_format, select_group_file,
    select_unit_file, tagger, tool_runs, unit_file_upload, upload, validate,
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
    with_response_layers(build(state).into_parts().0)
}

/// Builds the whole route tree via `GatedRouter` -- see `route_access`'s
/// module doc for why every route below carries an explicit `RouteAccess`
/// at its call site. Kept separate from `router()`/`with_response_layers`
/// so tests can call this directly (with a fixture `AppState`) purely to
/// read back the manifest, without needing to also build the response-
/// shaping layers below, none of which affect authorization.
fn build(state: AppState) -> GatedRouter<()> {
    // Ten requests answered immediately, one more every three seconds
    // after that (~20/min sustained) -- generous enough that a real
    // person retrying a cancelled Windows Hello prompt or fumbling a
    // TOTP code a few times in a row never notices this exists, while
    // bounding how fast an anonymous caller can iterate through
    // addresses or guess codes against these endpoints. Keying is by the
    // TCP peer address (`tower_governor`'s default `PeerIpKeyExtractor`),
    // never a client-supplied header -- this deliberately does not
    // attempt to trust `X-Forwarded-For`, since no trusted-reverse-proxy
    // policy exists yet (see the `ip_address` NULL comments in
    // auth_register.rs / auth_login.rs for the same open question). Once
    // real client IPs need trusting for any reason, this and that NULL
    // should be revisited together, not separately -- they are the same
    // unresolved question in two places. Until then, behind a reverse
    // proxy that does not preserve the original TCP peer, this still
    // limits correctly, just coarsely: every client behind that proxy
    // shares one bucket rather than getting one each, which is strictly
    // more restrictive than intended, never less.
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
    // ever seen and nothing prunes that on its own -- `retain_recent()`
    // is `governor`'s own answer, and it has to be called from somewhere.
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
    // the main router below while it is still `GatedRouter<AppState>`,
    // since `.merge` requires matching state types and `.with_state`
    // further down converts the main chain to `GatedRouter<()>`.
    let auth_routes = GatedRouter::new()
        .gated_route(
            "/auth/register/begin",
            post(auth_register::register_begin),
            [(Method::POST, RouteAccess::AuthCeremony)],
        )
        .gated_route(
            "/auth/register/finish",
            post(auth_register::register_finish),
            [(Method::POST, RouteAccess::AuthCeremony)],
        )
        .gated_route(
            "/auth/login/begin",
            post(auth_login::login_begin),
            [(Method::POST, RouteAccess::AuthCeremony)],
        )
        .gated_route(
            "/auth/login/finish",
            post(auth_login::login_finish),
            [(Method::POST, RouteAccess::AuthCeremony)],
        )
        .map_router(|r| {
            r.layer(
                GovernorLayer::new(auth_rate_limit)
                    .error_handler(rate_limit_exceeded_with_audit("auth", state.db.clone())),
            )
        });

    let invite_routes = GatedRouter::new()
        // Admin-only. Authorization is `require_permission` inside the
        // handler plus the admin-only RLS policies underneath it, not a
        // route-level guard -- there is no middleware layer that could be
        // reordered away from this path.
        .gated_route(
            "/auth/invites",
            post(auth_invites::create_invite),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["users.manage", "users.manage_roles"],
                    action: "create_invite",
                },
            )],
        )
        // Despite the name, this is an admin-only account-recovery action
        // (revokes and reissues every credential on the target account),
        // not a self-service "forgot my passkey" flow -- see
        // THREAT_MODEL.md's actor table: "isn't reachable unauth -- admin
        // only". Shares this bucket rather than the anonymous
        // `auth_routes` one above -- same trust level as invite creation.
        .gated_route(
            "/auth/invites/recover",
            post(auth_invites::recover_account),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["users.manage"],
                    action: "recover_account",
                },
            )],
        )
        .map_router(|r| {
            r.layer(
                GovernorLayer::new(invite_rate_limit)
                    .error_handler(rate_limit_exceeded_with_audit("invite", state.db.clone())),
            )
        });

    // Tighter than the router-wide DefaultBodyLimit near the bottom of
    // this function -- see TAGGER_CHECK_BODY_LIMIT_BYTES's own doc
    // comment. Split into its own router purely so the layer applies to
    // this one route, same "split for layer scoping" pattern as
    // auth_routes/invite_routes above.
    let tagger_check_route = GatedRouter::new()
        .gated_route(
            "/tagger/check",
            post(tagger::check),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .map_router(|r| r.layer(DefaultBodyLimit::max(TAGGER_CHECK_BODY_LIMIT_BYTES)));

    GatedRouter::new()
        .gated_route("/health", get(health), [(Method::GET, RouteAccess::Public)])
        .gated_route(
            "/health/db",
            get(health_db),
            [(Method::GET, RouteAccess::Public)],
        )
        // Deliberately NOT behind the AuthenticatedUser extractor: signing
        // out must succeed with a stale or missing cookie, or the one case
        // where a user most needs to clear it is the case that 401s. See
        // auth_logout's module docs.
        .gated_route(
            "/health/whoami",
            get(whoami),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        // TOTP is authenticated-only end to end (the extractor is in every
        // handler below) -- there is no unauthenticated TOTP path any
        // more. See auth_totp.rs's module docs for why: it's a step-up
        // check for an already-signed-in session, not a way to log in.
        .gated_route(
            "/auth/totp/enroll/begin",
            post(auth_totp::enroll_begin),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/auth/totp/enroll/confirm",
            post(auth_totp::enroll_confirm),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/auth/totp/step-up",
            post(auth_totp::step_up),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        // Passkey-based step-up gating self-service TOTP re-enrolment --
        // the mirror of TOTP step-up gating add_passkey. See
        // auth_passkey_reverify.rs's module docs.
        .gated_route(
            "/auth/reverify/begin",
            post(auth_passkey_reverify::reverify_begin),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/auth/reverify/finish",
            post(auth_passkey_reverify::reverify_finish),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        // Admin-only, read-only -- no dedicated rate limit bucket the way
        // /auth/invites has, since a GET hit by an ordinary page load
        // isn't the "trusted caller hammering a write" case that
        // reasoning exists for.
        .gated_route(
            "/auth/users",
            get(auth_users::list_users),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["users.manage"],
                    action: "list_users",
                },
            )],
        )
        .gated_route(
            "/auth/users/export",
            get(auth_users::export_users),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["users.manage"],
                    action: "export_users",
                },
            )],
        )
        .gated_route(
            "/auth/users/{id}/deactivate",
            post(auth_user_status::deactivate_user),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["users.manage"],
                    action: "deactivate_user",
                },
            )],
        )
        .gated_route(
            "/auth/users/{id}/reactivate",
            post(auth_user_status::reactivate_user),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["users.manage"],
                    action: "reactivate_user",
                },
            )],
        )
        .gated_route(
            "/auth/users/{id}/roles",
            post(auth_user_role::grant_role),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["users.manage_roles"],
                    action: "grant_role",
                },
            )],
        )
        .gated_route(
            "/auth/users/{id}/roles/{role_key}",
            delete(auth_user_role::revoke_role),
            [(
                Method::DELETE,
                RouteAccess::Permission {
                    keys: &["users.manage_roles"],
                    action: "revoke_role",
                },
            )],
        )
        // No dedicated rate-limit bucket -- read-only catalog data any
        // authenticated caller can already reach under RLS.
        .gated_route(
            "/auth/roles",
            get(auth_roles::list_roles),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/auth/configuration",
            get(auth_configuration::get_configuration)
                .put(auth_configuration::update_configuration),
            [
                (
                    Method::GET,
                    RouteAccess::Permission {
                        keys: &["security_policies.manage"],
                        action: "get_configuration",
                    },
                ),
                (
                    Method::PUT,
                    RouteAccess::Permission {
                        keys: &["security_policies.manage"],
                        action: "update_configuration",
                    },
                ),
            ],
        )
        // Read: any authenticated caller, same reasoning as /auth/roles
        // above. Writes: gated on client_ops.manage_tags inside each
        // handler (admin, onboarding_manager, department_manager all
        // hold it) — see client_ops_qms_tags's module doc.
        .gated_route(
            "/client-ops/qms-tags",
            get(client_ops_qms_tags::list_qms_tags).post(client_ops_qms_tags::create_qms_tag),
            [
                (Method::GET, RouteAccess::Authenticated),
                (
                    Method::POST,
                    RouteAccess::Permission {
                        keys: &["client_ops.manage_tags"],
                        action: "create_qms_tag",
                    },
                ),
            ],
        )
        .gated_route(
            "/client-ops/qms-tags/{tag_key}",
            put(client_ops_qms_tags::update_qms_tag),
            [(
                Method::PUT,
                RouteAccess::Permission {
                    keys: &["client_ops.manage_tags"],
                    action: "update_qms_tag",
                },
            )],
        )
        .gated_route(
            "/client-ops/qms-tags/{tag_key}/deactivate",
            patch(client_ops_qms_tags::deactivate_qms_tag),
            [(
                Method::PATCH,
                RouteAccess::Permission {
                    keys: &["client_ops.manage_tags"],
                    action: "deactivate_qms_tag",
                },
            )],
        )
        .gated_route(
            "/client-ops/qms-tags/{tag_key}/reactivate",
            patch(client_ops_qms_tags::reactivate_qms_tag),
            [(
                Method::PATCH,
                RouteAccess::Permission {
                    keys: &["client_ops.manage_tags"],
                    action: "reactivate_qms_tag",
                },
            )],
        )
        // Activity Logs -- gated on activity_logs.read inside each handler
        // (admin, onboarding_manager, department_manager all hold it),
        // same shape as /auth/audit-logs below but backed by
        // client_ops.audit_log instead of the security audit trail.
        .gated_route(
            "/client-ops/activity-logs",
            get(client_ops_activity_logs::list_activity_logs),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["activity_logs.read"],
                    action: "list_activity_logs",
                },
            )],
        )
        .gated_route(
            "/client-ops/activity-logs/event-types",
            get(client_ops_activity_logs::list_event_types),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["activity_logs.read"],
                    action: "list_activity_log_event_types",
                },
            )],
        )
        .gated_route(
            "/client-ops/activity-logs/export",
            post(client_ops_activity_logs_export::export_activity_logs),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["activity_logs.read"],
                    action: "export_activity_logs",
                },
            )],
        )
        .gated_route(
            "/client-ops/activity-logs/export/preview",
            post(client_ops_activity_logs_export::preview_activity_logs),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["activity_logs.read"],
                    action: "preview_activity_logs",
                },
            )],
        )
        // Any authenticated caller -- read-only discovery data (facility/
        // person names), same reasoning as the qms-tags read above. See
        // clients_search's own module doc for the two searches this runs.
        .gated_route(
            "/clients/search",
            get(clients_search::search_clients),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        // Read-only, no live PS write -- see clients_preview's own module doc.
        .gated_route(
            "/clients/preview",
            post(clients_preview::preview_clients),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        // GET: any authenticated caller (every client-scoped tool needs
        // this list to navigate). POST: requires client_ops.perform --
        // see clients_companies's and clients_create's own module docs.
        .gated_route(
            "/clients",
            get(clients_companies::list_companies).post(clients_create::create_client),
            [
                (Method::GET, RouteAccess::Authenticated),
                (
                    Method::POST,
                    RouteAccess::Permission {
                        keys: &["client_ops.perform"],
                        action: "create_client_from_process_street",
                    },
                ),
            ],
        )
        // Any authenticated caller -- read-only discovery data for the
        // clients-page filter checkboxes, same reasoning as
        // clients_search above. See clients_filter_options's own module doc.
        .gated_route(
            "/clients/filter-options",
            get(clients_filter_options::get_filter_options),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        // Requires client_ops.perform -- see clients_companies's own module doc.
        .gated_route(
            "/clients/{company_id}/archive",
            post(clients_companies::archive_company),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "archive_company",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/unarchive",
            post(clients_companies::unarchive_company),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "unarchive_company",
                },
            )],
        )
        // Requires client_ops.perform -- see clients_resync's own module doc.
        .gated_route(
            "/clients/{company_id}/resync/preview",
            post(clients_resync::preview_resync),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "preview_resync",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/resync/apply",
            post(clients_resync::apply_resync),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "apply_resync",
                },
            )],
        )
        // Company page's "Manual Link" button -- requires client_ops.perform,
        // see clients_manual_link's own module doc.
        .gated_route(
            "/clients/{company_id}/manual-link",
            post(clients_manual_link::manual_link),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "manual_link",
                },
            )],
        )
        // GET: any authenticated caller -- see clients_detail's own
        // module doc. DELETE: requires client_ops.perform -- see
        // clients_companies's own module doc (a genuine permanent
        // delete, distinct from archive/unarchive above).
        .gated_route(
            "/clients/{company_id}",
            get(clients_detail::get_company_detail).delete(clients_companies::delete_company),
            [
                (Method::GET, RouteAccess::Authenticated),
                (
                    Method::DELETE,
                    RouteAccess::Permission {
                        keys: &["client_ops.perform"],
                        action: "delete_company",
                    },
                ),
            ],
        )
        // Company page's Onboarding Summary tab -- read-only, any
        // authenticated caller, RLS is the real gate (see
        // clients_onboarding_summary's own module doc).
        .gated_route(
            "/clients/{company_id}/onboarding-summary",
            get(clients_onboarding_summary::get_onboarding_summary),
            [(Method::GET, RouteAccess::RlsRead)],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}",
            get(clients_detail::get_facility_detail),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/policies",
            get(clients_detail::get_facility_policies),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        // Manual edit for each split Facility Policies tab -- no extra
        // permission check, RLS already gates these tables to
        // onboarding_manager/department_manager (see
        // clients_facility_policies_edit's own module doc).
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/policies/fees",
            put(clients_facility_policies_edit::update_fees),
            [(Method::PUT, RouteAccess::RlsWrite)],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/policies/taxes",
            put(clients_facility_policies_edit::update_taxes),
            [(Method::PUT, RouteAccess::RlsWrite)],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/policies/delinquency",
            put(clients_facility_policies_edit::update_delinquency),
            [(Method::PUT, RouteAccess::RlsWrite)],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/policies/coverage",
            put(clients_facility_policies_edit::update_coverage),
            [(Method::PUT, RouteAccess::RlsWrite)],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/policies/specials",
            put(clients_facility_policies_edit::update_specials),
            [(Method::PUT, RouteAccess::RlsWrite)],
        )
        // Read: any authenticated caller. Link/unlink/resync:
        // client_ops.perform -- see clients_elavon's own module doc.
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/elavon",
            get(clients_elavon::get_facility_elavon),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/elavon/link",
            post(clients_elavon::link_facility_elavon)
                .delete(clients_elavon::unlink_facility_elavon),
            [
                (
                    Method::POST,
                    RouteAccess::Permission {
                        keys: &["client_ops.perform"],
                        action: "link_facility_merchant_account",
                    },
                ),
                (
                    Method::DELETE,
                    RouteAccess::Permission {
                        keys: &["client_ops.perform"],
                        action: "unlink_facility_merchant_account",
                    },
                ),
            ],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/elavon/resync",
            post(clients_elavon::resync_elavon_data),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "resync_elavon_data",
                },
            )],
        )
        // DropBox tab -- no extra permission check, RLS is the real
        // gate (see clients_dropbox_folder's own module doc).
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/dropbox-folder",
            put(clients_dropbox_folder::update_facility_dropbox_folder),
            [(Method::PUT, RouteAccess::RlsWrite)],
        )
        // Users tab -- read and write both just need authentication, RLS
        // is the real gate (see clients_facility_people's own module doc).
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/people",
            get(clients_facility_people::get_facility_people)
                .post(clients_facility_people::add_facility_person),
            [
                (Method::GET, RouteAccess::RlsRead),
                (Method::POST, RouteAccess::RlsWrite),
            ],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/people/{person_id}",
            put(clients_facility_people::edit_facility_person)
                .delete(clients_facility_people::unlink_facility_person),
            [
                (Method::PUT, RouteAccess::RlsWrite),
                (Method::DELETE, RouteAccess::RlsWrite),
            ],
        )
        // Onboarding Work tab -- read-only, any authenticated caller, RLS
        // is the real gate (see tool_runs's own module doc). DELETE
        // (clearing a mistaken run) requires client_ops.perform.
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/tool-runs",
            get(tool_runs::list_facility_tool_runs),
            [(Method::GET, RouteAccess::RlsRead)],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/tool-runs/{run_id}",
            delete(tool_runs::delete_tool_run),
            [(
                Method::DELETE,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "delete_tool_run",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/tool-runs/{run_id}/output",
            get(tool_runs::download_tool_run_output),
            [(Method::GET, RouteAccess::RlsRead)],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/tool-runs/{run_id}/source",
            get(tool_runs::download_tool_run_source),
            [(Method::GET, RouteAccess::RlsRead)],
        )
        // Requires client_ops.perform to start; status read is any
        // authenticated caller -- see clients_sync's own module doc.
        .gated_route(
            "/clients/sync",
            post(clients_sync::start_sync),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "start_process_street_sync",
                },
            )],
        )
        .gated_route(
            "/clients/sync/status",
            get(clients_sync::sync_status),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        // Both GET and PUT require integrations.manage (admin-only) --
        // corrected 2026-09-23 from a stale "Read: any authenticated
        // caller" comment here that no longer matched
        // `process_street_settings::get_settings`, which gates on this
        // permission too (it returns the live API key). Exactly the kind
        // of drift the new `permission_gate_tests` module below exists to
        // catch instead of relying on a comment staying accurate by hand.
        .gated_route(
            "/integrations/process-street/settings",
            get(process_street_settings::get_settings)
                .put(process_street_settings::update_settings),
            [
                (
                    Method::GET,
                    RouteAccess::Permission {
                        keys: &["integrations.manage"],
                        action: "get_process_street_settings",
                    },
                ),
                (
                    Method::PUT,
                    RouteAccess::Permission {
                        keys: &["integrations.manage"],
                        action: "update_process_street_settings",
                    },
                ),
            ],
        )
        // Admin-only (integrations.manage) read and write -- this one
        // holds the Dropbox app's own secrets, so unlike the Process
        // Street settings above, even the read side is gated. See
        // dropbox_settings's own module doc.
        .gated_route(
            "/integrations/dropbox/settings",
            get(dropbox_settings::get_settings).put(dropbox_settings::update_settings),
            [
                (
                    Method::GET,
                    RouteAccess::Permission {
                        keys: &["integrations.manage"],
                        action: "get_dropbox_settings",
                    },
                ),
                (
                    Method::PUT,
                    RouteAccess::Permission {
                        keys: &["integrations.manage"],
                        action: "update_dropbox_settings",
                    },
                ),
            ],
        )
        // Any authenticated caller -- folder names only, nothing
        // sensitive, same reasoning as the qms-tags read above. See
        // dropbox_browse's module doc for the root-path enforcement this
        // relies on.
        .gated_route(
            "/dropbox/list",
            get(dropbox_browse::list_folder),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        // Same reasoning as /dropbox/list above -- see
        // dropbox_browse::search_folders's own doc comment for why no
        // root-boundary check is needed on this one.
        .gated_route(
            "/dropbox/search",
            get(dropbox_browse::search_folders),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        // Any authenticated caller -- read-only discovery, same reasoning
        // as the two routes above. See dropbox_browse::facility_dropbox_folder's
        // own doc comment for why this takes a facility name (query
        // param), not a facility id path segment.
        .gated_route(
            "/clients/{company_id}/dropbox-folder",
            get(dropbox_browse::facility_dropbox_folder),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        // Admin-only, read-only -- same no-dedicated-bucket reasoning as
        // /auth/users above.
        .gated_route(
            "/auth/audit-logs",
            get(auth_audit_logs::list_audit_logs),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["audit_logs.read"],
                    action: "list_audit_logs",
                },
            )],
        )
        .gated_route(
            "/auth/audit-logs/event-types",
            get(auth_audit_logs::list_event_types),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["audit_logs.read"],
                    action: "list_event_types",
                },
            )],
        )
        .gated_route(
            "/auth/audit-logs/export",
            post(auth_audit_logs_export::export_audit_logs),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["audit_logs.read"],
                    action: "export_audit_logs",
                },
            )],
        )
        .gated_route(
            "/auth/audit-logs/export/preview",
            post(auth_audit_logs_export::preview_audit_logs),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["audit_logs.read"],
                    action: "preview_audit_logs",
                },
            )],
        )
        .gated_route(
            "/auth/logout",
            post(auth_logout::logout),
            [(Method::POST, RouteAccess::Public)],
        )
        .gated_route(
            "/auth/logout/everywhere",
            post(auth_logout::logout_everywhere),
            [(Method::POST, RouteAccess::Public)],
        )
        .merge(auth_routes)
        .merge(invite_routes)
        // Tool-session routes below: any authenticated caller may use the
        // tools, no specific permission required (see THREAT_MODEL.md's
        // "Known gaps" -- these are intentionally ungated, not an
        // oversight).
        .gated_route(
            "/upload",
            post(upload::upload),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/upload-dropbox",
            post(upload::import_from_dropbox),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/discover",
            post(discover::discover),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/validate",
            post(validate::validate),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/correct",
            post(correct::correct),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/correct-group",
            post(correct_group::correct_group),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/exempt-dimensions",
            post(exempt::exempt_dimensions),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/exclude-group",
            post(exclude_group::exclude_group),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/exclude-groups",
            post(exclude_groups::exclude_groups),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/acknowledge-group-warnings",
            post(acknowledge_group_warnings::acknowledge_group_warnings),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/analyze",
            post(analyze::analyze),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/export",
            post(export::export),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/export/save-location",
            post(export::save_location),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/export/export-dropbox",
            post(export::export_to_dropbox),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/unit-file/select",
            post(select_unit_file::select_unit_file),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/unit-file/resolve-format",
            post(resolve_unit_format::resolve_unit_format),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/unit-file/upload",
            post(unit_file_upload::upload_unit_file),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/group-file/upload",
            post(group_file_upload::upload_group_file),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/group-file/confirm",
            post(group_file_confirm::confirm_group_file),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/group-file/select",
            post(select_group_file::select_group_file),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/session/cancel",
            post(cancel_session::cancel_session),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/check",
            post(dedup::check),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/detect-vendor",
            post(dedup::detect_vendor_format),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/detect-vendor-dropbox",
            post(dedup::detect_vendor_format_dropbox),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/import-dropbox",
            post(dedup::import_from_dropbox),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/report",
            post(dedup::report),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/save-location",
            post(dedup::save_location),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/export",
            post(dedup::export),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/export-dropbox",
            post(dedup::export_to_dropbox),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/tagger/import-dropbox",
            post(tagger::import_from_dropbox),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/tagger/report",
            post(tagger::report),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/tagger/save-location",
            post(tagger::save_location),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/tagger/apply",
            post(tagger::apply),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/tagger/apply-dropbox",
            post(tagger::apply_to_dropbox),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .merge(tagger_check_route)
        .map_router(|r| r.layer(DefaultBodyLimit::max(100 * 1024 * 1024)))
        .with_state(state)
}

/// Response-shaping middleware, applied to the state-erased `Router<()>`
/// -- none of this affects authorization, so it lives outside `build`
/// and outside the `GatedRouter` manifest entirely.
fn with_response_layers(router: Router) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins()))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
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

    router
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
                .on_response(
                    |response: &Response, latency: Duration, _span: &tracing::Span| {
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
                    },
                ),
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

/// See the doc comment on its `.layer(...)` call site in `with_response_layers`
/// above. Only rewrites a response that (a) has one of the three status codes
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

/// Closes THREAT_MODEL.md's "no formal, automated check that every new
/// privilege-gated handler actually calls `require_permission`" gap. See
/// `route_access`'s module doc for the compile-time half (every route
/// must declare a `RouteAccess`); this is the runtime half -- for every
/// route `build()` classified as `RouteAccess::Permission`, prove the
/// exact handler wired up to it actually 403s a caller holding no
/// permissions, the same `empty_state()`/dummy-argument pattern this
/// codebase's existing per-handler tests already use (see e.g.
/// `clients_companies::tests::archiving_refuses_insufficient_permission_without_touching_anything`).
#[cfg(test)]
mod permission_gate_tests {
    use std::future::Future;
    use std::net::SocketAddr;
    use std::pin::Pin;

    use axum::extract::{ConnectInfo, Path, Query};
    use axum::http::{HeaderMap, Method, StatusCode};
    use axum::response::Response;
    use axum::Json;
    use uuid::Uuid;

    use crate::api::clients_companies;
    use crate::api::route_access::RouteAccess;
    use crate::api::test_support::{empty_state, test_user};
    use crate::api::{
        auth_audit_logs, auth_audit_logs_export, auth_configuration, auth_invites, auth_user_role,
        auth_user_status, auth_users, client_ops_activity_logs, client_ops_activity_logs_export,
        client_ops_qms_tags, clients_create, clients_elavon, clients_manual_link, clients_resync,
        clients_sync, dropbox_settings, process_street_settings, tool_runs,
    };

    use super::build;

    type BoxFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

    /// `(path, method, assert_denied)` -- one entry per `Permission`-
    /// classified route, as returned by `permission_route_checks` below.
    type PermissionRouteCheck = (&'static str, Method, fn() -> BoxFuture);

    fn local_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    /// One entry per `RouteAccess::Permission` route `build()` registers
    /// below -- calls the exact handler wired up to that (path, method),
    /// with `test_user()` (a valid session holding zero permissions).
    /// Adding a `Permission`-classified `gated_route` without a matching
    /// entry here fails `every_permission_route_is_covered_and_enforced`
    /// below by construction: the whole point of this list is to make
    /// that omission loud instead of silent.
    fn permission_route_checks() -> Vec<PermissionRouteCheck> {
        vec![
            (
                "/auth/invites",
                Method::POST,
                (|| {
                    Box::pin(auth_invites::create_invite(
                        axum::extract::State(empty_state()),
                        test_user(),
                        local_addr(),
                        HeaderMap::new(),
                        Json(auth_invites::CreateInviteRequest {
                            email: "ada@example.com".to_string(),
                            first_name: "Ada".to_string(),
                            last_name: "Lovelace".to_string(),
                            company: "quikstor".to_string(),
                            job_title: None,
                            role: "onboarding_manager".to_string(),
                        }),
                    ))
                }) as fn() -> BoxFuture,
            ),
            ("/auth/invites/recover", Method::POST, || {
                Box::pin(auth_invites::recover_account(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    HeaderMap::new(),
                    Json(auth_invites::RecoverAccountRequest {
                        email: "someone@example.com".to_string(),
                    }),
                ))
            }),
            ("/auth/users", Method::GET, || {
                Box::pin(auth_users::list_users(
                    axum::extract::State(empty_state()),
                    test_user(),
                ))
            }),
            ("/auth/users/export", Method::GET, || {
                Box::pin(auth_users::export_users(
                    axum::extract::State(empty_state()),
                    test_user(),
                ))
            }),
            ("/auth/users/{id}/deactivate", Method::POST, || {
                Box::pin(auth_user_status::deactivate_user(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    HeaderMap::new(),
                    Path(Uuid::new_v4()),
                ))
            }),
            ("/auth/users/{id}/reactivate", Method::POST, || {
                Box::pin(auth_user_status::reactivate_user(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    HeaderMap::new(),
                    Path(Uuid::new_v4()),
                ))
            }),
            ("/auth/users/{id}/roles", Method::POST, || {
                Box::pin(auth_user_role::grant_role(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    HeaderMap::new(),
                    Path(Uuid::new_v4()),
                    Json(auth_user_role::GrantRoleRequest {
                        role: "onboarding_manager".to_string(),
                    }),
                ))
            }),
            ("/auth/users/{id}/roles/{role_key}", Method::DELETE, || {
                Box::pin(auth_user_role::revoke_role(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    HeaderMap::new(),
                    Path((Uuid::new_v4(), "onboarding_manager".to_string())),
                ))
            }),
            ("/auth/configuration", Method::GET, || {
                Box::pin(auth_configuration::get_configuration(
                    axum::extract::State(empty_state()),
                    test_user(),
                ))
            }),
            ("/auth/configuration", Method::PUT, || {
                Box::pin(auth_configuration::update_configuration(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    HeaderMap::new(),
                    Json(auth_configuration::UpdateAuthConfigurationRequest {
                        step_up_actions: vec![],
                    }),
                ))
            }),
            ("/client-ops/qms-tags", Method::POST, || {
                Box::pin(client_ops_qms_tags::create_qms_tag(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Json(client_ops_qms_tags::CreateQmsTagRequest {
                        tag_key: "e.test".to_string(),
                        label: "Test".to_string(),
                        category: "Tenant".to_string(),
                    }),
                ))
            }),
            ("/client-ops/qms-tags/{tag_key}", Method::PUT, || {
                Box::pin(client_ops_qms_tags::update_qms_tag(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path("e.test".to_string()),
                    Json(client_ops_qms_tags::UpdateQmsTagRequest {
                        label: "Test".to_string(),
                        category: "Tenant".to_string(),
                    }),
                ))
            }),
            (
                "/client-ops/qms-tags/{tag_key}/deactivate",
                Method::PATCH,
                || {
                    Box::pin(client_ops_qms_tags::deactivate_qms_tag(
                        axum::extract::State(empty_state()),
                        test_user(),
                        HeaderMap::new(),
                        Path("e.test".to_string()),
                    ))
                },
            ),
            (
                "/client-ops/qms-tags/{tag_key}/reactivate",
                Method::PATCH,
                || {
                    Box::pin(client_ops_qms_tags::reactivate_qms_tag(
                        axum::extract::State(empty_state()),
                        test_user(),
                        HeaderMap::new(),
                        Path("e.test".to_string()),
                    ))
                },
            ),
            ("/client-ops/activity-logs", Method::GET, || {
                Box::pin(client_ops_activity_logs::list_activity_logs(
                    axum::extract::State(empty_state()),
                    test_user(),
                    Query(client_ops_activity_logs::ActivityLogQuery {
                        limit: None,
                        before_id: None,
                        event_type: None,
                        entity_type: None,
                        actor_user_id: None,
                    }),
                ))
            }),
            ("/client-ops/activity-logs/event-types", Method::GET, || {
                Box::pin(client_ops_activity_logs::list_event_types(
                    axum::extract::State(empty_state()),
                    test_user(),
                ))
            }),
            ("/client-ops/activity-logs/export", Method::POST, || {
                Box::pin(client_ops_activity_logs_export::export_activity_logs(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    HeaderMap::new(),
                    Json(client_ops_activity_logs_export::ExportActivityLogsRequest {
                        date_from: "2026-08-01T00:00:00Z".parse().unwrap(),
                        date_to: "2026-08-05T00:00:00Z".parse().unwrap(),
                        event_types: vec![],
                        entity_types: vec![],
                        actor_user_ids: vec![],
                    }),
                ))
            }),
            (
                "/client-ops/activity-logs/export/preview",
                Method::POST,
                || {
                    Box::pin(client_ops_activity_logs_export::preview_activity_logs(
                        axum::extract::State(empty_state()),
                        test_user(),
                        local_addr(),
                        Json(client_ops_activity_logs_export::ExportActivityLogsRequest {
                            date_from: "2026-08-01T00:00:00Z".parse().unwrap(),
                            date_to: "2026-08-05T00:00:00Z".parse().unwrap(),
                            event_types: vec![],
                            entity_types: vec![],
                            actor_user_ids: vec![],
                        }),
                    ))
                },
            ),
            ("/clients", Method::POST, || {
                Box::pin(clients_create::create_client(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Json(clients_create::CreateClientRequest {
                        company_intake_run_id: "abc123".to_string(),
                        company: Default::default(),
                        facilities: vec![],
                    }),
                ))
            }),
            ("/clients/{company_id}/archive", Method::POST, || {
                Box::pin(clients_companies::archive_company(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path(Uuid::new_v4()),
                ))
            }),
            ("/clients/{company_id}/unarchive", Method::POST, || {
                Box::pin(clients_companies::unarchive_company(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path(Uuid::new_v4()),
                ))
            }),
            ("/clients/{company_id}/resync/preview", Method::POST, || {
                Box::pin(clients_resync::preview_resync(
                    axum::extract::State(empty_state()),
                    test_user(),
                    Path(Uuid::new_v4()),
                ))
            }),
            ("/clients/{company_id}/resync/apply", Method::POST, || {
                Box::pin(clients_resync::apply_resync(
                    axum::extract::State(empty_state()),
                    test_user(),
                    Path(Uuid::new_v4()),
                    Json(clients_resync::ApplyResyncRequest {
                        resolutions: vec![],
                    }),
                ))
            }),
            ("/clients/{company_id}/manual-link", Method::POST, || {
                Box::pin(clients_manual_link::manual_link(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path(Uuid::new_v4()),
                    Json(clients_manual_link::ManualLinkRequest {
                        facility_id: Uuid::new_v4(),
                        workflow: clients_manual_link::ManualLinkWorkflow::Intake,
                        run_id: "abc123".to_string(),
                    }),
                ))
            }),
            ("/clients/{company_id}", Method::DELETE, || {
                Box::pin(clients_companies::delete_company(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Path(Uuid::new_v4()),
                ))
            }),
            (
                "/clients/{company_id}/facilities/{facility_id}/elavon/link",
                Method::POST,
                || {
                    Box::pin(clients_elavon::link_facility_elavon(
                        axum::extract::State(empty_state()),
                        test_user(),
                        HeaderMap::new(),
                        Path((Uuid::new_v4(), Uuid::new_v4())),
                        Json(clients_elavon::LinkElavonRequest {
                            merchant_account_run_id: "abc123".to_string(),
                        }),
                    ))
                },
            ),
            (
                "/clients/{company_id}/facilities/{facility_id}/elavon/link",
                Method::DELETE,
                || {
                    Box::pin(clients_elavon::unlink_facility_elavon(
                        axum::extract::State(empty_state()),
                        test_user(),
                        HeaderMap::new(),
                        Path((Uuid::new_v4(), Uuid::new_v4())),
                    ))
                },
            ),
            (
                "/clients/{company_id}/facilities/{facility_id}/elavon/resync",
                Method::POST,
                || {
                    Box::pin(clients_elavon::resync_elavon_data(
                        axum::extract::State(empty_state()),
                        test_user(),
                        HeaderMap::new(),
                        Path((Uuid::new_v4(), Uuid::new_v4())),
                    ))
                },
            ),
            (
                "/clients/{company_id}/facilities/{facility_id}/tool-runs/{run_id}",
                Method::DELETE,
                || {
                    Box::pin(tool_runs::delete_tool_run(
                        axum::extract::State(empty_state()),
                        test_user(),
                        HeaderMap::new(),
                        Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
                    ))
                },
            ),
            ("/clients/sync", Method::POST, || {
                Box::pin(clients_sync::start_sync(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Query(clients_sync::StartSyncQuery { force: false }),
                ))
            }),
            ("/integrations/process-street/settings", Method::GET, || {
                Box::pin(process_street_settings::get_settings(
                    axum::extract::State(empty_state()),
                    test_user(),
                ))
            }),
            ("/integrations/process-street/settings", Method::PUT, || {
                Box::pin(process_street_settings::update_settings(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Json(
                        process_street_settings::UpdateProcessStreetSettingsRequest {
                            schedule_mode: "interval".to_string(),
                            sync_interval_hours: 24,
                            sync_time: None,
                            sync_timezone: None,
                            api_key: "test".to_string(),
                        },
                    ),
                ))
            }),
            ("/integrations/dropbox/settings", Method::GET, || {
                Box::pin(dropbox_settings::get_settings(
                    axum::extract::State(empty_state()),
                    test_user(),
                ))
            }),
            ("/integrations/dropbox/settings", Method::PUT, || {
                Box::pin(dropbox_settings::update_settings(
                    axum::extract::State(empty_state()),
                    test_user(),
                    HeaderMap::new(),
                    Json(dropbox_settings::UpdateDropboxSettingsRequest {
                        app_key: "key".to_string(),
                        app_secret: "secret".to_string(),
                        refresh_token: "token".to_string(),
                        root_namespace_id: "ns".to_string(),
                        root_path: "/".to_string(),
                    }),
                ))
            }),
            ("/auth/audit-logs", Method::GET, || {
                Box::pin(auth_audit_logs::list_audit_logs(
                    axum::extract::State(empty_state()),
                    test_user(),
                    Query(auth_audit_logs::AuditLogQuery {
                        limit: None,
                        before_id: None,
                        event_type: None,
                        user_id: None,
                    }),
                ))
            }),
            ("/auth/audit-logs/event-types", Method::GET, || {
                Box::pin(auth_audit_logs::list_event_types(
                    axum::extract::State(empty_state()),
                    test_user(),
                ))
            }),
            ("/auth/audit-logs/export", Method::POST, || {
                Box::pin(auth_audit_logs_export::export_audit_logs(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    HeaderMap::new(),
                    Json(auth_audit_logs_export::ExportAuditLogsRequest {
                        date_from: "2026-08-01T00:00:00Z".parse().unwrap(),
                        date_to: "2026-08-05T00:00:00Z".parse().unwrap(),
                        event_types: vec![],
                        user_ids: vec![],
                        ip_address: None,
                    }),
                ))
            }),
            ("/auth/audit-logs/export/preview", Method::POST, || {
                Box::pin(auth_audit_logs_export::preview_audit_logs(
                    axum::extract::State(empty_state()),
                    test_user(),
                    local_addr(),
                    Json(auth_audit_logs_export::ExportAuditLogsRequest {
                        date_from: "2026-08-01T00:00:00Z".parse().unwrap(),
                        date_to: "2026-08-05T00:00:00Z".parse().unwrap(),
                        event_types: vec![],
                        user_ids: vec![],
                        ip_address: None,
                    }),
                ))
            }),
        ]
    }

    #[tokio::test]
    async fn every_permission_route_is_covered_and_enforced() {
        let (_, manifest) = build(empty_state()).into_parts();
        let checks = permission_route_checks();

        let permission_entries: Vec<_> = manifest
            .iter()
            .filter(|entry| matches!(entry.access, RouteAccess::Permission { .. }))
            .collect();

        assert!(
            !permission_entries.is_empty(),
            "expected at least one RouteAccess::Permission route -- did classification regress?"
        );

        for entry in permission_entries {
            let check = checks
                .iter()
                .find(|(path, method, _)| *path == entry.path && *method == entry.method);

            let Some((_, _, assert_denied)) = check else {
                panic!(
                    "{} {} is classified as RouteAccess::Permission but has no entry in \
                     permission_route_checks() -- add one so this test can prove the handler \
                     actually enforces it",
                    entry.method, entry.path
                );
            };

            let response = assert_denied().await;

            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{} {} is classified as requiring a permission, but a caller with none got {} \
                 instead of 403 -- the handler may never call require_permission",
                entry.method,
                entry.path,
                response.status()
            );
        }
    }

    /// The inverse of the check above: every entry in
    /// `permission_route_checks()` must actually correspond to a real
    /// `Permission`-classified route in the manifest -- otherwise a
    /// stale entry (route renamed/removed) would silently stop proving
    /// anything.
    #[tokio::test]
    async fn every_permission_route_check_matches_a_real_manifest_entry() {
        let (_, manifest) = build(empty_state()).into_parts();

        for (path, method, _) in permission_route_checks() {
            let found = manifest.iter().any(|entry| {
                entry.path == path
                    && entry.method == method
                    && matches!(entry.access, RouteAccess::Permission { .. })
            });

            assert!(
                found,
                "permission_route_checks() has an entry for {method} {path}, but build()'s \
                 manifest has no matching RouteAccess::Permission route -- remove the stale \
                 check or fix the classification"
            );
        }
    }
}
