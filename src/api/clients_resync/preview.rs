//! `POST .../resync/preview` -- reports what a re-sync would do without writing anything.

use super::compare::CachedComparisons;
use super::compare::{
    classify_company_diff, classify_facility_diff, load_comparisons, PreviewResyncResponse,
};
use super::PERMISSION;
use crate::api::rls::try_response;
use crate::api::{internal_error, not_found, process_street_not_configured, AppState};
use crate::auth::AuthenticatedUser;
use axum::extract::{Json, Path, State};
use axum::response::{IntoResponse, Response};
use std::time::Instant;
use uuid::Uuid;

/// Requires `client_ops.perform` -- same gate `create_client` uses; this
/// reads live PS data but writes nothing, still gated the same way since
/// it's part of the same client-ops action, not a plain read.
pub async fn preview_resync(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "preview_resync", None, None)
            .await
    );

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    let comparisons = match load_comparisons(
        &state.db,
        user.user_id,
        &user.role_keys,
        &client,
        company_id,
    )
    .await
    {
        Ok(Some(comparisons)) => comparisons,
        Ok(None) => return not_found("company_not_found", "No such company.".to_string()),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "resync preview query failed");
            return internal_error("Could not preview the re-sync");
        }
    };

    let (company, facilities, people_by_run_id, merchant_account_refreshes) = comparisons;
    let (mut safe_update_count, mut conflicts) = classify_company_diff(&company);
    for facility in &facilities {
        let (facility_safe, facility_conflicts) = classify_facility_diff(facility);
        safe_update_count += facility_safe;
        conflicts.extend(facility_conflicts);
    }

    let merchant_accounts_to_refresh = merchant_account_refreshes.len();

    // Stashed for `apply_resync` to reuse -- see this module's own doc
    // comment. Overwrites any still-unused entry from an earlier
    // preview of this same company, which is exactly right: this is
    // the freshest fetch, so it's the one a follow-up apply should act on.
    state.resync_preview_cache.write().insert(
        company_id,
        CachedComparisons {
            computed_at: Instant::now(),
            comparisons: (
                company,
                facilities,
                people_by_run_id,
                merchant_account_refreshes,
            ),
        },
    );

    Json(PreviewResyncResponse {
        safe_update_count,
        conflicts,
        merchant_accounts_to_refresh,
    })
    .into_response()
}
