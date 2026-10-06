//! Client-ops tooling routes: QMS tags and activity logs.

use axum::{
    http::Method,
    routing::{get, patch, post, put},
};

use crate::api::route_access::{GatedRouter, RouteAccess};
use crate::api::{
    client_ops_activity_logs, client_ops_activity_logs_export, client_ops_qms_tags, AppState,
};

/// QMS tags and activity logs.
pub(super) fn client_ops_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
}
