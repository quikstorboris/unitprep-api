//! Response shapes for a facility's people (the Users tab).

use crate::clients::people::PersonAssignment;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FacilityPerson {
    pub person_id: Uuid,
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// Access level (owner / district_manager / manager) -- what this
    /// person can do inside QMS, from the Intake form's user-level
    /// fields. NOT legal ownership; see `legal_owner` below.
    pub role: String,
    pub source: String,
    /// True when this person is also listed as an owner on the Merchant
    /// Account Pre-App -- see `clients::legal_owner`. Computed on read,
    /// never stored (hence `skip`: it isn't a column in the roster query).
    #[sqlx(skip)]
    pub legal_owner: bool,
}

#[derive(Debug, Serialize)]
pub struct FacilityPeopleResponse {
    pub roster: Vec<FacilityPerson>,
    pub candidates: Vec<PersonAssignment>,
    /// Merchant Account Pre-App owners with no roster row and no
    /// `candidates` entry either -- see `clients::legal_owner::
    /// unmatched_owners`. Never duplicates a person already reachable
    /// through the roster or an existing candidate chip.
    pub missing_legal_owners: Vec<MissingLegalOwner>,
    /// Set when this facility has no Merchant Account owners of its own
    /// and the Legal Owner checkmarks (and `missing_legal_owners`) were
    /// worked out from a *sister facility's* form instead -- see
    /// `sister_facility_owners`. `None` means they came from this
    /// facility's own form, or there are none at all.
    pub legal_owner_source: Option<LegalOwnerSource>,
}

/// The sister facility whose Merchant Account form supplied the owners
/// when this facility has none of its own.
#[derive(Debug, Serialize)]
pub struct LegalOwnerSource {
    pub facility_id: Uuid,
    pub facility_name: String,
}

/// One Pre-App owner the Users tab has no other way to surface --
/// `role` is deliberately absent (unlike `PersonAssignment`): a
/// Merchant Account owner has no QMS access level of their own, so the
/// frontend defaults one (`"owner"`) only when actually adding them.
#[derive(Debug, Serialize)]
pub struct MissingLegalOwner {
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
}
