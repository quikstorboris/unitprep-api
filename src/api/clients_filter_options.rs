//! `GET /clients/filter-options` -- the distinct `state`/`previous_pms`
//! values actually in use across `clients.facilities`, plus the users
//! currently assigned as an Implementation Manager or Sales Rep on any
//! `clients.companies` row, so the clients-page checkbox filters only
//! ever show real, in-use options rather than a hardcoded enum. Every
//! `staff` entry is real today only once Implementation Manager/Sales
//! Rep assignment is actually wired up (PS field mapping + backfill --
//! both a follow-up; the `staff_resolution` module meant to do it was never
//! built and has been removed);
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
use crate::api::rls::{begin_for, try_response};
use crate::api::{internal_error, AppState};
use crate::auth::AuthenticatedUser;
use crate::clients::us_states;

/// One state option for the filter checkbox list -- `name` is what's
/// shown and what gets sent back as the filter value (the canonical
/// full name, or the raw value verbatim if it's not a recognized US
/// state), `abbreviation` is included only so the frontend's dropdown
/// can also match a typed postal code (e.g. "CA") against a "California"
/// option it wouldn't otherwise find by label text alone.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct StateOption {
    pub name: String,
    pub abbreviation: Option<String>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct FilterOptionsResponse {
    pub states: Vec<StateOption>,
    pub previous_pms: Vec<String>,
    /// Every user currently assigned as an Implementation Manager or a
    /// Sales Rep on at least one company -- one combined, deduplicated
    /// list (not split by which role), since either filter's checkbox
    /// list is drawn from the same "real, in-use staff" set.
    pub staff: Vec<StaffRef>,
}

/// The one row [`get_filter_options`]'s combined query returns.
#[derive(sqlx::FromRow)]
struct FilterOptionsRow {
    states: Vec<String>,
    previous_pms: Vec<String>,
    staff: sqlx::types::Json<Vec<StaffRow>>,
}

/// One assigned staff member inside [`FilterOptionsRow::staff`].
#[derive(serde::Deserialize)]
struct StaffRow {
    id: Uuid,
    first_name: String,
    last_name: String,
}

