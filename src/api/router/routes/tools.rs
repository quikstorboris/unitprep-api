//! Onboarding tool routes: unit-group workflow, dedup and tagger.

use axum::{
    extract::DefaultBodyLimit,
    http::Method,
    routing::{get, post},
};

use crate::api::route_access::{GatedRouter, RouteAccess};
use crate::api::{
    acknowledge_group_warnings, analyze, cancel_session, correct, correct_group, dedup,
    dedup_files, dedup_rematch, discover, exclude_group, exclude_groups, exempt, export,
    group_file_confirm, group_file_upload, resolve_unit_format, select_group_file,
    select_unit_file, tagger, unit_file_upload, upload, validate, AppState,
};

/// Unit-group workflow: upload, discover, validate, correct, analyze, export, session cancel.
pub(super) fn unit_group_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
}

/// Duplicate-tenant check.
pub(super) fn dedup_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
        .gated_route(
            "/dedup/check",
            post(dedup::check),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/classify-files",
            post(dedup_files::classify_files),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/classify-dropbox-folder",
            post(dedup_files::classify_dropbox_folder),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/file-requirements",
            get(dedup_files::file_requirements),
            [(Method::GET, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/import-dropbox",
            post(dedup::import_from_dropbox),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .gated_route(
            "/dedup/unidentified",
            post(dedup_rematch::set_unidentified_mode),
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
}

/// Template tagger (except `/tagger/check`, which has its own body limit).
pub(super) fn tagger_routes() -> GatedRouter<AppState> {
    GatedRouter::new()
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
}

/// Ceiling for `/tagger/check`'s upload specifically, well under the
/// router-wide `DefaultBodyLimit` -- a `.docx` template is XML plus
/// occasional embedded media, not a bulk data export, so 10MB comfortably
/// covers a real template while bounding a pathological upload much
/// tighter than the general 100MB ceiling meant for other endpoints.
const TAGGER_CHECK_BODY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// `/tagger/check` alone, split out purely so its tighter body limit
/// applies to this one route (same "split for layer scoping" pattern as
/// the rate-limited routes).
pub(super) fn tagger_check_route() -> GatedRouter<AppState> {
    GatedRouter::new()
        .gated_route(
            "/tagger/check",
            post(tagger::check),
            [(Method::POST, RouteAccess::Authenticated)],
        )
        .map_router(|r| r.layer(DefaultBodyLimit::max(TAGGER_CHECK_BODY_LIMIT_BYTES)))
}
