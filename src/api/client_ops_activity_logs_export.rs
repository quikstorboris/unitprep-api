//! PDF-export and live-preview handlers for the Activity Logs viewer --
//! filtered-query formatting, distinct from `client_ops_activity_logs`'s
//! paginated keyset listing. Shares that module's query-builder filter
//! helpers and `bad_request`, and shares `infrastructure::audit_log_pdf`'s
//! entire renderer with `auth_audit_logs_export` (see that module's own
//! `report_title` field) -- both PDF exports are the same fixed-column
//! table with a different title and a different underlying query.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::api::client_ops_activity_logs::{bad_request, push_actor_filter, push_in_filter};
use crate::api::{internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::infrastructure::audit_log_pdf::{render_audit_log_pdf, AuditLogPdfReport, AuditLogPdfRow};

const EXPORT_ROW_CAP: i64 = 5000;
const PREVIEW_ROW_CAP: i64 = 25;
const PERMISSION: &str = "activity_logs.read";

#[derive(Debug, Deserialize)]
pub struct ExportActivityLogsRequest {
    pub date_from: DateTime<Utc>,
    pub date_to: DateTime<Utc>,

    #[serde(default)]
    pub event_types: Vec<String>,

    #[serde(default)]
    pub entity_types: Vec<String>,

    #[serde(default)]
    pub actor_user_ids: Vec<Uuid>,
}

/// "System" for the nil-UUID placeholder `clients::sync`'s scheduled
/// runs write as their actor (see that module's own `SYSTEM_USER_ID` doc
/// comment) -- every other row has a real actor by construction, same
/// reasoning `client_ops::audit_log`'s own module doc already states.
fn actor_label(actor_user_id: Option<Uuid>, first_name: Option<String>, last_name: Option<String>) -> String {
    match actor_user_id {
        None => "—".to_string(),
        Some(id) if id.is_nil() => "System (scheduled sync)".to_string(),
        Some(id) => match (first_name, last_name) {
            (Some(first), Some(last)) => format!("{first} {last}"),
            _ => id.to_string(),
        },
    }
}

fn entity_label(entity_type: &str, entity_id: Option<&str>) -> String {
    match entity_id {
        Some(id) => format!("{entity_type}: {id}"),
        None => entity_type.to_string(),
    }
}

fn plain_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Same reasoning as `auth_audit_logs_export::summarize_details`:
/// before/after diffs win when present, falling back to metadata, falling
/// back to nothing for a bare occurrence.
fn summarize_details(before: &Option<Value>, after: &Option<Value>, metadata: &Value) -> String {
    if let (Some(Value::Object(before)), Some(Value::Object(after))) = (before, after) {
        let mut parts = Vec::new();
        for (key, before_value) in before {
            let after_value = after.get(key);
            if Some(before_value) != after_value {
                parts.push(format!(
                    "{key}: {} -> {}",
                    plain_value(before_value),
                    after_value.map(plain_value).unwrap_or_default()
                ));
            }
        }
        if !parts.is_empty() {
            return parts.join("; ");
        }
    }

    if let Value::Object(map) = metadata {
        if !map.is_empty() {
            return map
                .iter()
                .map(|(key, value)| format!("{key}={}", plain_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
        }
    }

    String::new()
}

fn format_generated_at(now: DateTime<Utc>) -> String {
    let pacific = now.with_timezone(&chrono_tz::America::Los_Angeles);
    format!(
        "{} UTC ({} {})",
        now.format("%Y-%m-%d %H:%M:%S"),
        pacific.format("%Y-%m-%d %H:%M:%S"),
        pacific.format("%Z"),
    )
}

fn filter_summary_lines(request: &ExportActivityLogsRequest) -> Vec<String> {
    vec![
        format!(
            "Date range: {} to {}",
            request.date_from.format("%Y-%m-%d"),
            request.date_to.format("%Y-%m-%d")
        ),
        format!(
            "Events: {}",
            if request.event_types.is_empty() {
                "All events".to_string()
            } else {
                request.event_types.join(", ")
            }
        ),
        format!(
            "Entities: {}",
            if request.entity_types.is_empty() {
                "All entities".to_string()
            } else {
                request.entity_types.join(", ")
            }
        ),
        format!(
            "Users: {}",
            if request.actor_user_ids.is_empty() {
                "All users".to_string()
            } else {
                format!("{} selected", request.actor_user_ids.len())
            }
        ),
    ]
}

struct FilteredActivityLogRow {
    id: Uuid,
    created_at: DateTime<Utc>,
    event_type: String,
    actor_label: String,
    target_label: String,
    details: String,
}

fn validate_filters(request: &ExportActivityLogsRequest) -> Result<(), Box<Response>> {
    if request.date_to < request.date_from {
        return Err(Box::new(bad_request(
            "invalid_date_range",
            "date_to must not be before date_from.".to_string(),
        )));
    }
    Ok(())
}

async fn fetch_filtered_activity_logs(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &ExportActivityLogsRequest,
    row_cap: i64,
) -> Result<(Vec<FilteredActivityLogRow>, bool), sqlx::Error> {
    let mut builder: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
        "SELECT l.id, l.event_type, l.actor_user_id, au.first_name, au.last_name, \
         l.entity_type, l.entity_id, l.metadata, l.before_state, l.after_state, l.created_at \
         FROM client_ops.audit_log l \
         LEFT JOIN auth.users au ON au.id = l.actor_user_id \
         WHERE l.created_at >= ",
    );
    builder.push_bind(request.date_from);
    builder.push(" AND l.created_at <= ").push_bind(request.date_to);

    push_in_filter(&mut builder, "l.event_type", &request.event_types);
    push_in_filter(&mut builder, "l.entity_type", &request.entity_types);
    push_actor_filter(&mut builder, "l.actor_user_id", &request.actor_user_ids);

    builder.push(" ORDER BY l.id DESC LIMIT ").push_bind(row_cap + 1);

    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        Uuid,
        String,
        Option<Uuid>,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Value,
        Option<Value>,
        Option<Value>,
        DateTime<Utc>,
    )> = builder.build_query_as().fetch_all(&mut **tx).await?;

    let truncated = rows.len() as i64 > row_cap;

    let mut mapped: Vec<FilteredActivityLogRow> = rows
        .into_iter()
        .map(
            |(
                id,
                event_type,
                actor_user_id,
                actor_first_name,
                actor_last_name,
                entity_type,
                entity_id,
                metadata,
                before_state,
                after_state,
                created_at,
            )| FilteredActivityLogRow {
                id,
                created_at,
                event_type,
                actor_label: actor_label(actor_user_id, actor_first_name, actor_last_name),
                target_label: entity_label(&entity_type, entity_id.as_deref()),
                details: summarize_details(&before_state, &after_state, &metadata),
            },
        )
        .collect();

    if truncated {
        mapped.truncate(row_cap as usize);
    }

    Ok((mapped, truncated))
}

/// PDF export of the activity log, filtered and permission-gated. Mirrors
/// `auth_audit_logs_export::export_audit_logs` structurally; see this
/// module's own doc comment for what actually differs.
pub async fn export_activity_logs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ExportActivityLogsRequest>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "export_activity_logs", user_agent, ip_address)
        .await
    {
        return response;
    }

    if let Err(response) = validate_filters(&request) {
        return *response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for activity log export");
            return internal_error("Could not export the activity log");
        }
    };

    let exporter_identity: Result<Option<(String, String, String)>, sqlx::Error> =
        sqlx::query_as("SELECT first_name, last_name, email::text FROM auth.users WHERE id = $1")
            .bind(user.user_id)
            .fetch_optional(&mut *tx)
            .await;

    let exporter_identity = match exporter_identity {
        Ok(identity) => identity,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to resolve exporting user's identity");
            return internal_error("Could not export the activity log");
        }
    };

    let generated_by = match exporter_identity {
        Some((first, last, email)) => format!("{first} {last} ({email})"),
        None => user.user_id.to_string(),
    };

    let (rows, truncated) = match fetch_filtered_activity_logs(&mut tx, &request, EXPORT_ROW_CAP).await {
        Ok(result) => result,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "activity log export query failed");
            return internal_error("Could not export the activity log");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit activity log export transaction");
        return internal_error("Could not export the activity log");
    }

    let row_count = rows.len();
    let truncated_at = truncated.then_some(row_count);

    let pdf_rows: Vec<AuditLogPdfRow> = rows
        .into_iter()
        .map(|row| AuditLogPdfRow {
            created_at: row.created_at.format("%Y-%m-%d %H:%M").to_string(),
            event_type: row.event_type,
            actor_label: row.actor_label,
            target_label: row.target_label,
            ip_address: String::new(),
            details: row.details,
        })
        .collect();

    let report = AuditLogPdfReport {
        report_title: "UnitPrep Activity Log Report".to_string(),
        generated_by: generated_by.clone(),
        generated_at: format_generated_at(Utc::now()),
        filter_lines: filter_summary_lines(&request),
        rows: pdf_rows,
        truncated_at,
    };

    let bytes = match tokio::task::spawn_blocking(move || render_audit_log_pdf(&report)).await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(error = %err, "activity log PDF render task panicked");
            return internal_error("Could not generate the activity log PDF");
        }
    };

    let filename = format!("unitprep-activity-log-{}.pdf", Utc::now().format("%Y-%m-%d"));

    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, "application/pdf".parse().unwrap());
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{filename}\"").parse().unwrap(),
    );

    tracing::info!(
        user_id = %user.user_id,
        row_count,
        truncated,
        "activity log exported as PDF"
    );

    (response_headers, bytes).into_response()
}

