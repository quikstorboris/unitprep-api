//! Account, session and audit routes: health, TOTP/passkey step-up, users and their roles/permissions, auth configuration, audit logs, logout.

use axum::{
    http::Method,
    routing::{delete, get, post, put},
};

use crate::api::health::{health, health_db, whoami};
use crate::api::route_access::{GatedRouter, RouteAccess};
use crate::api::{
    auth_audit_logs, auth_audit_logs_export, auth_configuration, auth_logout,
    auth_passkey_reverify, auth_roles, auth_totp, auth_user_permissions, auth_user_role,
    auth_user_status, auth_users, AppState,
};

/// Health probes (public) and `whoami`.
pub(super) fn health_routes() -> GatedRouter<AppState> {
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
}

/// TOTP enrolment/step-up and passkey re-verification (all authenticated).
pub(super) fn step_up_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
}

/// User administration: list/export, status, roles, per-user permissions.
pub(super) fn user_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
                    keys: &["users.view"],
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
        // Direct (non-role) permission grants -- the Users page's
        // "Add permissions" dialog. user_permissions.manage is admin-only
        // today; see the ClickUp Integration design log for the plan to
        // extend it to department managers.
        .gated_route(
            "/auth/users/{id}/permissions",
            get(auth_user_permissions::list_user_permissions),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["user_permissions.manage"],
                    action: "list_user_permissions",
                },
            )],
        )
        .gated_route(
            "/auth/users/{id}/permissions/{permission_key}",
            put(auth_user_permissions::grant_user_permission)
                .delete(auth_user_permissions::revoke_user_permission),
            [
                (
                    Method::PUT,
                    RouteAccess::Permission {
                        keys: &["user_permissions.manage"],
                        action: "grant_user_permission",
                    },
                ),
                (
                    Method::DELETE,
                    RouteAccess::Permission {
                        keys: &["user_permissions.manage"],
                        action: "revoke_user_permission",
                    },
                ),
            ],
        )
}

/// Role catalogue and auth configuration.
pub(super) fn auth_admin_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
}

/// Security audit log list/export.
pub(super) fn audit_log_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
}

/// Sign-out (deliberately public: must work with a stale cookie).
pub(super) fn logout_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
}
