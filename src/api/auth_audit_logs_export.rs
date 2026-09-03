//! PDF-export and live-preview handlers for the admin audit log --
//! filtered-query formatting, distinct from `auth_audit_logs`'s paginated
//! keyset listing. Shares that module's query-builder filter helpers
//! (`push_event_type_filter`/`push_user_id_filter`) and `bad_request`
//! rather than duplicating them, since both modules run variations of the
//! same underlying filtered query against `auth.auth_audit_logs`.

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

use crate::api::auth_audit_logs::{bad_request, push_event_type_filter, push_user_id_filter};
use crate::api::{internal_error, AppState};
use crate::auth::{audit_log, begin_rls_transaction, AuthenticatedUser};
use crate::infrastructure::audit_log_pdf::{
    render_audit_log_pdf, AuditLogPdfReport, AuditLogPdfRow,
};

/// Hard ceiling on how many rows one export can carry -- fetched as
/// `EXPORT_ROW_CAP + 1` so a truncated result is detectable without a
/// separate `COUNT(*)` query (the PDF only needs "was this capped", not
/// the exact true total).
const EXPORT_ROW_CAP: i64 = 5000;

#[derive(Debug, Deserialize)]
pub struct ExportAuditLogsRequest {
    /// Both mandatory: required, non-`Option` fields, so a request missing
    /// either is rejected by the `Json` extractor itself before this
    /// handler ever runs -- the same convention this codebase already uses
    /// for a genuinely required field (see `CreateInviteRequest::email`).
    pub date_from: DateTime<Utc>,
    pub date_to: DateTime<Utc>,

    #[serde(default)]
    pub event_types: Vec<String>,

    #[serde(default)]
    pub user_ids: Vec<Uuid>,

    #[serde(default)]
    pub ip_address: Option<String>,
}

fn user_label(
    user_id: Option<Uuid>,
    first_name: Option<String>,
    last_name: Option<String>,
) -> String {
    match (user_id, first_name, last_name) {
        (None, _, _) => "—".to_string(),
        (Some(_), Some(first), Some(last)) => format!("{first} {last}"),
        (Some(id), _, _) => id.to_string(),
    }
}

fn plain_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A short, plain-text summary of what a row actually recorded --
/// before/after diffs win when present (the single most informative fact
/// about a change-type event), falling back to metadata, falling back to
/// nothing for a bare occurrence (a login, say).
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

/// Formats the report's "Generated at" line: UTC first (the report's one
/// declared reference timezone -- see the PDF's own "All times shown in
/// UTC" note, which is why per-row timestamps don't repeat a timezone),
/// with a Pacific-time equivalent alongside for a US-HQ reader's
/// convenience. `chrono_tz::America::Los_Angeles` carries the real IANA
/// DST rules, so this correctly reads PST in winter and PDT in summer --
/// a fixed "-8" would be wrong for roughly half the year. Also drops
/// `Utc::now()`'s raw sub-second precision (nanoseconds by default),
/// which otherwise made this line far noisier than the seconds-level
/// precision an audit report's header actually needs.
fn format_generated_at(now: DateTime<Utc>) -> String {
    let pacific = now.with_timezone(&chrono_tz::America::Los_Angeles);
    format!(
        "{} UTC ({} {})",
        now.format("%Y-%m-%d %H:%M:%S"),
        pacific.format("%Y-%m-%d %H:%M:%S"),
        pacific.format("%Z"),
    )
}

fn filter_summary_lines(request: &ExportAuditLogsRequest) -> Vec<String> {
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
            "Users: {}",
            if request.user_ids.is_empty() {
                "All users".to_string()
            } else {
                format!("{} selected", request.user_ids.len())
            }
        ),
        format!(
            "IP address: {}",
            request
                .ip_address
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("Any")
        ),
    ]
}

/// One matched row, already resolved (names, details summary) -- shared
/// output shape between the full PDF export and its JSON preview, since
/// both run the identical filtered query and differ only in what they do
/// with the result.
struct FilteredAuditLogRow {
    id: i64,
    created_at: DateTime<Utc>,
    event_type: String,
    actor_label: String,
    target_label: String,
    ip_address: Option<IpNetwork>,
    details: String,
}

