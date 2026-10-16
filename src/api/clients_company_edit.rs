//! Company page edits made by hand: the company's own information fields
//! and its Implementation Manager. Until now both only ever came from
//! Process Street, so a company whose "first time" Intake run was a
//! different facility (or that has none) could not be corrected in OO.
//!
//! **Edits survive sync.** Every field changed here is added to the
//! company's `manually_edited_fields`, which `clients::sync::
//! apply_company_refresh` already honours field by field -- the same
//! protection the New Client review screen gives its hand edits.
//!
//! **Implementation Manager** is separate from the info fields because it
//! is a user reference, not text. Sync only fills it while it is empty
//! (`staff_resolution::assign_implementation_manager`), so a manager chosen
//! here sticks; clearing it hands it back to the next sync.
//!
//! Both writes are gated to `client_ops.perform` (same as archive and
//! implementation-completed) and audit-logged after the commit.

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::api::clients_companies::StaffRef;
use crate::api::rls::{begin_for, try_response};
use crate::api::{bad_request, internal_error, not_found, user_agent_from, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;

const PERMISSION: &str = "client_ops.perform";

/// The company fields a person may edit. Names are column names *and*
/// `manually_edited_fields` keys, so they must match `refresh.rs`.
#[derive(Debug, Clone, Deserialize, sqlx::FromRow, ts_rs::TS)]
#[ts(export)]
pub struct CompanyInfoFields {
    pub legal_name: Option<String>,
    pub corporate_email: Option<String>,
    pub corporate_phone: Option<String>,
    pub corporate_address_street: Option<String>,
    pub corporate_address_city: Option<String>,
    pub corporate_address_state: Option<String>,
    pub corporate_address_zip: Option<String>,
    pub subdomain: Option<String>,
    pub website_url: Option<String>,
    pub accepted_payment_methods: Option<String>,
    pub accounting_basis: Option<String>,
    pub payment_scheme: Option<String>,
    pub offers_tenant_insurance_raw: Option<String>,
    pub insurance_provider: Option<String>,
}

impl CompanyInfoFields {
    fn pairs(&self) -> [(&'static str, Option<&str>); 14] {
        [
            ("legal_name", self.legal_name.as_deref()),
            ("corporate_email", self.corporate_email.as_deref()),
            ("corporate_phone", self.corporate_phone.as_deref()),
            (
                "corporate_address_street",
                self.corporate_address_street.as_deref(),
            ),
            (
                "corporate_address_city",
                self.corporate_address_city.as_deref(),
            ),
            (
                "corporate_address_state",
                self.corporate_address_state.as_deref(),
            ),
            (
                "corporate_address_zip",
                self.corporate_address_zip.as_deref(),
            ),
            ("subdomain", self.subdomain.as_deref()),
            ("website_url", self.website_url.as_deref()),
            (
                "accepted_payment_methods",
                self.accepted_payment_methods.as_deref(),
            ),
            ("accounting_basis", self.accounting_basis.as_deref()),
            ("payment_scheme", self.payment_scheme.as_deref()),
            (
                "offers_tenant_insurance_raw",
                self.offers_tenant_insurance_raw.as_deref(),
            ),
            ("insurance_provider", self.insurance_provider.as_deref()),
        ]
    }

    /// Trims every value; a blank one becomes `None` (stored as NULL).
    fn normalized(self) -> Self {
        let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Self {
            legal_name: clean(self.legal_name),
            corporate_email: clean(self.corporate_email),
            corporate_phone: clean(self.corporate_phone),
            corporate_address_street: clean(self.corporate_address_street),
            corporate_address_city: clean(self.corporate_address_city),
            corporate_address_state: clean(self.corporate_address_state),
            corporate_address_zip: clean(self.corporate_address_zip),
            subdomain: clean(self.subdomain),
            website_url: clean(self.website_url),
            accepted_payment_methods: clean(self.accepted_payment_methods),
            accounting_basis: clean(self.accounting_basis),
            payment_scheme: clean(self.payment_scheme),
            offers_tenant_insurance_raw: clean(self.offers_tenant_insurance_raw),
            insurance_provider: clean(self.insurance_provider),
        }
    }
}

type FieldChange<'a> = (&'static str, Option<&'a str>, Option<&'a str>);

/// The fields whose value differs between `current` and `requested`, as
/// `(name, before, after)`.
fn changed_fields<'a>(
    current: &'a CompanyInfoFields,
    requested: &'a CompanyInfoFields,
) -> Vec<FieldChange<'a>> {
    current
        .pairs()
        .into_iter()
        .zip(requested.pairs())
        .filter(|((_, before), (_, after))| before != after)
        .map(|((name, before), (_, after))| (name, before, after))
        .collect()
}

