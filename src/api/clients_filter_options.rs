//! `GET /clients/filter-options` -- the distinct `state`/`previous_pms`
//! values actually in use across `clients.facilities`, plus the users
//! currently assigned as an Implementation Manager or Sales Rep on any
//! `clients.companies` row, so the clients-page checkbox filters only
//! ever show real, in-use options rather than a hardcoded enum. Every
//! `staff` entry is real today only once Implementation Manager/Sales
//! Rep assignment is actually wired up (PS field mapping + backfill --
//! both a follow-up, see `clients::staff_resolution`'s own module doc);
//! until then this legitimately returns an empty `staff` list, same "not
//! a bug" reasoning `CompanySummary`'s own doc comment gives.
//!
//! Any authenticated caller -- same reasoning as `clients_companies::
//! list_companies` (this is read-only discovery data for a filter UI,
//! not a client operation in its own right).

use std::collections::BTreeMap;

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use uuid::Uuid;

use crate::api::clients_companies::StaffRef;
use crate::api::{internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::us_states;

/// One state option for the filter checkbox list -- `name` is what's
/// shown and what gets sent back as the filter value (the canonical
/// full name, or the raw value verbatim if it's not a recognized US
/// state), `abbreviation` is included only so the frontend's dropdown
/// can also match a typed postal code (e.g. "CA") against a "California"
/// option it wouldn't otherwise find by label text alone.
#[derive(Debug, Serialize)]
pub struct StateOption {
    pub name: String,
    pub abbreviation: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FilterOptionsResponse {
    pub states: Vec<StateOption>,
    pub previous_pms: Vec<String>,
    /// Every user currently assigned as an Implementation Manager or a
    /// Sales Rep on at least one company -- one combined, deduplicated
    /// list (not split by which role), since either filter's checkbox
    /// list is drawn from the same "real, in-use staff" set.
    pub staff: Vec<StaffRef>,
}

pub async fn get_filter_options(
    State(state): State<AppState>,
    user: AuthenticatedUser,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for clients filter options");
            return internal_error("Could not load client filter options");
        }
    };

    let raw_states: Result<Vec<(String,)>, sqlx::Error> = sqlx::query_as(
        "SELECT DISTINCT state FROM clients.facilities WHERE state IS NOT NULL AND state <> ''",
    )
    .fetch_all(&mut *tx)
    .await;
    let raw_states = match raw_states {
        Ok(rows) => rows.into_iter().map(|(value,)| value).collect::<Vec<_>>(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "distinct state fetch failed");
            return internal_error("Could not load client filter options");
        }
    };
    // `clients.facilities.state` is raw PS free text -- some runs
    // answered "CA", others "California". Deduplicate by canonical full
    // name (see `clients::us_states`) so the checkbox list shows one
    // "California" entry, not one per raw spelling in use.
    let mut states_by_name: BTreeMap<String, Option<String>> = BTreeMap::new();
    for raw in raw_states {
        match us_states::canonical_name(&raw) {
            Some(name) => {
                states_by_name
                    .entry(name.to_string())
                    .or_insert_with(|| us_states::abbreviation_for(name).map(str::to_string));
            }
            None => {
                states_by_name.entry(raw).or_insert(None);
            }
        }
    }
    let states = states_by_name
        .into_iter()
        .map(|(name, abbreviation)| StateOption { name, abbreviation })
        .collect();

    let previous_pms: Result<Vec<(String,)>, sqlx::Error> = sqlx::query_as(
        "SELECT DISTINCT previous_pms FROM clients.facilities \
          WHERE previous_pms IS NOT NULL AND previous_pms <> '' ORDER BY previous_pms",
    )
    .fetch_all(&mut *tx)
    .await;
    let previous_pms = match previous_pms {
        Ok(rows) => rows.into_iter().map(|(value,)| value).collect(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "distinct previous_pms fetch failed");
            return internal_error("Could not load client filter options");
        }
    };

    // auth.staff_directory() (see 20260911130000_create_staff_identity_alias)
    // is used here rather than a plain auth.users join for the same
    // reason CompanySummary::from needs it: auth.users' own RLS only lets
    // a caller see their own row or every row if admin.
    let staff: Result<Vec<(Uuid, String, String)>, sqlx::Error> = sqlx::query_as(
        "SELECT sd.id, sd.first_name, sd.last_name
           FROM auth.staff_directory() sd
          WHERE sd.id IN (
                SELECT implementation_manager_user_id FROM clients.companies
                 WHERE implementation_manager_user_id IS NOT NULL
                UNION
                SELECT sales_rep_user_id FROM clients.companies
                 WHERE sales_rep_user_id IS NOT NULL
            )
          ORDER BY sd.first_name, sd.last_name",
    )
    .fetch_all(&mut *tx)
    .await;
    let staff = match staff {
        Ok(rows) => rows
            .into_iter()
            .map(|(id, first_name, last_name)| StaffRef {
                id,
                name: format!("{first_name} {last_name}"),
            })
            .collect(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "assigned staff fetch failed");
            return internal_error("Could not load client filter options");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit clients filter options transaction");
        return internal_error("Could not load client filter options");
    }

    Json(FilterOptionsResponse {
        states,
        previous_pms,
        staff,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    /// Any authenticated caller reaches the database (no permission
    /// gate) -- the 500 here (against the unreachable test pool) is the
    /// success signal, same convention as
    /// `client_ops_activity_logs`'s own tests.
    #[tokio::test]
    async fn get_filter_options_reaches_the_database() {
        let response = get_filter_options(State(empty_state()), test_user()).await;

        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