/// Validates the parts of `ExportAuditLogsRequest` common to both the full
/// export and its preview -- the mandatory date range, and, if given, a
/// syntactically valid IP address. `Ok` carries the parsed IP filter (or
/// `None`); `Err` carries the exact `Response` to return.
fn validate_filters(request: &ExportAuditLogsRequest) -> Result<Option<IpNetwork>, Box<Response>> {
    if request.date_to < request.date_from {
        return Err(Box::new(bad_request(
            "invalid_date_range",
            "date_to must not be before date_from.".to_string(),
        )));
    }

    match request.ip_address.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => match raw.parse::<IpNetwork>() {
            Ok(ip) => Ok(Some(ip)),
            Err(_) => Err(Box::new(bad_request(
                "invalid_ip_address",
                "ip_address must be a valid IP address.".to_string(),
            ))),
        },
        _ => Ok(None),
    }
}

/// Runs the shared filtered query behind both the full PDF export and its
/// live preview: mandatory date range, optional event types/user ids/IP,
/// actor/target names resolved via `LEFT JOIN` (so PDF rendering and the
/// preview response both get names without a second round trip). Fetches
/// `row_cap + 1` rows so the caller can detect truncation without a
/// separate `COUNT(*)` query -- the second element of the returned tuple
/// is whether the true result set was larger than `row_cap`.
async fn fetch_filtered_audit_logs(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &ExportAuditLogsRequest,
    ip_filter: Option<IpNetwork>,
    row_cap: i64,
) -> Result<(Vec<FilteredAuditLogRow>, bool), sqlx::Error> {
    let mut builder: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
        "SELECT l.id, l.event_type, l.actor_user_id, au.first_name, au.last_name, \
         l.target_user_id, tu.first_name, tu.last_name, l.metadata, l.before_state, \
         l.after_state, l.ip_address, l.created_at \
         FROM auth.auth_audit_logs l \
         LEFT JOIN auth.users au ON au.id = l.actor_user_id \
         LEFT JOIN auth.users tu ON tu.id = l.target_user_id \
         WHERE l.created_at >= ",
    );
    builder.push_bind(request.date_from);
    builder
        .push(" AND l.created_at <= ")
        .push_bind(request.date_to);

    push_event_type_filter(&mut builder, "l.event_type", &request.event_types);
    push_user_id_filter(
        &mut builder,
        "l.actor_user_id",
        "l.target_user_id",
        &request.user_ids,
    );

    if let Some(ip) = ip_filter {
        builder.push(" AND l.ip_address = ").push_bind(ip);
    }

    builder
        .push(" ORDER BY l.id DESC LIMIT ")
        .push_bind(row_cap + 1);

    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        String,
        Option<Uuid>,
        Option<String>,
        Option<String>,
        Option<Uuid>,
        Option<String>,
        Option<String>,
        Value,
        Option<Value>,
        Option<Value>,
        Option<IpNetwork>,
        DateTime<Utc>,
    )> = builder.build_query_as().fetch_all(&mut **tx).await?;

    let truncated = rows.len() as i64 > row_cap;

    let mut mapped: Vec<FilteredAuditLogRow> = rows
        .into_iter()
        .map(
            |(
                id,
                event_type,
                actor_user_id,
                actor_first_name,
                actor_last_name,
                target_user_id,
                target_first_name,
                target_last_name,
                metadata,
                before_state,
                after_state,
                ip_address,
                created_at,
            )| FilteredAuditLogRow {
                id,
                created_at,
                event_type,
                actor_label: user_label(actor_user_id, actor_first_name, actor_last_name),
                target_label: user_label(target_user_id, target_first_name, target_last_name),
                ip_address,
                details: summarize_details(&before_state, &after_state, &metadata),
            },
        )
        .collect();

    if truncated {
        mapped.truncate(row_cap as usize);
    }

    Ok((mapped, truncated))
}

