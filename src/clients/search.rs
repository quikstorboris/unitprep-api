//! Company/facility-name search across the three Process Street
//! workflows -- Phase 2's cheap half. Uses PS's own server-side `name`
//! filter directly (see `ProcessStreetClient::search_workflow_runs_by_name`),
//! no local index/cache needed. Called by `api::clients_search` alongside
//! `clients::sync`'s locally-indexed person-name search (the harder
//! half -- PS has no server-side search over form-field values, so that
//! one reads `clients.ps_person_index` instead of calling PS live).

use crate::clients::known_workflows::{
    CONTRACT_ORDER_WORKFLOW_ID, INTAKE_WORKFLOW_ID, MERCHANT_ACCOUNT_WORKFLOW_ID,
};
use crate::process_street::{ProcessStreetClient, ProcessStreetError};

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub run_id: String,
    pub run_name: String,
    /// `intake` | `merchant_account` | `contract_order`.
    pub workflow: &'static str,
    pub status: String,
}

const WORKFLOWS: &[(&str, &str)] = &[
    (INTAKE_WORKFLOW_ID, "intake"),
    (MERCHANT_ACCOUNT_WORKFLOW_ID, "merchant_account"),
    (CONTRACT_ORDER_WORKFLOW_ID, "contract_order"),
];

/// Searches all three workflows' run names for `query` (case-insensitive
/// substring, PS's own server-side filter) and returns every match,
/// labeled by which workflow it came from.
///
/// **A single real facility routinely appears under more than one
/// workflow with a genuinely different title each time** -- confirmed
/// directly: Prairie Enterprises' Highway 20 facility is
/// `"Highway 20 Self Storage - QMS Onboarding"` on Intake but
/// `"Prairie Enterprises (Highway 20)"` on New Merchant Account. Callers
/// (the eventual search UI) must expect and group multiple results for
/// what is really one facility, not assume one match per facility or
/// that every match's title contains the same substring a human typed.
pub async fn search_by_facility_name(
    client: &ProcessStreetClient,
    query: &str,
) -> Result<Vec<SearchResult>, ProcessStreetError> {
    let mut results = Vec::new();
    for (workflow_id, workflow_label) in WORKFLOWS {
        let runs = client.search_workflow_runs_by_name(workflow_id, query).await?;
        results.extend(runs.into_iter().map(|r| SearchResult {
            run_id: r.id,
            run_name: r.name,
            workflow: workflow_label,
            status: r.status,
        }));
    }
    Ok(results)
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// The only test in this file, and it hits the live API -- there's
    /// no fixture to substitute for "does PS's server-side name filter
    /// actually work the way this module assumes." `#[ignore]`d, run
    /// explicitly with `cargo test -- --ignored searches_across_workflows_for_a_real_facility`.
    #[tokio::test]
    #[ignore = "needs a real Process Street API key -- see doc comment"]
    async fn searches_across_workflows_for_a_real_facility() {
        let _ = dotenvy::from_filename(".env.local");
        let config = crate::process_street::ProcessStreetConfig::from_env()
            .expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(config);

        let results = search_by_facility_name(&client, "highway")
            .await
            .expect("search must succeed against the live API");

        let intake_match = results
            .iter()
            .find(|r| r.workflow == "intake")
            .expect("Highway 20's Intake run must be found");
        assert_eq!(intake_match.run_name, "Highway 20 Self Storage - QMS Onboarding");

        let nma_match = results
            .iter()
            .find(|r| r.workflow == "merchant_account")
            .expect("Highway 20's New Merchant Account run must be found under its different real title");
        assert_eq!(nma_match.run_name, "Prairie Enterprises (Highway 20)");

        // Highway 20 DOES have a Contract Order run after all --
        // discovered via this exact search test, 2026-08-31. Earlier
        // sessions concluded "Highway 20 has no Contract Order run"
        // using the pre-fix Active-only default, the same class of gap
        // `list_workflow_runs`/`search_workflow_runs_by_name` now fix.
        // Its Contract Order run has a real, filled `migrating_from_system`
        // value ("Sentinel Winsen") -- the first one seen this
        // integration, see contract_order_mapping's own tests.
        let contract_order_match = results
            .iter()
            .find(|r| r.workflow == "contract_order")
            .expect("Highway 20's Contract Order run must be found -- confirmed real 2026-08-31");
        assert_eq!(contract_order_match.run_name, "Order for Highway 20 Self Storage");

        // A query with no real match anywhere must yield an empty
        // result, not an error.
        let no_match = search_by_facility_name(&client, "zzz_definitely_not_a_real_facility_zzz")
            .await
            .expect("a query with no matches must still succeed");
        assert!(no_match.is_empty());
    }
}
