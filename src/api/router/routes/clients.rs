//! Client directory, facility and sync routes.

use axum::{
    http::Method,
    routing::{delete, get, post, put},
};

use crate::api::route_access::{GatedRouter, RouteAccess};
use crate::api::{
    clients_clickup_parent, clients_companies, clients_create, clients_detail,
    clients_dropbox_folder, clients_elavon, clients_facility_people,
    clients_facility_policies_edit, clients_filter_options, clients_implementation_status,
    clients_manual_link, clients_onboarding_summary, clients_preview, clients_resync,
    clients_search, clients_sync, dedup_rematch, tool_runs, AppState,
};

/// Client search/create/list/archive/resync/detail and the onboarding summary.
pub(super) fn client_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
        // Requires client_ops.perform -- see clients_implementation_status's
        // own module doc. PUT marks the implementation completed, DELETE
        // reopens it; both idempotent.
        .gated_route(
            "/clients/{company_id}/implementation-completed",
            put(clients_implementation_status::mark_implementation_completed)
                .delete(clients_implementation_status::reopen_implementation),
            [
                (
                    Method::PUT,
                    RouteAccess::Permission {
                        keys: &["client_ops.perform"],
                        action: "mark_implementation_completed",
                    },
                ),
                (
                    Method::DELETE,
                    RouteAccess::Permission {
                        keys: &["client_ops.perform"],
                        action: "reopen_implementation",
                    },
                ),
            ],
        )
        // Requires client_ops.perform -- see clients_clickup_parent's own
        // module doc. The designated parent facility for ClickUp Copy.
        .gated_route(
            "/clients/{company_id}/clickup-parent",
            put(clients_clickup_parent::set_clickup_parent),
            [(
                Method::PUT,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "set_clickup_parent",
                },
            )],
        )
        // Requires client_ops.perform. PUT marks "no ClickUp project",
        // DELETE clears it; both idempotent.
        .gated_route(
            "/clients/{company_id}/clickup-waiver",
            put(clients_clickup_parent::waive_clickup_project)
                .delete(clients_clickup_parent::clear_clickup_waiver),
            [
                (
                    Method::PUT,
                    RouteAccess::Permission {
                        keys: &["client_ops.perform"],
                        action: "waive_clickup_project",
                    },
                ),
                (
                    Method::DELETE,
                    RouteAccess::Permission {
                        keys: &["client_ops.perform"],
                        action: "clear_clickup_waiver",
                    },
                ),
            ],
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
}

/// Facility policies, Elavon, Dropbox folder, people and tool runs.
pub(super) fn facility_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
            "/clients/{company_id}/facilities/{facility_id}/tool-runs/{run_id}/rematch",
            post(dedup_rematch::rematch_tool_run),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "rematch_tool_run",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/tool-runs/{run_id}/output",
            get(tool_runs::download_tool_run_output),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "download_tool_run_output",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/tool-runs/{run_id}/source",
            get(tool_runs::download_tool_run_source),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["client_ops.perform"],
                    action: "download_tool_run_source",
                },
            )],
        )
}

/// Process Street sync trigger and status.
pub(super) fn client_sync_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
}
