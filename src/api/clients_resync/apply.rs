//! `POST .../resync/apply` -- writes the re-sync, honouring the caller's per-conflict choices.

use super::compare::{load_comparisons, usable_cache_entry};
use super::write::{write_all, ApplyError};
use super::PERMISSION;
use crate::api::rls::{begin_for, try_response};
use crate::api::{
    encryption_not_configured, internal_error, not_found, process_street_not_configured, AppState,
};
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;
use crate::clients::repository::IngestMerchantAccountError;
use axum::extract::{Json, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ConflictResolution {
    pub entity_type: String,
    pub entity_id: Uuid,
    pub field: String,
    /// `true` overwrites this one field from Process Street (and clears
    /// it from `manually_edited_fields`, since it no longer diverges);
    /// `false` -- or simply not listing this conflict at all -- keeps
    /// the manually-set value, same as the scheduled sync's own default.
    pub use_fresh: bool,
}

#[derive(Debug, Deserialize)]
pub struct ApplyResyncRequest {
    #[serde(default)]
    pub resolutions: Vec<ConflictResolution>,
}

#[derive(Debug, Serialize)]
pub struct ApplyResyncResponse {
    pub updated_count: usize,
    pub merchant_accounts_refreshed: usize,
}

/// The fields still protected after folding in this apply's own
/// resolutions -- a field resolved `use_fresh: true` for this exact
/// entity is dropped from the protected set (it no longer diverges from
/// Process Street); everything else stays exactly as stored.
pub(super) fn effective_protected_fields(
    stored: &[String],
    resolutions: &[ConflictResolution],
    entity_type: &str,
    entity_id: Uuid,
) -> Vec<String> {
    stored
        .iter()
        .filter(|field| {
            !resolutions.iter().any(|r| {
                r.use_fresh
                    && r.entity_type == entity_type
                    && r.entity_id == entity_id
                    && &r.field == *field
            })
        })
        .cloned()
        .collect()
}

pub async fn apply_resync(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
    Json(request): Json<ApplyResyncRequest>,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "apply_resync", None, None)
        .await
    {
        return response;
    }

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    // Single-use: a hit is consumed here whether or not it's still
    // fresh enough to use, so a stale leftover never lingers to be
    // mistaken for a later preview's own result.
    let cached = usable_cache_entry(state.resync_preview_cache.write().remove(&company_id));

    // On a miss the comparison is built HERE, before the write
    // transaction opens: `load_comparisons` reads the rows in its own
    // short transaction, calls Process Street with no transaction held,
    // and only then does the write transaction below begin. (This used to
    // open the write transaction first and call Process Street inside it.)
    // The rows it read are then a few seconds old by the time the writes
    // run -- the same staleness window the preview-cache path above has
    // always had, for up to `PREVIEW_CACHE_TTL`.
    let (company, facilities, people_by_run_id, merchant_account_refreshes) = match cached {
        Some(comparisons) => comparisons,
        None => match load_comparisons(
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
                tracing::error!(error = %err, user_id = %user.user_id, "resync apply query failed");
                return internal_error("Could not apply the re-sync");
            }
        },
    };

    let mut tx = try_response!(begin_for(&state, &user, "Could not apply the re-sync").await);

    let written = match write_all(
        &mut tx,
        &company,
        &facilities,
        &people_by_run_id,
        &merchant_account_refreshes,
        &request.resolutions,
    )
    .await
    {
        Ok(written) => written,
        Err(err) => {
            let _ = tx.rollback().await;
            return apply_error_response(err, user.user_id);
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit resync apply transaction");
        return internal_error("Could not apply the re-sync");
    }

    audit_log::record(
        &state.db,
        audit_log::event::SYNC_COMPLETED,
        user.user_id,
        "company",
        Some(&company_id.to_string()),
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({
            "trigger": "manual_resync",
            "updated_count": written.updated_count,
            "people_indexed": written.people_indexed,
            "merchant_accounts_refreshed": written.merchant_accounts_refreshed,
            "resolutions_applied": request.resolutions.iter().filter(|r| r.use_fresh).count(),
        }),
    )
    .await;

    Json(ApplyResyncResponse {
        updated_count: written.updated_count,
        merchant_accounts_refreshed: written.merchant_accounts_refreshed,
    })
    .into_response()
}

/// Logs a failed write step and picks the response: a missing
/// `CLIENT_PII_ENCRYPTION_KEY` is its own 503, everything else a plain 500.
fn apply_error_response(err: ApplyError, user_id: Uuid) -> Response {
    match err {
        ApplyError::Database { step, error } => {
            tracing::error!(error = %error, user_id = %user_id, "resync apply failed to {step}");
            internal_error("Could not apply the re-sync")
        }
        ApplyError::MerchantAccount { facility_id, error } => {
            tracing::error!(error = %error, user_id = %user_id, facility_id = %facility_id, "resync apply failed to refresh a facility's Merchant Account data");
            match error {
                IngestMerchantAccountError::Encryption(_) => encryption_not_configured(),
                IngestMerchantAccountError::Database(_) => {
                    internal_error("Could not apply the re-sync")
                }
            }
        }
        ApplyError::MerchantTasks { facility_id, error } => {
            tracing::error!(error = %error, user_id = %user_id, facility_id = %facility_id, "resync apply failed to refresh a facility's Merchant Account task statuses");
            internal_error("Could not apply the re-sync")
        }
    }
}
