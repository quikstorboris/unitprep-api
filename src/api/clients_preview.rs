//! Preview step for the "Add to OO" confirmation screen: live-fetches
//! and maps every selected Intake run (no company/facility role
//! assigned yet -- that's a per-row choice made *on* the confirmation
//! screen, see `api::clients_create`'s own doc comment) and returns
//! both a `MappedCompany` and a `MappedFacility` view of each one.
//! Writes nothing -- `create_client` is the real trigger; this exists
//! so the frontend has real values to show (and let a manager edit)
//! before that call happens, instead of Create blindly re-deriving
//! values the user never got to see.
//!
//! **Suggested Legal Name, same source as search's "Company" column**
//! (2026-09-01): a facility's own Intake run usually has its Corporate
//! Info fields blank (PS only fills them in on the "first-time"
//! facility of a company -- see the vault's sister-site writeup), so
//! `company.legal_name` from Intake alone is often empty even for a
//! real, named company. Whenever a run correlates to its own Merchant
//! Account run (`clients::merchant_account_correlation`, by matching
//! the Merchant Account run's title against this run's own facility
//! name -- see that module's own doc comment for why the earlier
//! shared-owner-email signal didn't actually work), this fetches that
//! run too and applies `company_naming::resolve_company_name` -- the
//! same naming rule `create` used to apply itself before this preview
//! step existed, now applied where the user can actually see and
//! correct it.
//!
//! **One combined concurrent fetch batch, not two sequential phases**
//! (2026-09-02): correlating a run needs *a* title to search for, but
//! not necessarily its live-mapped `facility.name` -- the frontend
//! already has each run's own raw PS title from the search step it
//! just came from (the exact same text `clients_search` itself already
//! correlates against). Taking it as `run_name` on the request means
//! correlation -- and therefore knowing which Merchant Account runs to
//! fetch -- no longer has to wait for the Intake fetches to finish
//! first. A real run's full field list is 100+ fields (~6 paginated
//! round trips, ~4s, confirmed against live PS), so collapsing "fetch
//! every Intake run, THEN fetch every correlated Merchant Account run"
//! into one single concurrent batch roughly halves total latency
//! versus running those as two back-to-back concurrent phases.
//!
//! **The user has already resolved any ambiguity by the time this
//! request exists** (2026-09-02, corrected): `clients_search`'s
//! "Potential Duplicates" rows exist so a manager can pick which
//! Merchant Account run is the real one for a facility -- that pick
//! *is* the resolution, not a fact this endpoint should try to
//! rediscover on its own. So `PreviewRunRequest::merchant_account_run_id`,
//! when the frontend sends it (a duplicate candidate the user actually
//! selected), always overrides whatever `correlate_by_title` infers for
//! that run -- including overriding a would-be-ambiguous result. Only a
//! run with *no* explicit choice falls back to auto-correlation at all.

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::company_naming::resolve_company_name;
use crate::clients::intake_mapping::{map_intake_fields, MappedCompany, MappedFacility};
use crate::clients::merchant_account_correlation::{
    correlate_by_title, merchant_account_run_titles, Correlation, IntakeRunTitle,
};
use crate::clients::merchant_account_mapping::map_merchant_account_fields;