#[derive(Debug, Serialize)]
pub struct ActivityLogPreviewRow {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub event_type: String,
    pub actor_label: String,
    pub target_label: String,
    pub details: String,
}

#[derive(Debug, Serialize)]
pub struct PreviewActivityLogsResponse {
    pub rows: Vec<ActivityLogPreviewRow>,
    pub truncated: bool,
}

/// A lightweight JSON preview of the exact filtered query
/// `export_activity_logs` uses -- same reasoning as
/// `auth_audit_logs_export::preview_audit_logs`.
pub async fn preview_activity_logs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(request): Json<ExportActivityLogsRequest>,
) -> Response {
    let ip_address = Some(IpNetwork::from(addr.ip()));

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "preview_activity_logs", None, ip_address)
        .await
    {
        return response;
    }

    if let Err(response) = validate_filters(&request) {
        return *response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for activity log preview");
            return internal_error("Could not preview the activity log");
        }
    };

    let (rows, truncated) = match fetch_filtered_activity_logs(&mut tx, &request, PREVIEW_ROW_CAP).await {
        Ok(result) => result,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "activity log preview query failed");
            return internal_error("Could not preview the activity log");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit activity log preview transaction");
        return internal_error("Could not preview the activity log");
    }

    let rows = rows
        .into_iter()
        .map(|row| ActivityLogPreviewRow {
            id: row.id,
            created_at: row.created_at,
            event_type: row.event_type,
            actor_label: row.actor_label,
            target_label: row.target_label,
            details: row.details,
        })
        .collect();

    Json(PreviewActivityLogsResponse { rows, truncated }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    fn valid_request() -> ExportActivityLogsRequest {
        ExportActivityLogsRequest {
            date_from: "2026-08-01T00:00:00Z".parse().unwrap(),
            date_to: "2026-08-05T00:00:00Z".parse().unwrap(),
            event_types: vec![],
            entity_types: vec![],
            actor_user_ids: vec![],
        }
    }

    #[tokio::test]
    async fn export_refuses_a_caller_without_the_permission_without_touching_the_database() {
        let response = export_activity_logs(
            State(empty_state()),
            test_user(),
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))),
            HeaderMap::new(),
            Json(valid_request()),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn validate_filters_rejects_a_reversed_date_range() {
        let mut request = valid_request();
        request.date_to = request.date_from - chrono::Duration::days(1);

        let result = validate_filters(&request);

        assert!(result.is_err());
    }

    #[test]
    fn entity_label_includes_the_id_when_present() {
        assert_eq!(entity_label("company", Some("abc-123")), "company: abc-123");
    }

    #[test]
    fn entity_label_falls_back_to_the_bare_entity_type() {
        assert_eq!(entity_label("sync_run", None), "sync_run");
    }

    #[test]
    fn actor_label_names_the_system_placeholder() {
        assert_eq!(actor_label(Some(Uuid::nil()), None, None), "System (scheduled sync)");
    }

    #[test]
    fn actor_label_prefers_the_resolved_name_over_the_bare_id() {
        let id = Uuid::new_v4();
        assert_eq!(
            actor_label(Some(id), Some("Boris".to_string()), Some("Maksimov".to_string())),
            "Boris Maksimov"
        );
    }
}