pub async fn get_filter_options(
    State(state): State<AppState>,
    user: AuthenticatedUser,
) -> Response {
    let mut tx =
        try_response!(begin_for(&state, &user, "Could not load client filter options").await);

    // One statement, not three: each statement is a network round trip to
    // the database, and these three reads are independent. The staff
    // subquery is a JSON array because `auth.staff_directory()` is a
    // function (see 20260911130000_create_staff_identity_alias) and
    // calling it once per column would run it three times.
    let options: Result<FilterOptionsRow, sqlx::Error> = sqlx::query_as(
        "SELECT
            ARRAY(SELECT DISTINCT state FROM clients.facilities
                   WHERE state IS NOT NULL AND state <> '') AS states,
            ARRAY(SELECT DISTINCT previous_pms FROM clients.facilities
                   WHERE previous_pms IS NOT NULL AND previous_pms <> ''
                   ORDER BY previous_pms) AS previous_pms,
            COALESCE((
                SELECT json_agg(
                           json_build_object('id', sd.id,
                                             'first_name', sd.first_name,
                                             'last_name', sd.last_name)
                           ORDER BY sd.first_name, sd.last_name)
                  FROM auth.staff_directory() sd
                 WHERE sd.id IN (
                       SELECT implementation_manager_user_id FROM clients.companies
                        WHERE implementation_manager_user_id IS NOT NULL
                       UNION
                       SELECT sales_rep_user_id FROM clients.companies
                        WHERE sales_rep_user_id IS NOT NULL
                   )
            ), '[]'::json) AS staff",
    )
    .fetch_one(&mut *tx)
    .await;
    let FilterOptionsRow {
        states: raw_states,
        previous_pms,
        staff,
    } = match options {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "filter options fetch failed");
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

    let staff = staff
        .0
        .into_iter()
        .map(|person| StaffRef {
            id: person.id,
            name: format!("{} {}", person.first_name, person.last_name),
        })
        .collect();

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

    /// Real-database check of the combined query: raw spellings of one state
    /// collapse to a single canonical entry, blank and NULL values are
    /// dropped, previous-PMS values are distinct and sorted, and only staff
    /// actually assigned to a company are listed. Local `test-db` only; see
    /// `clickup_db_tests`' module doc for how to run it.
    #[tokio::test]
    #[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
    async fn get_filter_options_db_returns_canonical_states_sorted_pms_and_assigned_staff() {
        use crate::api::clickup_db_tests::{body_json, caller, superuser_pool};

        let _ = dotenvy::from_filename(".env.local");
        let superuser = superuser_pool();
        let tag = Uuid::new_v4().simple().to_string();
        let tag = &tag[..8];
        let odd_state = format!("Zzstate{tag}");
        let pms = format!("Zzpms{tag}");
        let last_name = format!("Zzstaff{tag}");

        let staffer: Uuid = sqlx::query_scalar(
            "INSERT INTO auth.users (email, first_name, last_name, company, status)
             VALUES ($1, 'Zed', $2, 'quikstor', 'active') RETURNING id",
        )
        .bind(format!("zz{tag}@example.test"))
        .bind(&last_name)
        .fetch_one(&superuser)
        .await
        .unwrap();
        let company: Uuid = sqlx::query_scalar(
            "INSERT INTO clients.companies (legal_name, source, implementation_manager_user_id)
             VALUES ($1, 'manual', $2) RETURNING id",
        )
        .bind(format!("Filter Co {tag}"))
        .bind(staffer)
        .fetch_one(&superuser)
        .await
        .unwrap();
        for (name, state, previous_pms) in [
            ("F1", Some("CA"), Some(pms.as_str())),
            ("F2", Some("California"), Some(pms.as_str())),
            ("F3", Some(odd_state.as_str()), None),
            ("F4", Some(""), Some("")),
        ] {
            sqlx::query(
                "INSERT INTO clients.facilities (company_id, name, source, state, previous_pms)
                 VALUES ($1, $2, 'manual', $3, $4)",
            )
            .bind(company)
            .bind(name)
            .bind(state)
            .bind(previous_pms)
            .execute(&superuser)
            .await
            .unwrap();
        }

        let state = AppState {
            db: crate::db::connect_test(),
            ..empty_state()
        };
        let response =
            get_filter_options(State(state), caller(Uuid::new_v4(), &["admin"], &[])).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = body_json(response).await;

        let states = body["states"].as_array().unwrap();
        let california: Vec<_> = states
            .iter()
            .filter(|s| s["name"] == "California")
            .collect();
        assert_eq!(
            california.len(),
            1,
            "CA and California must collapse to one entry"
        );
        assert_eq!(california[0]["abbreviation"], "CA");
        let odd = states.iter().find(|s| s["name"] == odd_state.as_str());
        assert!(odd.is_some_and(|s| s["abbreviation"].is_null()));
        assert!(
            states.iter().all(|s| s["name"] != ""),
            "blank states are dropped"
        );

        let previous_pms: Vec<&str> = body["previous_pms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(previous_pms.iter().filter(|v| **v == pms).count(), 1);
        assert!(previous_pms.iter().all(|v| !v.is_empty()));
        assert!(
            previous_pms.windows(2).all(|pair| pair[0] <= pair[1]),
            "sorted"
        );

        let staff_name = format!("Zed {last_name}");
        assert!(
            body["staff"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["name"] == staff_name.as_str()),
            "a staff member assigned to a company is listed"
        );

        sqlx::query("DELETE FROM clients.companies WHERE id = $1")
            .bind(company)
            .execute(&superuser)
            .await
            .unwrap();
        sqlx::query("DELETE FROM auth.users WHERE id = $1")
            .bind(staffer)
            .execute(&superuser)
            .await
            .unwrap();
    }
}