/// PDF export of the audit log, filtered and admin-gated. Distinct from
/// `list_audit_logs`'s keyset pagination: this fetches up to
/// `EXPORT_ROW_CAP` matching rows in one pass (capped, not paginated --
/// see the truncation note rendered into the PDF when hit).
pub async fn export_audit_logs(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ExportAuditLogsRequest>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    // Redundant with the RLS policy by design -- see list_audit_logs above.
    if let Err(response) = admin
        .require_permission(
            &state.db,
            "audit_logs.read",
            "export_audit_logs",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    let ip_filter = match validate_filters(&request) {
        Ok(ip) => ip,
        Err(response) => return *response,
    };

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for audit log export");
            return internal_error("Could not export the audit log");
        }
    };

    let admin_identity: Result<Option<(String, String, String)>, sqlx::Error> =
        sqlx::query_as("SELECT first_name, last_name, email::text FROM auth.users WHERE id = $1")
            .bind(admin.user_id)
            .fetch_optional(&mut *tx)
            .await;

    let admin_identity = match admin_identity {
        Ok(identity) => identity,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to resolve exporting admin's identity");
            return internal_error("Could not export the audit log");
        }
    };

    let generated_by = match admin_identity {
        Some((first, last, email)) => format!("{first} {last} ({email})"),
        None => admin.user_id.to_string(),
    };

    let (rows, truncated) = match fetch_filtered_audit_logs(
        &mut tx,
        &request,
        ip_filter,
        EXPORT_ROW_CAP,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "audit log export query failed");
            return internal_error("Could not export the audit log");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to commit audit log export transaction");
        return internal_error("Could not export the audit log");
    }

    let row_count = rows.len();
    let truncated_at = truncated.then_some(row_count);

    let pdf_rows: Vec<AuditLogPdfRow> = rows
        .into_iter()
        .map(|row| AuditLogPdfRow {
            // Compact on purpose -- full RFC3339 (with seconds and an
            // offset) was wide enough to force aggressive truncation in
            // the Time column, e.g. "2026-08-01T12:00:0…" (cuts off
            // mid-digit, worse than useless). The report states "All
            // times shown in UTC" once, so per-row precision beyond the
            // minute isn't needed to stay unambiguous.
            created_at: row.created_at.format("%Y-%m-%d %H:%M").to_string(),
            event_type: row.event_type,
            actor_label: row.actor_label,
            target_label: row.target_label,
            ip_address: row.ip_address.map(|ip| ip.to_string()).unwrap_or_default(),
            details: row.details,
        })
        .collect();

    let report = AuditLogPdfReport {
        report_title: "UnitPrep Security Log Report".to_string(),
        generated_by: generated_by.clone(),
        generated_at: format_generated_at(Utc::now()),
        filter_lines: filter_summary_lines(&request),
        rows: pdf_rows,
        truncated_at,
    };

    // CPU-bound (layout + font metrics over up to EXPORT_ROW_CAP rows), so
    // it runs on the blocking pool rather than tying up an async worker
    // thread for the duration of the render.
    let bytes = match tokio::task::spawn_blocking(move || render_audit_log_pdf(&report)).await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(error = %err, "audit log PDF render task panicked");
            return internal_error("Could not generate the audit log PDF");
        }
    };

    let filename = format!("unitprep-audit-log-{}.pdf", Utc::now().format("%Y-%m-%d"));

    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, "application/pdf".parse().unwrap());
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{filename}\"")
            .parse()
            .unwrap(),
    );

    audit_log::record(
        &state.db,
        audit_log::event::AUDIT_LOG_EXPORTED,
        audit_log::Subjects::by(admin.user_id),
        user_agent,
        ip_address,
        audit_log::Change::none(),
        serde_json::json!({
            "date_from": request.date_from,
            "date_to": request.date_to,
            "event_types": request.event_types,
            "user_ids": request.user_ids,
            "ip_address": request.ip_address,
            "row_count": row_count,
            "truncated": truncated,
        }),
    )
    .await;

    tracing::info!(
        admin_user_id = %admin.user_id,
        row_count,
        truncated,
        "audit log exported as PDF"
    );

    (response_headers, bytes).into_response()
}

