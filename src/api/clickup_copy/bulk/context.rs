//! Loading a bulk copy's context: the company's facilities sorted into
//! source, parent, destinations and unlinked.

use axum::response::Response;
use uuid::Uuid;

use super::super::lists::{list_info, FacilityRow, ListInfo};
use crate::api::{bad_request, conflict, internal_error, not_found, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

pub(super) struct BulkContext {
    pub(super) source: ListInfo,
    pub(super) parent: Option<ListInfo>,
    /// Every other facility with a linked list.
    pub(super) destinations: Vec<ListInfo>,
    /// Facilities with no list linked yet (named, so the page can say so).
    pub(super) unlinked: Vec<(Uuid, String)>,
}

/// The company's facilities sorted into source, parent, destinations and
/// unlinked, or the response to return instead.
pub(super) async fn prepare_bulk(
    state: &AppState,
    user: &AuthenticatedUser,
    company_id: Uuid,
    source_facility_id: Option<Uuid>,
) -> Result<BulkContext, Response> {
    let fail = |err: sqlx::Error| {
        tracing::error!(error = %err, user_id = %user.user_id, "ClickUp bulk copy lookup failed");
        internal_error("Could not prepare ClickUp Copy")
    };

    let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys)
        .await
        .map_err(fail)?;
    let parent_id: Option<Option<Uuid>> = sqlx::query_scalar(
        "SELECT clickup_parent_facility_id FROM clients.companies WHERE id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(fail)?;
    let rows: Vec<FacilityRow> = sqlx::query_as(
        "SELECT id, name, clickup_list_id, clickup_list_name, clickup_list_url \
           FROM clients.facilities WHERE company_id = $1 ORDER BY name",
    )
    .bind(company_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(fail)?;
    tx.commit().await.map_err(fail)?;

    let Some(parent_id) = parent_id else {
        return Err(not_found("not_found", "No such client.".to_string()));
    };

    let Some(source_id) = source_facility_id.or(parent_id) else {
        return Err(conflict(
            "no_source_facility",
            "Choose which facility to copy from, or designate a parent facility on the Company page."
                .to_string(),
        ));
    };

    let Some(source_row) = rows.iter().find(|row| row.0 == source_id).cloned() else {
        return Err(bad_request(
            "invalid_source_facility",
            "That facility does not belong to this client.".to_string(),
        ));
    };
    let source_name = source_row.1.clone();
    let Some(source) = list_info(source_row) else {
        return Err(conflict(
            "source_not_linked_to_clickup",
            format!("{source_name} has no ClickUp list linked, so there is nothing to copy from."),
        ));
    };

    let parent = parent_id
        .and_then(|id| rows.iter().find(|row| row.0 == id).cloned())
        .and_then(list_info);

    let mut destinations = Vec::new();
    let mut unlinked = Vec::new();
    for row in rows.into_iter().filter(|row| row.0 != source_id) {
        let (id, name) = (row.0, row.1.clone());
        match list_info(row) {
            Some(list) => destinations.push(list),
            None => unlinked.push((id, name)),
        }
    }

    Ok(BulkContext {
        source,
        parent,
        destinations,
        unlinked,
    })
}
