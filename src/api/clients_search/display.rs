//! The live Process Street reads that enrich search rows with a Merchant
//! Account run's own display info (company name, masked EIN, business
//! address). Both are display enrichment only: a failed fetch degrades to
//! "no info" for that one run and never fails the search.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::matching::MaDisplayInfo;
use crate::clients::company_naming::resolve_company_name;
use crate::clients::merchant_account_correlation::Correlation;
use crate::clients::merchant_account_mapping::map_merchant_account_fields;
use crate::clients::search::SearchResult;
use crate::integrations::http::join_all_bounded;
use crate::process_street::ProcessStreetClient;

/// One live PS call per *distinct* correlated Merchant Account run,
/// concurrently, not one after another -- typically a handful at
/// most for a single company's worth of search results. Every
/// ambiguous candidate gets its own fetch too, not just unambiguous
/// ones, since a "Potential Duplicates" row still needs its own
/// suggested company name.
pub(super) async fn correlated(
    client: &ProcessStreetClient,
    correlations: &HashMap<String, Correlation>,
    user_id: Uuid,
) -> HashMap<String, MaDisplayInfo> {
    let distinct_ma_run_ids: HashSet<&str> = correlations
        .values()
        .flat_map(|c| match c {
            Correlation::Unambiguous(id) => std::slice::from_ref(id),
            Correlation::Ambiguous(ids) => ids.as_slice(),
        })
        .map(String::as_str)
        .collect();
    let fetches = distinct_ma_run_ids
        .iter()
        .map(|ma_run_id| async move { (*ma_run_id, client.get_run_form_fields(ma_run_id).await) });

    let mut ma_display: HashMap<String, MaDisplayInfo> = HashMap::new();
    for (ma_run_id, result) in join_all_bounded(fetches).await {
        let display = match result {
            Ok(fields) => {
                let mapped = map_merchant_account_fields(&fields);
                MaDisplayInfo {
                    company_name: resolve_company_name(None, Some(&mapped)),
                    ein_last_4: mapped.ein_last_4,
                    business_address: mapped.business_address,
                }
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    user_id = %user_id,
                    ma_run_id = %ma_run_id,
                    "failed to fetch correlated Merchant Account run's fields for display"
                );
                MaDisplayInfo::default()
            }
        };
        ma_display.insert(ma_run_id.to_string(), display);
    }
    ma_display
}

/// Every standalone (uncorrelated) Merchant Account match gets its
/// own live fetch too -- this list has no other in-flight fetch to
/// piggyback on the way correlated candidates do, but it's
/// exactly the list a real mistake was made from (2026-09-23:
/// "Milton Self Storage"'s run id, copied from here, manually linked
/// to a different real facility). Bounded the same way -- typically
/// a handful of results for one query, not a background job.
pub(super) async fn standalone(
    client: &ProcessStreetClient,
    merchant_account_results: &[SearchResult],
    user_id: Uuid,
) -> HashMap<String, MaDisplayInfo> {
    let fetches = merchant_account_results.iter().map(|r| async move {
        (
            r.run_id.as_str(),
            client.get_run_form_fields(&r.run_id).await,
        )
    });

    let mut standalone_display: HashMap<String, MaDisplayInfo> = HashMap::new();
    for (run_id, result) in join_all_bounded(fetches).await {
        let display = match result {
            Ok(fields) => {
                let mapped = map_merchant_account_fields(&fields);
                MaDisplayInfo {
                    company_name: None,
                    ein_last_4: mapped.ein_last_4,
                    business_address: mapped.business_address,
                }
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    user_id = %user_id,
                    ma_run_id = %run_id,
                    "failed to fetch a standalone Merchant Account match's fields for display"
                );
                MaDisplayInfo::default()
            }
        };
        standalone_display.insert(run_id.to_string(), display);
    }
    standalone_display
}
