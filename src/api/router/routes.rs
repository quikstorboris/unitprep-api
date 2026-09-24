//! The full route table -- every path this API serves, paired with the
//! `RouteAccess` that authorizes it (see `route_access`'s module doc for
//! why `GatedRouter` makes that pairing unavoidable). Split out from
//! `router/mod.rs` (2026-09-24) once that file crossed 1900 lines: this
//! table and `permission_gate_tests`'s proof that it's actually enforced
//! were together most of that growth -- two genuinely separable concerns
//! (route declarations vs. response-shaping middleware) that had been
//! sharing one file since before either existed.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::DefaultBodyLimit,
    http::Method,
    routing::{delete, get, patch, post, put},
};

use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};

use super::super::health::{health, health_db, whoami};
use super::super::route_access::{GatedRouter, RouteAccess};
use super::super::AppState;
use super::super::{
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
use super::rate_limit_exceeded_with_audit;

/// Ceiling for `/tagger/check`'s upload specifically, well under the
/// router-wide `DefaultBodyLimit` below -- a `.docx` template is XML plus
/// occasional embedded media, not a bulk data export, so 10MB comfortably
/// covers a real template while bounding a pathological upload much
/// tighter than the general 100MB ceiling meant for other endpoints.
const TAGGER_CHECK_BODY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// Builds the whole route tree via `GatedRouter` -- see `route_access`'s
/// module doc for why every route below carries an explicit `RouteAccess`
/// at its call site. Kept separate from `router()`/`with_response_layers`
/// so tests can call this directly (with a fixture `AppState`) purely to
/// read back the manifest, without needing to also build the response-
/// shaping layers below, none of which affect authorization.
pub(super) fn build(state: AppState) -> GatedRouter<()> {
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