const SELECT_CURRENT: &str = "SELECT legal_name, corporate_email, corporate_phone, \
    corporate_address_street, corporate_address_city, corporate_address_state, \
    corporate_address_zip, subdomain, website_url, accepted_payment_methods, \
    accounting_basis, payment_scheme, offers_tenant_insurance_raw, insurance_provider \
    FROM clients.companies WHERE id = $1 FOR UPDATE";

/// `PUT /clients/{company_id}/info` -- saves every editable company field.
pub async fn update_company_info(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(request): Json<CompanyInfoFields>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "update_company_info",
            user_agent,
            None
        )
        .await
    );

    let requested = request.normalized();
    if requested.legal_name.is_none() {
        return bad_request("invalid_request", "A company needs a name.".to_string());
    }

    let mut tx = try_response!(begin_for(&state, &user, "Could not update this client").await);

    let current: Option<CompanyInfoFields> = match sqlx::query_as(SELECT_CURRENT)
        .bind(company_id)
        .fetch_optional(&mut *tx)
        .await
    {
        Ok(row) => row,
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, company_id = %company_id, "company lookup for info update failed");
            return internal_error("Could not update this client");
        }
    };
    let Some(current) = current else {
        let _ = tx.rollback().await;
        return not_found("not_found", "Client not found.".to_string());
    };

    let changes = changed_fields(&current, &requested);
    if changes.is_empty() {
        let _ = tx.rollback().await;
        return StatusCode::NO_CONTENT.into_response();
    }

    // Column names come from the fixed list in `pairs`, never from input.
    let mut update = QueryBuilder::new("UPDATE clients.companies SET ");
    for (index, (name, _, after)) in changes.iter().enumerate() {
        if index > 0 {
            update.push(", ");
        }
        update
            .push(*name)
            .push(" = ")
            .push_bind(after.map(str::to_string));
    }
    let names: Vec<String> = changes
        .iter()
        .map(|(name, _, _)| name.to_string())
        .collect();
    update
        .push(", manually_edited_fields = ARRAY(SELECT DISTINCT unnest(manually_edited_fields || ");
    update.push_bind(names);
    update.push(")) WHERE id = ").push_bind(company_id);

    if let Err(err) = update.build().execute(&mut *tx).await {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, company_id = %company_id, "company info update failed");
        return internal_error("Could not update this client");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit the company info update");
        return internal_error("Could not update this client");
    }

    let (mut before, mut after) = (Map::new(), Map::new());
    for (name, old, new) in &changes {
        before.insert(name.to_string(), json!(old));
        after.insert(name.to_string(), json!(new));
    }
    audit_log::record(
        &state.db,
        audit_log::event::CLIENT_INFO_UPDATED,
        user.user_id,
        "company",
        Some(&company_id.to_string()),
        audit_log::Change::from_to(Value::Object(before), Value::Object(after)),
        user_agent,
        None,
        json!({}),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct SetImplementationManagerRequest {
    /// `None` clears the assignment.
    pub user_id: Option<Uuid>,
}

/// `PUT /clients/{company_id}/implementation-manager`
pub async fn set_implementation_manager(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(request): Json<SetImplementationManagerRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "set_implementation_manager",
            user_agent,
            None,
        )
        .await
    );

    let mut tx = try_response!(begin_for(&state, &user, "Could not update this client").await);

    let previous: Result<Option<Option<Uuid>>, sqlx::Error> = sqlx::query_scalar(
        "SELECT implementation_manager_user_id FROM clients.companies WHERE id = $1 FOR UPDATE",
    )
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await;
    let previous = match previous {
        Ok(Some(previous)) => previous,
        Ok(None) => {
            let _ = tx.rollback().await;
            return not_found("not_found", "Client not found.".to_string());
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "company lookup for manager update failed");
            return internal_error("Could not update this client");
        }
    };

    if let Some(new_id) = request.user_id {
        let known: Result<Option<Uuid>, sqlx::Error> =
            sqlx::query_scalar("SELECT id FROM auth.staff_directory() WHERE id = $1")
                .bind(new_id)
                .fetch_optional(&mut *tx)
                .await;
        match known {
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = tx.rollback().await;
                return bad_request("invalid_request", "No such staff member.".to_string());
            }
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "staff lookup for manager update failed");
                return internal_error("Could not update this client");
            }
        }
    }

    if let Err(err) = sqlx::query(
        "UPDATE clients.companies SET implementation_manager_user_id = $1 WHERE id = $2",
    )
    .bind(request.user_id)
    .bind(company_id)
    .execute(&mut *tx)
    .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, company_id = %company_id, "implementation manager update failed");
        return internal_error("Could not update this client");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit the implementation manager update");
        return internal_error("Could not update this client");
    }

    if previous != request.user_id {
        audit_log::record(
            &state.db,
            audit_log::event::CLIENT_IMPLEMENTATION_MANAGER_CHANGED,
            user.user_id,
            "company",
            Some(&company_id.to_string()),
            audit_log::Change::from_to(json!(previous), json!(request.user_id)),
            user_agent,
            None,
            json!({}),
        )
        .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct StaffListResponse {
    pub staff: Vec<StaffRef>,
}

/// `GET /clients/staff` -- every active staff member, for the Implementation
/// Manager picker. Any authenticated caller, like the filter options.
pub async fn list_staff(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    let mut tx = try_response!(begin_for(&state, &user, "Could not load staff").await);

    let rows: Result<Vec<(Uuid, String, String)>, sqlx::Error> = sqlx::query_as(
        "SELECT id, first_name, last_name FROM auth.staff_directory() \
          ORDER BY first_name, last_name",
    )
    .fetch_all(&mut *tx)
    .await;
    let _ = tx.rollback().await;

    match rows {
        Ok(rows) => Json(StaffListResponse {
            staff: rows
                .into_iter()
                .map(|(id, first, last)| StaffRef {
                    id,
                    name: format!("{first} {last}"),
                })
                .collect(),
        })
        .into_response(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "staff list fetch failed");
            internal_error("Could not load staff")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(name: &str) -> CompanyInfoFields {
        CompanyInfoFields {
            legal_name: Some(name.to_string()),
            corporate_email: None,
            corporate_phone: None,
            corporate_address_street: None,
            corporate_address_city: None,
            corporate_address_state: None,
            corporate_address_zip: None,
            subdomain: None,
            website_url: None,
            accepted_payment_methods: None,
            accounting_basis: None,
            payment_scheme: None,
            offers_tenant_insurance_raw: None,
            insurance_provider: None,
        }
    }

    #[test]
    fn normalized_trims_and_blanks_become_none() {
        let mut input = fields("  KH Moss  ");
        input.corporate_email = Some("   ".to_string());
        input.website_url = Some(" https://khmoss.example ".to_string());
        let out = input.normalized();
        assert_eq!(out.legal_name.as_deref(), Some("KH Moss"));
        assert_eq!(out.corporate_email, None);
        assert_eq!(out.website_url.as_deref(), Some("https://khmoss.example"));
    }

    #[test]
    fn changed_fields_lists_only_differences() {
        let current = fields("KH Moss, LLC");
        let mut requested = fields("KH Moss");
        requested.corporate_phone = Some("5551234567".to_string());
        let changes = changed_fields(&current, &requested);
        let names: Vec<_> = changes.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(names, vec!["legal_name", "corporate_phone"]);
        assert_eq!(changes[0].1, Some("KH Moss, LLC"));
        assert_eq!(changes[0].2, Some("KH Moss"));
    }

    #[test]
    fn unchanged_input_yields_no_changes() {
        assert!(changed_fields(&fields("A"), &fields("A")).is_empty());
    }
}
