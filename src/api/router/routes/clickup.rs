//! ClickUp integration and per-client ClickUp link routes.

use axum::{
    http::Method,
    routing::{delete, get, post, put},
};

use crate::api::route_access::{GatedRouter, RouteAccess};
use crate::api::{
    clickup_connection, clickup_copy, clickup_duplicate_check, clickup_lookup, clickup_prefetch,
    clients_clickup_links, AppState,
};

/// Every ClickUp route.
pub(super) fn clickup_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
        // A user's own ClickUp connection. integrations.clickup is
        // granted per user (never via a role), and every handler only
        // touches the caller's own row.
        .gated_route(
            "/integrations/clickup/connection",
            get(clickup_connection::get_connection),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "get_clickup_connection",
                },
            )],
        )
        .gated_route(
            "/integrations/clickup/token",
            put(clickup_connection::save_token).delete(clickup_connection::remove_token),
            [
                (
                    Method::PUT,
                    RouteAccess::Permission {
                        keys: &["integrations.clickup"],
                        action: "save_clickup_token",
                    },
                ),
                (
                    Method::DELETE,
                    RouteAccess::Permission {
                        keys: &["integrations.clickup"],
                        action: "remove_clickup_token",
                    },
                ),
            ],
        )
        .gated_route(
            "/integrations/clickup/test",
            post(clickup_connection::test_connection),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "test_clickup_connection",
                },
            )],
        )
        // Link ClickUp (Company page): the catalog of onboarding lists for
        // the per-row dropdown, resolving a pasted ClickUp URL, per-company
        // match suggestions, and saving/removing links. All need the
        // per-user integrations.clickup permission; they call ClickUp with
        // the caller's own token.
        .gated_route(
            "/integrations/clickup/lists",
            get(clickup_lookup::list_clickup_lists),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "list_clickup_lists",
                },
            )],
        )
        .gated_route(
            "/integrations/clickup/resolve-url",
            post(clickup_lookup::resolve_clickup_url),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "resolve_clickup_url",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/clickup/suggestions",
            get(clickup_lookup::clickup_suggestions),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_suggestions",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/clickup/links",
            put(clients_clickup_links::save_clickup_links)
                .delete(clients_clickup_links::unlink_company_clickup),
            [
                (
                    Method::PUT,
                    RouteAccess::Permission {
                        keys: &["integrations.clickup"],
                        action: "save_clickup_links",
                    },
                ),
                (
                    Method::DELETE,
                    RouteAccess::Permission {
                        keys: &["integrations.clickup"],
                        action: "unlink_company_clickup",
                    },
                ),
            ],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/clickup-link",
            delete(clients_clickup_links::unlink_facility_clickup),
            [(
                Method::DELETE,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "unlink_facility_clickup",
                },
            )],
        )
        // Fire-and-forget cache warm-ups (read-only; answer 202 at once).
        .gated_route(
            "/integrations/clickup/prefetch",
            post(clickup_prefetch::prefetch_hierarchy),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "prefetch_clickup_hierarchy",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/clickup/prefetch-tasks",
            post(clickup_prefetch::prefetch_facility_tasks),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "prefetch_clickup_tasks",
                },
            )],
        )
        // Posting a finished duplicate check to the facility's ClickUp task.
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/clickup/duplicate-check-tasks",
            get(clickup_duplicate_check::duplicate_check_tasks),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_duplicate_check_tasks",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/clickup/duplicate-check-results",
            post(clickup_duplicate_check::post_duplicate_check_results),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "post_clickup_duplicate_check_results",
                },
            )],
        )
        // ClickUp Copy: copying a comment from a task in another facility's
        // list to its counterpart in this facility's list. See clickup_copy's
        // own module doc.
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/clickup/copy-pairs",
            get(clickup_copy::copy_pairs),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_copy_pairs",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/clickup/copy-comments",
            get(clickup_copy::copy_comments),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_copy_comments",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/facilities/{facility_id}/clickup/copy",
            post(clickup_copy::copy_comments_to_tasks),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_copy_comments",
                },
            )],
        )
        // ClickUp Copy, client level: one comment to many facilities. Big
        // copies run as background jobs the person polls.
        .gated_route(
            "/clients/{company_id}/clickup/bulk-tasks",
            get(clickup_copy::bulk_tasks),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_bulk_tasks",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/clickup/bulk-pairs",
            get(clickup_copy::bulk_pairs),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_bulk_pairs",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/clickup/bulk-comment",
            get(clickup_copy::bulk_comment),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_bulk_comment",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/clickup/bulk-copy",
            post(clickup_copy::bulk_copy),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_bulk_copy",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/clickup/copy-jobs",
            get(clickup_copy::list_copy_jobs),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_copy_jobs",
                },
            )],
        )
        .gated_route(
            "/clients/{company_id}/clickup/copy-jobs/{job_id}",
            get(clickup_copy::get_copy_job),
            [(
                Method::GET,
                RouteAccess::Permission {
                    keys: &["integrations.clickup"],
                    action: "clickup_copy_jobs",
                },
            )],
        )
}
