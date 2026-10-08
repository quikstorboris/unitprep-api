//! Changing what a Dedup report does with the tenants that have no
//! customer id (ignore them, or match them by name against everyone).
//!
//! Two entry points, one outcome. `set_unidentified_mode` acts on the live
//! session behind the results page. `rematch_tool_run` acts on a past run
//! from the facility's Onboarding Work tab, using the normalized records
//! the run kept. Either way the report is recomputed, the run's stored
//! report is replaced, and -- if the run has a stored output file -- the
//! file is regenerated so the download matches the report. (A copy that
//! was saved to Dropbox is not touched: Dropbox is the user's.)

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use unitprep_dedup::{DedupReport, TemplateNoteComposer, TenantRecord, UnidentifiedMode};

use crate::api::blocking::run_blocking;
use crate::api::dedup::ExportFormat;
use crate::api::dedup_blocking;
use crate::api::dedup_view::{build_report_view, DedupReportView};
use crate::api::rls::{begin_for, try_response};
use crate::api::tool_runs::facility_belongs_to_company;
use crate::api::{internal_error, not_found, session_not_found, AppState};
use crate::application::dedup_session_service::DedupSessionService;
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::tool_runs::{self, OutputFile};

const PERMISSION: &str = "client_ops.perform";

const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

#[derive(Debug, Deserialize)]
pub struct SetUnidentifiedModeRequest {
    pub session_id: String,
    pub mode: UnidentifiedMode,
}

#[derive(Debug, Deserialize)]
pub struct RematchRequest {
    pub mode: UnidentifiedMode,
}

#[derive(Debug, Serialize)]
pub struct RematchResponse {
    pub report: DedupReportView,
}

/// Recomputes the live session's report with the user's choice.
pub async fn set_unidentified_mode(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<SetUnidentifiedModeRequest>,
) -> Response {
    // Re-runs the whole report over every tenant: CPU-bound, so off the
    // async workers.
    let service = DedupSessionService::new(std::sync::Arc::clone(&state.dedup_sessions));
    let session_id = request.session_id.clone();
    let (owner_id, mode) = (user.user_id, request.mode);
    let recomputed = match run_blocking("re-check the session", move || {
        service.set_unidentified_mode(&session_id, owner_id, mode)
    })
    .await
    {
        Ok(recomputed) => recomputed,
        Err(response) => return response,
    };
    let Some((report, records)) = recomputed else {
        return session_not_found(&request.session_id);
    };

    let (view, report, records) = match dedup_blocking::report_view(report, records).await {
        Ok(built) => built,
        Err(response) => return response,
    };
    persist(&state, &user, &request.session_id, &view, &report, &records).await;

    Json(RematchResponse { report: view }).into_response()
}

/// Re-checks a past run from its kept records with the user's choice.
pub async fn rematch_tool_run(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
    Json(request): Json<RematchRequest>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok());

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "rematch_tool_run", user_agent, None)
        .await
    {
        return response;
    }

    let mut tx = try_response!(begin_for(&state, &user, "Could not re-check this run").await);

    match facility_belongs_to_company(&mut tx, facility_id, company_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.commit().await;
            return not_found("not_found", "No such facility.".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for tool run rematch failed");
            return internal_error("Could not re-check this run");
        }
    }

    #[allow(clippy::type_complexity)]
    let row: Result<Option<(String, Option<Vec<u8>>)>, sqlx::Error> = sqlx::query_as(
        "SELECT session_id, records_encrypted
           FROM client_ops.tool_runs
          WHERE id = $1 AND facility_id = $2",
    )
    .bind(run_id)
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await;

    let _ = tx.commit().await;

    let (session_id, blob) = match row {
        Ok(Some((session_id, Some(blob)))) => (session_id, blob),
        Ok(Some((_, None))) => {
            return (
                axum::http::StatusCode::CONFLICT,
                Json(crate::api::ApiErrorBody {
                    error: "rematch_unavailable",
                    message: "This run was recorded before re-checking was available, so it can't be re-checked. Run the check again."
                        .to_string(),
                }),
            )
                .into_response();
        }
        Ok(None) => {
            return not_found("tool_run_not_found", "No such run.".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "tool run lookup for rematch failed");
            return internal_error("Could not re-check this run");
        }
    };

    let records = match tool_runs::open_records(&session_id, &blob) {
        Ok(records) => records,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "tool run records could not be decrypted");
            return internal_error("Could not re-check this run");
        }
    };

    // Recompute the report and its view: CPU-bound, so off the async workers.
    let mode = request.mode;
    let (report, view, records) = match run_blocking("re-check the run", move || {
        let report = unitprep_dedup::run_with_options(records.clone(), &TemplateNoteComposer, mode);
        let view = build_report_view(&report, &records);
        (report, view, records)
    })
    .await
    {
        Ok(recomputed) => recomputed,
        Err(response) => return response,
    };

    persist(&state, &user, &session_id, &view, &report, &records).await;

    Json(RematchResponse { report: view }).into_response()
}

/// Stores the recomputed report on the run and refreshes its stored
/// output file (if it has one) to match.
async fn persist(
    state: &AppState,
    user: &AuthenticatedUser,
    session_id: &str,
    view: &DedupReportView,
    report: &DedupReport,
    records: &[TenantRecord],
) {
    let summary = serde_json::to_value(view).unwrap_or_default();
    let output = regenerate_output(state, user, session_id, report, records).await;

    tool_runs::update_report(
        &state.db,
        user.user_id,
        &user.role_keys,
        session_id,
        &summary,
        output
            .as_ref()
            .map(|(bytes, content_type, file_name)| OutputFile {
                bytes: bytes.clone(),
                content_type,
                file_name,
            }),
    )
    .await;
}

/// The run's stored output file regenerated from `report`, in the same
/// format and under the same name, or `None` when the run has none (or it
/// can't be regenerated, which is logged and never fails the re-check).
async fn regenerate_output(
    state: &AppState,
    user: &AuthenticatedUser,
    session_id: &str,
    report: &DedupReport,
    records: &[TenantRecord],
) -> Option<(Vec<u8>, String, String)> {
    let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys)
        .await
        .ok()?;
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT output_content_type, output_file_name
           FROM client_ops.tool_runs
          WHERE session_id = $1 AND output_bytes IS NOT NULL",
    )
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await
    .ok()?;
    let _ = tx.commit().await;

    let (Some(content_type), Some(file_name)) = row? else {
        return None;
    };

    let format = match content_type.as_str() {
        "text/csv" => ExportFormat::Csv,
        XLSX => ExportFormat::Xlsx,
        "application/zip" => ExportFormat::Both,
        _ => return None,
    };

    // A ZIP's two inner files are named after the ZIP itself.
    let stem = file_name.strip_suffix(".zip").unwrap_or(&file_name);
    let inner = (format!("{stem}.csv"), format!("{stem}.xlsx"));

    // Cloned into the closure (the blocking pool needs owned data); the
    // clone is cheap next to generating the file, and goes away with the
    // clone-reduction work in the refactor plan.
    match dedup_blocking::export(
        format,
        session_id.to_string(),
        report.clone(),
        records.to_vec(),
        inner,
    )
    .await
    {
        Ok((bytes, content_type, _report, _records)) => {
            Some((bytes, content_type.to_string(), file_name))
        }
        Err(_) => {
            tracing::error!(
                session_id,
                "could not regenerate the stored output after a re-check"
            );
            None
        }
    }
}