#[derive(Debug, Deserialize)]
pub struct PreviewRunRequest {
    pub run_id: String,
    /// The run's own raw PS title, exactly as `clients_search` already
    /// has it from the search step -- lets correlation happen without
    /// an extra live fetch first. See this module's own doc comment.
    pub run_name: String,
    /// Set when this run was one of `clients_search`'s "Potential
    /// Duplicates" candidates and the user picked *this specific*
    /// Merchant Account run on the search page -- always wins over
    /// whatever `correlate_by_title` would infer on its own, since the
    /// ambiguity search surfaced is already resolved by the time a
    /// preview request exists at all. See this module's own doc
    /// comment ("the user has already resolved it").
    #[serde(default)]
    pub merchant_account_run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PreviewClientsRequest {
    pub runs: Vec<PreviewRunRequest>,
}

#[derive(Debug, Serialize)]
pub struct PreviewedRun {
    pub run_id: String,
    /// PS's own `Is_this_their_first_time_filling_out_this_form?` for
    /// this specific run -- `None` when unanswered. Purely a signal for
    /// `pickCompanySourceRun` (`unitprep-ui`) to prefer the run PS
    /// itself marks authoritative for company data over one that merely
    /// resolved *a* legal name (e.g. via a stray `Company_Name:` answer
    /// or a Merchant Account correlation) while its own Corporate Info
    /// section stayed blank. See `clients::intake_mapping::
    /// MappedIntakeRun::is_first_time`'s own doc comment for why this
    /// isn't folded into `company` itself.
    pub is_first_time: Option<bool>,
    /// This run's data read as a company -- relevant only if this row
    /// ends up designated Company on the confirmation screen.
    /// `legal_name` may already reflect a correlated Merchant Account
    /// run's Legal Name, not just this run's own Intake field -- see
    /// this module's own doc comment.
    pub company: MappedCompany,
    /// This run's data read as a facility -- relevant only if this row
    /// ends up designated Facility. Every selected run gets both views;
    /// PS's own Intake form doesn't distinguish "company" vs "facility"
    /// data at the field level, the confirmation screen's own toggle is
    /// what decides which view is actually used.
    pub facility: MappedFacility,
    /// The Merchant Account run this facility/company correlates to, if
    /// any -- auto-correlated or the caller's own explicit choice, same
    /// resolution `apply_suggested_legal_name` already used for
    /// `company.legal_name` above. Carried through unchanged by the
    /// confirmation screen into `api::clients_create`'s own request, so
    /// Create can actually ingest the Elavon data this run resolved to
    /// -- previously computed here and then silently dropped, so nothing
    /// ever wrote to `clients.facility_merchant_accounts` at all
    /// (2026-09-03 fix; see `clients::create`'s own doc comment).
    pub merchant_account_run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PreviewClientsResponse {
    pub runs: Vec<PreviewedRun>,
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "invalid_request",
            message: message.to_string(),
        }),
    )
        .into_response()
}

fn process_street_not_configured() -> Response {
    tracing::warn!("client preview attempted with Process Street not configured");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiErrorBody {
            error: "process_street_not_configured",
            message: "Process Street integration is not configured on this server.".to_string(),
        }),
    )
        .into_response()
}

/// Lets each request row's own explicit `merchant_account_run_id` --
/// the user's actual pick from `clients_search`'s "Potential
/// Duplicates" rows, when present -- override whatever
/// `correlate_by_title` auto-inferred for that run. See this module's
/// own doc comment for why the explicit choice always wins, including
/// over an already-unambiguous auto-correlation.
fn apply_explicit_merchant_account_choices(
    mut merchant_account_run_ids: std::collections::HashMap<String, String>,
    runs: &[PreviewRunRequest],
) -> std::collections::HashMap<String, String> {
    for r in runs {
        if let Some(ma_run_id) = &r.merchant_account_run_id {
            merchant_account_run_ids.insert(r.run_id.clone(), ma_run_id.clone());
        }
    }
    merchant_account_run_ids
}

/// Overrides `company.legal_name` with the correlated Merchant
/// Account's resolved name, when one exists -- leaves it exactly as
/// Intake mapped it (often blank, per this module's own doc comment)
/// when this run has no unambiguous correlation, or the correlated
/// run's fetch/resolution didn't produce a name.
fn apply_suggested_legal_name(
    mut company: MappedCompany,
    run_id: &str,
    merchant_account_run_ids: &std::collections::HashMap<String, String>,
    suggested_legal_names: &std::collections::HashMap<String, Option<String>>,
) -> MappedCompany {
    if let Some(ma_run_id) = merchant_account_run_ids.get(run_id) {
        if let Some(Some(suggested)) = suggested_legal_names.get(ma_run_id) {
            company.legal_name = Some(suggested.clone());
        }
    }
    company
}