#[derive(Debug, Serialize)]
pub struct AuditLogPreviewRow {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub event_type: String,
    pub actor_label: String,
    pub target_label: String,
    pub ip_address: Option<String>,
    pub details: String,
}

#[derive(Debug, Serialize)]
pub struct PreviewAuditLogsResponse {
    pub rows: Vec<AuditLogPreviewRow>,
    pub truncated: bool,
}

/// Deliberately smaller than `EXPORT_ROW_CAP` -- this backs a preview
/// panel refetched on every filter change, not the deliverable itself.
const PREVIEW_ROW_CAP: i64 = 25;

/// A lightweight JSON preview of the exact filtered query `export_audit_logs`
/// uses, so the export filters page can show "what will be in the report"
/// as the admin adjusts filters, without generating a full PDF on every
/// change. Not audited as a successful action, same reasoning as
/// `list_audit_logs`/`list_users`: this is a view, not a change -- a
/// *refused* preview (wrong role) is audited, matching every other
/// admin-gated read in this file.
pub async fn preview_audit_logs(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(request): Json<ExportAuditLogsRequest>,
) -> Response {
    let ip_address = Some(IpNetwork::from(addr.ip()));

    if let Err(response) = admin
        .require_permission(
            &state.db,
            "audit_logs.read",
            "preview_audit_logs",
            None,
            ip_address,
        )
        .await
    {
        return response;
    }

    let ip_filter = match validate_filters(&request) {
        Ok(ip) => ip,
        Err(response) => return *response,
    };

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for audit log preview");
            return internal_error("Could not preview the audit log");
        }
    };

    let (rows, truncated) = match fetch_filtered_audit_logs(
        &mut tx,
        &request,
        ip_filter,
        PREVIEW_ROW_CAP,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "audit log preview query failed");
            return internal_error("Could not preview the audit log");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to commit audit log preview transaction");
        return internal_error("Could not preview the audit log");
    }

    Json(PreviewAuditLogsResponse {
        rows: rows
            .into_iter()
            .map(|row| AuditLogPreviewRow {
                id: row.id,
                created_at: row.created_at,
                event_type: row.event_type,
                actor_label: row.actor_label,
                target_label: row.target_label,
                ip_address: row.ip_address.map(|ip| ip.to_string()),
                details: row.details,
            })
            .collect(),
        truncated,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{admin_user, empty_state, onboarding_manager_user};
    use axum::http::StatusCode;
    use chrono::{TimeZone, Timelike};

    fn test_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    fn export_request(date_from: DateTime<Utc>, date_to: DateTime<Utc>) -> ExportAuditLogsRequest {
        ExportAuditLogsRequest {
            date_from,
            date_to,
            event_types: Vec::new(),
            user_ids: Vec::new(),
            ip_address: None,
        }
    }

    #[tokio::test]
    async fn export_refuses_a_non_admin_role_without_touching_the_database() {
        let response = export_audit_logs(
            State(empty_state()),
            onboarding_manager_user(),
            test_addr(),
            HeaderMap::new(),
            Json(export_request(Utc::now(), Utc::now())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn export_refuses_an_inverted_date_range_without_touching_the_database() {
        let now = Utc::now();
        let response = export_audit_logs(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(export_request(now, now - chrono::Duration::days(1))),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn export_refuses_an_invalid_ip_address_without_touching_the_database() {
        let now = Utc::now();
        let mut request = export_request(now - chrono::Duration::days(1), now);
        request.ip_address = Some("not-an-ip".to_string());

        let response = export_audit_logs(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(request),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A syntactically valid request must reach the database -- the 500
    /// here (against the unreachable test pool) is the success signal,
    /// same convention used throughout this codebase's other handlers.
    #[tokio::test]
    async fn export_with_a_valid_request_reaches_the_database() {
        let now = Utc::now();
        let response = export_audit_logs(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(export_request(now - chrono::Duration::days(1), now)),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn preview_refuses_a_non_admin_role_without_touching_the_database() {
        let response = preview_audit_logs(
            State(empty_state()),
            onboarding_manager_user(),
            test_addr(),
            Json(export_request(Utc::now(), Utc::now())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn preview_refuses_an_inverted_date_range_without_touching_the_database() {
        let now = Utc::now();
        let response = preview_audit_logs(
            State(empty_state()),
            admin_user(),
            test_addr(),
            Json(export_request(now, now - chrono::Duration::days(1))),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn preview_with_a_valid_request_reaches_the_database() {
        let now = Utc::now();
        let response = preview_audit_logs(
            State(empty_state()),
            admin_user(),
            test_addr(),
            Json(export_request(now - chrono::Duration::days(1), now)),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn user_label_prefers_a_resolved_name_over_the_bare_uuid() {
        let id = Uuid::new_v4();
        assert_eq!(
            user_label(
                Some(id),
                Some("Ada".to_string()),
                Some("Lovelace".to_string())
            ),
            "Ada Lovelace"
        );
    }

    #[test]
    fn user_label_falls_back_to_the_uuid_when_unresolved() {
        let id = Uuid::new_v4();
        assert_eq!(user_label(Some(id), None, None), id.to_string());
    }

    #[test]
    fn user_label_is_an_em_dash_for_no_subject() {
        assert_eq!(user_label(None, None, None), "—");
    }

    #[test]
    fn summarize_details_diffs_only_the_changed_keys() {
        let before = Some(serde_json::json!({ "status": "active", "role": "admin" }));
        let after = Some(serde_json::json!({ "status": "deactivated", "role": "admin" }));

        let summary = summarize_details(&before, &after, &serde_json::json!({}));

        assert_eq!(summary, "status: active -> deactivated");
    }

    #[test]
    fn summarize_details_falls_back_to_metadata_without_a_diff() {
        let summary = summarize_details(&None, &None, &serde_json::json!({ "reissued": true }));

        assert_eq!(summary, "reissued=true");
    }

    #[test]
    fn summarize_details_is_empty_for_a_bare_occurrence() {
        let summary = summarize_details(&None, &None, &serde_json::json!({}));

        assert_eq!(summary, "");
    }

    #[test]
    fn format_generated_at_shows_pst_in_winter_and_pdt_in_summer() {
        let winter = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let summer = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();

        assert_eq!(
            format_generated_at(winter),
            "2026-01-15 12:00:00 UTC (2026-01-15 04:00:00 PST)"
        );
        assert_eq!(
            format_generated_at(summer),
            "2026-07-15 12:00:00 UTC (2026-07-15 05:00:00 PDT)"
        );
    }

    #[test]
    fn format_generated_at_drops_sub_second_precision() {
        let with_nanos = Utc
            .with_ymd_and_hms(2026, 8, 6, 14, 56, 21)
            .unwrap()
            .with_nanosecond(427_274_104)
            .unwrap();

        assert!(!format_generated_at(with_nanos).contains('.'));
    }

    #[test]
    fn filter_summary_lines_say_all_when_nothing_is_selected() {
        let now = Utc::now();
        let lines = filter_summary_lines(&export_request(now - chrono::Duration::days(1), now));

        assert!(lines[1].contains("All events"));
        assert!(lines[2].contains("All users"));
        assert!(lines[3].contains("Any"));
    }

    #[test]
    fn filter_summary_lines_reflect_selected_filters() {
        let now = Utc::now();
        let mut request = export_request(now - chrono::Duration::days(1), now);
        request.event_types = vec!["login_failed".to_string()];
        request.user_ids = vec![Uuid::new_v4()];
        request.ip_address = Some("203.0.113.1".to_string());

        let lines = filter_summary_lines(&request);

        assert!(lines[1].contains("login_failed"));
        assert!(lines[2].contains("1 selected"));
        assert!(lines[3].contains("203.0.113.1"));
    }
}
