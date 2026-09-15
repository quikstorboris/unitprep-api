//! Lists real `clients.companies` rows (with archiving) -- the data
//! behind the unified `/clients` page: the landing list after a
//! successful "Add to OO" create, and the entry point for Dedup/Unit
//! Groups/Template Tagger, which now key off a real company id instead
//! of the retired session-scoped client concept (see the vault's
//! Process Street Integration notes, 2026-09-01).
//!
//! Read (list) is any authenticated caller -- every tool under
//! `/clients/[clientId]/...` needs this list to even navigate, not just
//! onboarding staff. Archive/unarchive are real mutations, gated to
//! `client_ops.perform` same as create.

use axum::{
    extract::{Json, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use crate::api::client_ops_activity_logs::push_actor_filter;
use crate::api::{bad_request, internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;

const PERMISSION: &str = "client_ops.perform";

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

fn parse_comma_separated(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_uuid_list(raw: &str) -> Result<Vec<Uuid>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| Uuid::parse_str(value).map_err(|_| value.to_string()))
        .collect()
}

/// Filters for `GET /clients` -- every field optional and additive (all
/// present filters narrow the result together). `q` is free text,
/// matched only against facility-side data (never Implementation
/// Manager/Sales Rep/state/previous PMS -- see `push_text_search_filter`).
/// The four id/value filters are comma-separated strings rather than
/// repeated query keys, same convention as
/// `client_ops_activity_logs::ActivityLogQuery`'s own `actor_user_id`/
/// `event_type`/`entity_type` fields.
#[derive(Debug, Deserialize)]
pub struct ListCompaniesQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub implementation_manager_user_id: Option<String>,
    #[serde(default)]
    pub sales_rep_user_id: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub previous_pms: Option<String>,
}

/// `implementation_manager`/`sales_rep`'s wire shape -- just enough to
/// label a filter chip or a grid's group heading, never the rest of
/// `auth.users` (see `auth.staff_directory()`'s own doc comment for why
/// that's a deliberate, narrow exposure).
#[derive(Debug, Serialize)]
pub struct StaffRef {
    pub id: Uuid,
    pub name: String,
}

fn staff_ref(
    id: Option<Uuid>,
    first_name: Option<String>,
    last_name: Option<String>,
) -> Option<StaffRef> {
    id.map(|id| StaffRef {
        id,
        name: format!(
            "{} {}",
            first_name.unwrap_or_default(),
            last_name.unwrap_or_default()
        ),
    })
}

#[derive(Debug, Serialize)]
pub struct CompanySummary {
    pub id: Uuid,
    pub legal_name: String,
    pub created_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    /// Ordered, possibly empty -- enough for the list page to show
    /// "3 facilities: Highway 20, Carpentersville, Pyott Road" without
    /// a second round trip. Not paginated: this mirrors `facility_names`
    /// scale (a handful of sister facilities per real company, not
    /// hundreds), same assumption `clients.ps_person_index`'s own
    /// indexing already makes at this data's real size.
    pub facility_names: Vec<String>,
    /// `None` for every company today -- Implementation Manager/Sales
    /// Rep assignment (PS field mapping + backfill) is a follow-up once
    /// the exact PS field shape is confirmed; see
    /// `clients::staff_resolution`'s own module doc. Not a bug.
    pub implementation_manager: Option<StaffRef>,
    pub sales_rep: Option<StaffRef>,
}

#[derive(Debug, sqlx::FromRow)]
struct CompanyRow {
    id: Uuid,
    legal_name: String,
    created_at: DateTime<Utc>,
    archived_at: Option<DateTime<Utc>>,
    facility_names: Vec<String>,
    im_id: Option<Uuid>,
    im_first_name: Option<String>,
    im_last_name: Option<String>,
    sr_id: Option<Uuid>,
    sr_first_name: Option<String>,
    sr_last_name: Option<String>,
}

impl From<CompanyRow> for CompanySummary {
    fn from(row: CompanyRow) -> Self {
        CompanySummary {
            id: row.id,
            legal_name: row.legal_name,
            created_at: row.created_at,
            archived_at: row.archived_at,
            facility_names: row.facility_names,
            implementation_manager: staff_ref(row.im_id, row.im_first_name, row.im_last_name),
            sales_rep: staff_ref(row.sr_id, row.sr_first_name, row.sr_last_name),
        }
    }
}

/// `state`/`previous_pms` live on `clients.facilities`, not on the
/// company row being grouped -- an `EXISTS` against a fresh, unjoined
/// alias keeps this a "does this company have a facility matching one of
/// these values" filter without perturbing `facility_names`' own
/// aggregation. A plain `AND f.state IN (...)` against the already-joined
/// `f` alias would silently drop non-matching sister facilities out of
/// `facility_names` too, which is not what a state/PMS filter is for.
fn push_facility_exists_filter(
    builder: &mut QueryBuilder<Postgres>,
    column: &str,
    values: &[String],
) {
    if values.is_empty() {
        return;
    }

    builder.push(format!(
        " AND EXISTS (SELECT 1 FROM clients.facilities ff WHERE ff.company_id = c.id AND ff.{column} IN ("
    ));
    {
        let mut separated = builder.separated(", ");
        for value in values {
            separated.push_bind(value.clone());
        }
    }
    builder.push("))");
}

/// Facility-side-only free text search (`q`) -- company `legal_name`/
/// `dba_name`, facility `name`/`email`/`phone`/address columns, and
/// `clients.facility_people`'s underlying `clients.people` name/email.
/// Never Implementation Manager/Sales Rep/state/previous PMS -- those are
/// the separate checkbox filters, not text search, per the plan. Plain
/// leading-wildcard `ILIKE`, no trigram/tsvector index -- the same "not
/// worth it at this scale" call `clients_search`'s own person-index
/// query already makes.
fn push_text_search_filter(builder: &mut QueryBuilder<Postgres>, q: &str) {
    let pattern = format!("%{q}%");

    builder
        .push(" AND (c.legal_name ILIKE ")
        .push_bind(pattern.clone());
    builder
        .push(" OR c.dba_name ILIKE ")
        .push_bind(pattern.clone());
    builder.push(
        " OR EXISTS (SELECT 1 FROM clients.facilities qf WHERE qf.company_id = c.id AND (qf.name ILIKE ",
    );
    builder.push_bind(pattern.clone());
    builder
        .push(" OR qf.email ILIKE ")
        .push_bind(pattern.clone());
    builder
        .push(" OR qf.phone ILIKE ")
        .push_bind(pattern.clone());
    builder
        .push(" OR qf.street_address ILIKE ")
        .push_bind(pattern.clone());
    builder
        .push(" OR qf.city ILIKE ")
        .push_bind(pattern.clone());
    builder
        .push(" OR qf.state ILIKE ")
        .push_bind(pattern.clone());
    builder.push(" OR qf.zip ILIKE ").push_bind(pattern.clone());
    builder.push("))");
    builder.push(
        " OR EXISTS (SELECT 1 FROM clients.facilities pf \
             JOIN clients.facility_people fpq ON fpq.facility_id = pf.id \
             JOIN clients.people pq ON pq.id = fpq.person_id \
            WHERE pf.company_id = c.id AND (pq.full_name ILIKE ",
    );
    builder.push_bind(pattern.clone());
    builder.push(" OR pq.email ILIKE ").push_bind(pattern);
    builder.push(")))");
}

/// Any authenticated caller -- see this module's own doc comment.
/// Supports `q` (free text, facility-side only) plus comma-separated
/// `implementation_manager_user_id`/`sales_rep_user_id`/`state`/
/// `previous_pms` filters -- all additive. With no query params this
/// returns exactly what it always has (same rows, same order), plus the
/// two new `implementation_manager`/`sales_rep` fields (both `None`
/// until that assignment is wired up -- see `CompanySummary`'s own doc
/// comment).
pub async fn list_companies(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<ListCompaniesQuery>,
) -> Response {
    let implementation_manager_ids = match query
        .implementation_manager_user_id
        .as_deref()
        .map(parse_uuid_list)
    {
        Some(Ok(ids)) => ids,
        Some(Err(bad_value)) => {
            return bad_request(
                "invalid_implementation_manager_user_id",
                format!("\"{bad_value}\" is not a valid UUID."),
            )
        }
        None => Vec::new(),
    };
    let sales_rep_ids = match query.sales_rep_user_id.as_deref().map(parse_uuid_list) {
        Some(Ok(ids)) => ids,
        Some(Err(bad_value)) => {
            return bad_request(
                "invalid_sales_rep_user_id",
                format!("\"{bad_value}\" is not a valid UUID."),
            )
        }
        None => Vec::new(),
    };
    // Each selected state is a canonical full name (or an unrecognized
    // raw value passed through as-is -- see `clients::us_states`), but
    // `clients.facilities.state` may hold either that full name or its
    // postal abbreviation. Expand every selected value to all its raw
    // forms before matching, so choosing "California" also matches
    // facilities stored as "CA".
    let states: Vec<String> = query
        .state
        .as_deref()
        .map(parse_comma_separated)
        .unwrap_or_default()
        .iter()
        .flat_map(|value| crate::clients::us_states::raw_variants(value))
        .collect();
    let previous_pms_values = query
        .previous_pms
        .as_deref()
        .map(parse_comma_separated)
        .unwrap_or_default();
    let q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for company list");
            return internal_error("Could not load clients");
        }
    };

    let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT c.id, c.legal_name, c.created_at, c.archived_at, \
             COALESCE(array_agg(DISTINCT f.name) FILTER (WHERE f.name IS NOT NULL), '{}') AS facility_names, \
             im.id AS im_id, im.first_name AS im_first_name, im.last_name AS im_last_name, \
             sr.id AS sr_id, sr.first_name AS sr_first_name, sr.last_name AS sr_last_name \
        FROM clients.companies c \
        LEFT JOIN clients.facilities f ON f.company_id = c.id \
        LEFT JOIN auth.staff_directory() im ON im.id = c.implementation_manager_user_id \
        LEFT JOIN auth.staff_directory() sr ON sr.id = c.sales_rep_user_id \
        WHERE true",
    );

    push_actor_filter(
        &mut builder,
        "c.implementation_manager_user_id",
        &implementation_manager_ids,
    );
    push_actor_filter(&mut builder, "c.sales_rep_user_id", &sales_rep_ids);
    push_facility_exists_filter(&mut builder, "state", &states);
    push_facility_exists_filter(&mut builder, "previous_pms", &previous_pms_values);
    if let Some(q) = q {
        push_text_search_filter(&mut builder, q);
    }

    builder.push(
        " GROUP BY c.id, im.id, im.first_name, im.last_name, sr.id, sr.first_name, sr.last_name \
          ORDER BY c.archived_at IS NOT NULL, c.legal_name",
    );

    let companies: Result<Vec<CompanyRow>, sqlx::Error> =
        builder.build_query_as().fetch_all(&mut *tx).await;

    let companies = match companies {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "company list query failed");
            return internal_error("Could not load clients");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit company list transaction");
        return internal_error("Could not load clients");
    }

    let companies: Vec<CompanySummary> = companies.into_iter().map(CompanySummary::from).collect();

    Json(companies).into_response()
}

