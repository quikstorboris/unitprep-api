//! Company/facility-name search -- Phase 2's cheap half. Uses PS's own
//! server-side `name` filter directly (see
//! `ProcessStreetClient::search_workflow_runs_by_name`), no local
//! index/cache needed. Called by `api::clients_search` alongside
//! `clients::sync`'s locally-indexed person-name search (the harder
//! half -- PS has no server-side search over form-field values, so that
//! one reads `clients.ps_person_index` instead of calling PS live).
//!
//! **Intake only, not all three workflows** -- narrowed 2026-08-31
//! (Boris's call). Every real facility has an Intake run; a Merchant
//! Account run only exists when that client actually uses Elavon, so
//! searching it too would make "no Merchant Account match" look like
//! "this facility doesn't exist" rather than "this facility doesn't use
//! Elavon." Facility identity (and the eventual "Add to OO" trigger)
//! only ever needs the Intake run's id regardless.

use crate::clients::known_workflows::INTAKE_WORKFLOW_ID;
use crate::process_street::{ProcessStreetClient, ProcessStreetError};

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub run_id: String,
    pub run_name: String,
    pub status: String,
}

/// Searches Intake run names for `query` (case-insensitive substring,
/// PS's own server-side filter) and returns every match.
pub async fn search_by_facility_name(
    client: &ProcessStreetClient,
    query: &str,
) -> Result<Vec<SearchResult>, ProcessStreetError> {
    let runs = client
        .search_workflow_runs_by_name(INTAKE_WORKFLOW_ID, query)
        .await?;
    Ok(runs
        .into_iter()
        .map(|r| SearchResult {
            run_id: r.id,
            run_name: r.name,
            status: r.status,
        })
        .collect())
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// The only test in this file, and it hits the live API -- there's
    /// no fixture to substitute for "does PS's server-side name filter
    /// actually work the way this module assumes." `#[ignore]`d, run
    /// explicitly with `cargo test -- --ignored searches_intake_runs_for_a_real_facility`.
    #[tokio::test]
    #[ignore = "needs a real Process Street API key -- see doc comment"]
    async fn searches_intake_runs_for_a_real_facility() {
        let _ = dotenvy::from_filename(".env.local");
        let config = crate::process_street::ProcessStreetConfig::from_env()
            .expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(config);

        let results = search_by_facility_name(&client, "highway")
            .await
            .expect("search must succeed against the live API");

        let intake_match = results
            .iter()
            .find(|r| r.run_name == "Highway 20 Self Storage - QMS Onboarding")
            .expect("Highway 20's Intake run must be found");
        assert_eq!(intake_match.status, "Active");

        // A query with no real match anywhere must yield an empty
        // result, not an error.
        let no_match = search_by_facility_name(&client, "zzz_definitely_not_a_real_facility_zzz")
            .await
            .expect("a query with no matches must still succeed");
        assert!(no_match.is_empty());
    }
}
