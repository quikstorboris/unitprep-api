//! Integration settings routes: Process Street, Dropbox, and Dropbox browsing.

use axum::{
    http::Method,
    routing::{get, put},
};

use crate::api::route_access::{GatedRouter, RouteAccess};
use crate::api::{
    dropbox_browse, dropbox_settings, process_street_settings, process_street_task_roles, AppState,
};

/// Integration settings and Dropbox browsing.
pub(super) fn integration_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
        // Which PS task names each role resolves through ("Task mapping"
        // section of the same page). Admin-only like its neighbors.
        .gated_route(
            "/integrations/process-street/task-roles",
            get(process_street_task_roles::get_task_roles),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.manage"],
                    action: "get_process_street_task_roles",
                },
            )],
        )
        .gated_route(
            "/integrations/process-street/task-roles/{role}",
            put(process_street_task_roles::update_task_role),
            [(
                Method::PUT,
                RouteAccess::Permission {
                    keys: &["integrations.manage"],
                    action: "update_process_street_task_role",
                },
            )],
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
}