async fn set_archived(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    company_id: Uuid,
    archive: bool,
) -> Response {
    let user_agent = request_context(&headers);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            if archive {
                "archive_company"
            } else {
                "unarchive_company"
            },
            user_agent,
            None,
        )
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for company archive toggle");
            return internal_error("Could not update this client");
        }
    };

    let query = if archive {
        "UPDATE clients.companies SET archived_at = now() WHERE id = $1 AND archived_at IS NULL RETURNING id, legal_name"
    } else {
        "UPDATE clients.companies SET archived_at = NULL WHERE id = $1 AND archived_at IS NOT NULL RETURNING id, legal_name"
    };

    let updated: Result<Option<(Uuid, String)>, sqlx::Error> = sqlx::query_as(query)
        .bind(company_id)
        .fetch_optional(&mut *tx)
        .await;

    let updated = match updated {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, company_id = %company_id, "company archive toggle failed");
            return internal_error("Could not update this client");
        }
    };

    let Some((_, legal_name)) = updated else {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a no-op archive toggle");
        }
        // Either the id doesn't exist, or it's already in the requested
        // state -- either way there's nothing to report beyond 404, the
        // same "don't distinguish a real 404 from an RLS-filtered row"
        // reasoning this codebase already applies elsewhere.
        return (
            StatusCode::NOT_FOUND,
            Json(ApiErrorBody {
                error: "not_found",
                message: "Client not found, or already in the requested state.".to_string(),
            }),
        )
            .into_response();
    };

    audit_log::record(
        &state.db,
        if archive {
            audit_log::event::CLIENT_ARCHIVED
        } else {
            audit_log::event::CLIENT_UNARCHIVED
        },
        user.user_id,
        "company",
        Some(&company_id.to_string()),
        audit_log::Change::none(),
        user_agent,
        None,
        serde_json::json!({ "legal_name": legal_name }),
    )
    .await;

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit company archive toggle");
        return internal_error("Could not update this client");
    }

    tracing::info!(user_id = %user.user_id, company_id = %company_id, archive, "user toggled a client's archived state");

    StatusCode::NO_CONTENT.into_response()
}

