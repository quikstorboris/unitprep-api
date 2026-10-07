//! Request and response shapes for client search.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct SearchClientsQuery {
    pub q: String,
}

/// Why a run showed up in `facility_matches` -- a literal PS title hit
/// carries a real `status`; a run pulled in only because a person on it
/// matched the query has no status available without an extra live PS
/// call per candidate, so it's `None` rather than guessed at.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatchedVia {
    Name,
    Person { full_name: String, role: String },
}

/// Present only when this facility's Merchant Account correlation was
/// genuinely ambiguous (2+ distinct candidate runs, e.g. Carpentersville's
/// real duplicate submission) -- one `FacilityMatch` per candidate, all
/// sharing the same `run_id`/`run_name` (they're the same real
/// facility), each identified by which Merchant Account run it came
/// from. The frontend brackets rows sharing a `run_id` and shows this
/// as "Potential Duplicates" rather than silently picking one.
#[derive(Debug, Serialize)]
pub struct DuplicateCandidate {
    pub merchant_account_run_id: String,
    /// PS's own `audit.updatedDate` for *this* Merchant Account run --
    /// deliberately separate from `FacilityMatch::last_activity_at`
    /// (the shared facility's own Intake activity, identical across
    /// every duplicate row) since this is the value that actually
    /// differs between candidates and helps a user tell which one is
    /// the stale duplicate.
    pub merchant_account_updated_at: DateTime<Utc>,
    /// This candidate's own masked EIN and business address, when
    /// answered -- the real disambiguating signals found after the
    /// Knapp's Self Stor of Milton Freewater / "Milton Self Storage"
    /// mix-up (2026-09-23), shown so a manager picking between
    /// candidates has more to go on than which title sounds closer.
    /// `None` for both on a run that (like "Milton Self Storage") never
    /// got past its own first form section.
    pub ein_last_4: Option<String>,
    pub business_address: Option<String>,
    /// Whether every candidate in this same group that answered a
    /// business address agrees with every other one that did (fuzzy --
    /// see `merchant_account_correlation::addresses_fuzzy_match`).
    /// `None` when fewer than two candidates have an address to compare
    /// at all. Identical across every row in the same group -- a
    /// group-level fact, not a per-candidate one, repeated here since
    /// each candidate is already its own `FacilityMatch` row.
    pub addresses_agree: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct FacilityMatch {
    pub run_id: String,
    pub run_name: String,
    pub status: Option<String>,
    /// Whether `clients.facilities` already has a row for this Intake
    /// run -- lets the UI grey this match out rather than inviting a
    /// duplicate "Add".
    pub already_imported: bool,
    pub matched_via: MatchedVia,
    /// The company this facility belongs to, per the same
    /// Merchant-Account-Legal-Name-first rule `resolve_company_name`
    /// already applies at import time -- `None` when no Merchant
    /// Account run could be confidently correlated to this facility's
    /// own Intake run (not every client uses Elavon).
    pub company_name: Option<String>,
    /// PS's own `audit.updatedDate` for this facility's own Intake run
    /// -- live for a literal title match, last-synced for a
    /// person-derived one (see this module's own `search_clients`
    /// body). Helps a user judge how current/relevant a result is.
    pub last_activity_at: Option<DateTime<Utc>>,
    pub duplicate: Option<DuplicateCandidate>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PersonMatch {
    /// `intake` | `merchant_account` | `contract_order`.
    pub workflow: String,
    pub ps_run_id: String,
    pub run_name: String,
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
}

/// One New Merchant Account run whose own title matched the query --
/// see this module's own doc comment for why this exists as its own
/// list rather than being folded into `facility_matches`.
#[derive(Debug, Serialize)]
pub struct MerchantAccountMatch {
    pub run_id: String,
    pub run_name: String,
    pub status: String,
    pub updated_at: DateTime<Utc>,
    /// Whether some real facility already has this run attached via
    /// `clients.facility_merchant_accounts.ps_new_merchant_run_id` --
    /// the Merchant Account analog of `FacilityMatch::already_imported`.
    pub already_linked: bool,
    /// Same disambiguation fields as `DuplicateCandidate` -- see its own
    /// doc comment. Fetched fresh for every standalone match (this list
    /// has no other live fetch to piggyback on the way correlated
    /// candidates do), so only as many as this query actually returned.
    pub ein_last_4: Option<String>,
    pub business_address: Option<String>,
    /// Titles among this search's own `facility_matches` that share a
    /// significant word with this run's own title but aren't a
    /// straightforward substring match either way (see
    /// `merchant_account_correlation::shares_a_significant_word`) --
    /// the real Knapp's Self Stor of Milton Freewater / "Milton Self
    /// Storage" shape: similar-sounding, textually unrelated by the
    /// stricter check, and (confirmed) two different real businesses.
    /// Empty when nothing in this same search shares any vocabulary
    /// with this run's own title -- not `None`, since "checked, found
    /// nothing" and "not checked" both look identical to the frontend
    /// either way and there's no third state worth modeling.
    pub similar_facility_names: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchClientsResponse {
    pub facility_matches: Vec<FacilityMatch>,
    pub merchant_account_matches: Vec<MerchantAccountMatch>,
    pub person_matches: Vec<PersonMatch>,
}