/// Requires only authentication, not a particular permission -- same
/// reasoning as `clients_search::search_clients`: read-only discovery
/// data, nothing written or mutated.
pub async fn preview_clients(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<PreviewClientsRequest>,
) -> Response {
    if request.runs.is_empty() {
        return bad_request("runs must not be empty.");
    }

    let Some(client) = state.process_street.as_ref() else {
        return process_street_not_configured();
    };

    // Correlation only needs each run's own title text (already on the
    // request) plus the locally-indexed Merchant Account titles -- no
    // live PS call, so this can (and does) happen before any of the
    // real per-run fetches below start. Closed before those, same
    // discipline `clients_search` already follows: never hold a DB
    // transaction open across network I/O.
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for client preview");
            return internal_error("Could not load these runs from Process Street");
        }
    };

    let merchant_account_titles = match merchant_account_run_titles(&mut tx).await {
        Ok(titles) => titles,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "merchant-account title fetch failed");
            return internal_error("Could not load these runs from Process Street");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit client preview transaction");
        return internal_error("Could not load these runs from Process Street");
    }

    let intake_titles: Vec<IntakeRunTitle> = request
        .runs
        .iter()
        .map(|r| IntakeRunTitle { run_id: r.run_id.clone(), title_text: r.run_name.clone() })
        .collect();
    let auto_correlated: std::collections::HashMap<String, String> =
        correlate_by_title(&intake_titles, &merchant_account_titles)
            .into_iter()
            .filter_map(|(run_id, correlation)| match correlation {
                Correlation::Unambiguous(ma_run_id) => Some((run_id, ma_run_id)),
                Correlation::Ambiguous(_) => None,
            })
            .collect();
    let merchant_account_run_ids = apply_explicit_merchant_account_choices(auto_correlated, &request.runs);
    let distinct_ma_run_ids: std::collections::HashSet<&str> =
        merchant_account_run_ids.values().map(String::as_str).collect();

    // One combined concurrent batch -- every selected run's own Intake
    // fields AND every distinct correlated Merchant Account run's
    // fields, all in flight together. See this module's own doc
    // comment for why this can be one batch now instead of two
    // sequential ones.
    enum FetchKind {
        Intake,
        MerchantAccount,
    }
    // Boxed so the two differently-shaped async blocks below share one
    // concrete type -- required to `.chain()` them into a single
    // `join_all` batch; no two async blocks have the same type on
    // their own, even when structurally identical.
    type BoxedFetch<'a> = futures::future::BoxFuture<
        'a,
        (FetchKind, &'a str, Result<Vec<crate::process_street::FormField>, crate::process_street::ProcessStreetError>),
    >;
    let intake_fetches = request.runs.iter().map(|r| {
        let run_id = r.run_id.as_str();
        Box::pin(async move { (FetchKind::Intake, run_id, client.get_run_form_fields(run_id).await) }) as BoxedFetch
    });
    let ma_fetches = distinct_ma_run_ids.iter().map(|ma_run_id| {
        Box::pin(async move {
            (FetchKind::MerchantAccount, *ma_run_id, client.get_run_form_fields(ma_run_id).await)
        }) as BoxedFetch
    });

    let mut mapped_runs: std::collections::HashMap<String, crate::clients::intake_mapping::MappedIntakeRun> =
        std::collections::HashMap::with_capacity(request.runs.len());
    let mut suggested_legal_names: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();

    for (kind, run_id, result) in futures::future::join_all(intake_fetches.chain(ma_fetches)).await {
        match kind {
            FetchKind::Intake => {
                let fields = match result {
                    Ok(fields) => fields,
                    Err(err) => {
                        tracing::error!(error = %err, user_id = %user.user_id, run_id = %run_id, "failed to fetch a run's fields for preview");
                        return internal_error("Could not load this run from Process Street");
                    }
                };
                mapped_runs.insert(run_id.to_string(), map_intake_fields(&fields));
            }
            FetchKind::MerchantAccount => {
                let mapped_ma = match result {
                    Ok(fields) => Some(map_merchant_account_fields(&fields)),
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            user_id = %user.user_id,
                            ma_run_id = %run_id,
                            "failed to fetch a correlated Merchant Account run's fields for preview"
                        );
                        None
                    }
                };
                let suggested_name = mapped_ma.and_then(|ma| resolve_company_name(None, Some(&ma)));
                suggested_legal_names.insert(run_id.to_string(), suggested_name);
            }
        }
    }

    let mut runs = Vec::with_capacity(request.runs.len());
    for r in &request.runs {
        let mapped = mapped_runs.remove(&r.run_id).expect("every run_id was mapped above");

        let company = apply_suggested_legal_name(
            mapped.company,
            &r.run_id,
            &merchant_account_run_ids,
            &suggested_legal_names,
        );

        runs.push(PreviewedRun {
            run_id: r.run_id.clone(),
            is_first_time: mapped.is_first_time,
            company,
            facility: mapped.facility,
            merchant_account_run_id: merchant_account_run_ids.get(&r.run_id).cloned(),
        });
    }

    Json(PreviewClientsResponse { runs }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn empty_runs_is_rejected_without_touching_anything() {
        let response = preview_clients(
            State(empty_state()),
            test_user(),
            Json(PreviewClientsRequest { runs: vec![] }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn missing_process_street_config_returns_service_unavailable() {
        let response = preview_clients(
            State(empty_state()),
            test_user(),
            Json(PreviewClientsRequest {
                runs: vec![PreviewRunRequest {
                    run_id: "run-1".to_string(),
                    run_name: "Run One".to_string(),
                    merchant_account_run_id: None,
                }],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn a_correlated_run_with_a_resolved_name_overrides_intake_legal_name() {
        // Highway 20's own Intake run has a real Corporate Info section
        // (it's the "first-time" facility); Carpentersville/Pyott Road
        // do not -- but this proves the override applies regardless of
        // what Intake itself said, once a correlation resolves.
        let company = MappedCompany {
            legal_name: Some("Whatever Intake Said".to_string()),
            ..Default::default()
        };
        let mut merchant_account_run_ids = std::collections::HashMap::new();
        merchant_account_run_ids.insert("run-highway-20".to_string(), "ma-highway-20".to_string());
        let mut suggested_legal_names = std::collections::HashMap::new();
        suggested_legal_names.insert("ma-highway-20".to_string(), Some("Prairie Enterprises LLC".to_string()));

        let result =
            apply_suggested_legal_name(company, "run-highway-20", &merchant_account_run_ids, &suggested_legal_names);

        assert_eq!(result.legal_name.as_deref(), Some("Prairie Enterprises LLC"));
    }

    #[test]
    fn a_run_with_no_correlation_keeps_its_own_intake_legal_name_untouched() {
        // Carpentersville's own Intake run: no correlation resolved (or
        // none exists) -- must NOT be silently blanked or guessed at,
        // just left exactly as Intake mapped it (often None, per this
        // module's own doc comment on the sister-site pattern).
        let company = MappedCompany::default();
        let merchant_account_run_ids = std::collections::HashMap::new();
        let suggested_legal_names = std::collections::HashMap::new();

        let result = apply_suggested_legal_name(
            company,
            "run-carpentersville",
            &merchant_account_run_ids,
            &suggested_legal_names,
        );

        assert_eq!(result.legal_name, None);
    }

    #[test]
    fn a_correlation_whose_merchant_account_fetch_failed_leaves_legal_name_untouched() {
        // merchant_account_run_ids has the correlation, but
        // suggested_legal_names has no entry (or None) for it -- the
        // live fetch/resolution failed. Must degrade to Intake's own
        // value, not silently drop it.
        let company = MappedCompany {
            legal_name: Some("Intake's Own Value".to_string()),
            ..Default::default()
        };
        let mut merchant_account_run_ids = std::collections::HashMap::new();
        merchant_account_run_ids.insert("run-highway-20".to_string(), "ma-highway-20".to_string());
        let mut suggested_legal_names = std::collections::HashMap::new();
        suggested_legal_names.insert("ma-highway-20".to_string(), None);

        let result =
            apply_suggested_legal_name(company, "run-highway-20", &merchant_account_run_ids, &suggested_legal_names);

        assert_eq!(result.legal_name.as_deref(), Some("Intake's Own Value"));
    }

    fn run_request(run_id: &str, merchant_account_run_id: Option<&str>) -> PreviewRunRequest {
        PreviewRunRequest {
            run_id: run_id.to_string(),
            run_name: format!("{run_id} title"),
            merchant_account_run_id: merchant_account_run_id.map(str::to_string),
        }
    }

    #[test]
    fn an_explicit_choice_fills_in_a_run_that_auto_correlation_left_ambiguous() {
        // The real Carpentersville case: auto-correlation found nothing
        // (dropped as ambiguous), but the user picked the correct
        // candidate on the search page's "Potential Duplicates" rows.
        let auto_correlated = std::collections::HashMap::new();
        let runs = vec![run_request("run-carpentersville", Some("ma-carpentersville-1"))];

        let result = apply_explicit_merchant_account_choices(auto_correlated, &runs);

        assert_eq!(
            result.get("run-carpentersville"),
            Some(&"ma-carpentersville-1".to_string())
        );
    }

    #[test]
    fn an_explicit_choice_overrides_an_already_unambiguous_auto_correlation() {
        // Not the real Carpentersville case, but must still hold: the
        // user's own pick is authoritative even when auto-correlation
        // already found something on its own.
        let mut auto_correlated = std::collections::HashMap::new();
        auto_correlated.insert("run-highway-20".to_string(), "ma-auto-guessed".to_string());
        let runs = vec![run_request("run-highway-20", Some("ma-user-picked"))];

        let result = apply_explicit_merchant_account_choices(auto_correlated, &runs);

        assert_eq!(result.get("run-highway-20"), Some(&"ma-user-picked".to_string()));
    }


    #[test]
    fn a_run_with_no_explicit_choice_keeps_its_auto_correlated_value() {
        let mut auto_correlated = std::collections::HashMap::new();
        auto_correlated.insert("run-highway-20".to_string(), "ma-highway-20".to_string());
        let runs = vec![run_request("run-highway-20", None)];

        let result = apply_explicit_merchant_account_choices(auto_correlated, &runs);

        assert_eq!(result.get("run-highway-20"), Some(&"ma-highway-20".to_string()));
    }
}