pub async fn archive_company(
    state: State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> Response {
    set_archived(state, user, headers, company_id, true).await
}

pub async fn unarchive_company(
    state: State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> Response {
    set_archived(state, user, headers, company_id, false).await
}

/// Permanently deletes a company and everything under it -- every
/// `clients.facilities` row, and everything that in turn cascades from
/// those (policies, fees, taxes, facility_people links, Elavon/Contract
/// Order data), all via `ON DELETE CASCADE` already declared on those
/// tables' own foreign keys. Never touches `clients.people` itself: a
/// person can legitimately be linked to facilities under other
/// companies too, so only the link rows (`clients.facility_people`) go
/// away, same restraint `unlink_person_from_facility` already uses.
///
/// Distinct from archive/unarchive (`set_archived` above), which is
/// reversible and keeps the row around -- this is for a genuine mistake
/// (e.g. a test import, or one created from the wrong Process Street
/// runs) where archiving would just leave permanent clutter. Same
/// `client_ops.perform` gate as archive; no separate confirmation step
/// here since the frontend already asks before calling this.
pub async fn delete_company(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> Response {
    let user_agent = request_context(&headers);

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "delete_company", user_agent, None)
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for company delete");
            return internal_error("Could not delete this client");
        }
    };

    let deleted: Result<Option<(Uuid, String)>, sqlx::Error> =
        sqlx::query_as("DELETE FROM clients.companies WHERE id = $1 RETURNING id, legal_name")
            .bind(company_id)
            .fetch_optional(&mut *tx)
            .await;

    let deleted = match deleted {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, company_id = %company_id, "company delete failed");
            return internal_error("Could not delete this client");
        }
    };

    let Some((_, legal_name)) = deleted else {
        let _ = tx.rollback().await;
        return (
            StatusCode::NOT_FOUND,
            Json(ApiErrorBody {
                error: "not_found",
                message: "Client not found.".to_string(),
            }),
        )
            .into_response();
    };

    audit_log::record(
        &state.db,
        audit_log::event::CLIENT_DELETED,
        user.user_id,
        "company",
        Some(&company_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!({ "legal_name": legal_name }),
            serde_json::json!(null),
        ),
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit company delete");
        return internal_error("Could not delete this client");
    }

    tracing::info!(user_id = %user.user_id, company_id = %company_id, "user permanently deleted a client");

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn archiving_refuses_insufficient_permission_without_touching_anything() {
        let response = archive_company(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unarchiving_refuses_insufficient_permission_without_touching_anything() {
        let response = unarchive_company(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn deleting_refuses_insufficient_permission_without_touching_anything() {
        let response = delete_company(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    fn empty_query() -> ListCompaniesQuery {
        ListCompaniesQuery {
            q: None,
            implementation_manager_user_id: None,
            sales_rep_user_id: None,
            state: None,
            previous_pms: None,
        }
    }

    #[test]
    fn parse_comma_separated_trims_and_drops_empties() {
        assert_eq!(
            parse_comma_separated(" CA , TX ,, "),
            vec!["CA".to_string(), "TX".to_string()]
        );
    }

    #[test]
    fn parse_uuid_list_names_the_specific_value_that_failed_to_parse() {
        let a = Uuid::new_v4();

        let err = parse_uuid_list(&format!("{a},not-a-uuid")).unwrap_err();

        assert_eq!(err, "not-a-uuid");
    }

    #[test]
    fn staff_ref_is_none_when_no_id_is_present() {
        assert!(staff_ref(
            None,
            Some("Boris".to_string()),
            Some("Maksimov".to_string())
        )
        .is_none());
    }

    #[test]
    fn staff_ref_combines_first_and_last_name() {
        let id = Uuid::new_v4();
        let staff = staff_ref(
            Some(id),
            Some("Boris".to_string()),
            Some("Maksimov".to_string()),
        )
        .unwrap();

        assert_eq!(staff.id, id);
        assert_eq!(staff.name, "Boris Maksimov");
    }

    #[tokio::test]
    async fn list_companies_refuses_an_invalid_implementation_manager_id_without_touching_the_database(
    ) {
        let mut query = empty_query();
        query.implementation_manager_user_id = Some("not-a-uuid".to_string());

        let response = list_companies(State(empty_state()), test_user(), Query(query)).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_companies_refuses_an_invalid_sales_rep_id_without_touching_the_database() {
        let mut query = empty_query();
        query.sales_rep_user_id = Some("not-a-uuid".to_string());

        let response = list_companies(State(empty_state()), test_user(), Query(query)).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// No filters at all still reaches the database (any authenticated
    /// caller, same as before this endpoint gained filters) -- the 500
    /// here (against the unreachable test pool) is the success signal,
    /// same convention as `client_ops_activity_logs`'s own tests.
    #[tokio::test]
    async fn list_companies_with_no_filters_reaches_the_database() {
        let response =
            list_companies(State(empty_state()), test_user(), Query(empty_query())).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn list_companies_with_valid_filters_reaches_the_database() {
        let mut query = empty_query();
        query.q = Some("highway".to_string());
        query.implementation_manager_user_id = Some(Uuid::new_v4().to_string());
        query.state = Some("CA,TX".to_string());

        let response = list_companies(State(empty_state()), test_user(), Query(query)).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
