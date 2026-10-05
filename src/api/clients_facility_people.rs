//! Facility page's Users tab. `GET` returns two things: the facility's
//! actual saved roster (`clients.facility_people`/`clients.people`,
//! written once at ingest and never touched again automatically otherwise)
//! and "Add User" candidates -- rows already sitting in
//! `clients.ps_person_index` for this facility's own `ps_intake_run_id`,
//! kept fresh by the nightly background sync independent of when the
//! facility was created or last touched. No live Process Street call and
//! no search box: the candidates are exactly what PS currently says for
//! this facility's own Intake run, already indexed.
//!
//! `GET` also silently self-heals: any roster person whose stored
//! name/phone disagrees with a fresh candidate gets corrected in place
//! before the response goes out (see `repository::heal_person_in_place`'s
//! own doc comment for the real example, Sand-Sto's "Irene Chen - (301)
//! 787-9221"). No click needed -- viewing the tab is enough. This
//! replaced an earlier design (Boris, 2026-09-04) where a click on an
//! already-linked chip did the refresh; that click now means unlink
//! instead (see `DELETE` below), so the fix had to stop needing a click
//! at all.
//!
//! **Matching is by email+role, but a shared inbox can make that
//! genuinely ambiguous** (2026-09-08, real bug: Dubuqueland's Soppe
//! family -- several distinct people sharing one family email, all
//! "owner"). Self-heal prefers an exact name match among same-email,
//! same-role candidates; only falls back to a bare email+role match when
//! it's unambiguous (exactly one such candidate). Two or more
//! different-named candidates sharing that email+role is left alone
//! rather than guessed at -- see `get_facility_people`'s own candidate-
//! selection code below, and `repository::link_person_to_facility`'s doc
//! comment for why `clients.people` identity itself is now (email, name)
//! rather than email alone.
//!
//! `POST` adds a person -- either an "Add User" chip click for a
//! candidate not yet on the roster (`source: "process_street"`), or a
//! brand-new person typed in by hand (`source: "manual"`). A 'manual'
//! link is permanently excluded from the self-heal pass above -- see
//! `clients.facility_people.source`'s own migration comment -- the
//! "add a user that will never be overwritten by re-sync" Boris asked
//! for, 2026-09-08.
//!
//! `PUT .../people/{person_id}` is the Users tab's "Edit" action --
//! retyping a roster person's own name/email/phone/role directly. A
//! 'process_street' person's edit carries `protect_from_resync`: when
//! true, their link flips to 'manual' (so this edit itself survives the
//! next self-heal); when false, the edit is saved but the next self-heal
//! pass can still silently revert it back to whatever the index says --
//! deliberately risky, the same choice/warning Boris asked the Users tab
//! present at edit time rather than losing the edit invisibly later. A
//! 'manual' person's edit never asks -- it's already permanently exempt.
//!
//! `DELETE .../people/{person_id}?role=...` unlinks one roster entry --
//! the same chip, now rendered red for an already-linked candidate,
//! rather than a separate control.

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{bad_request, internal_error, not_found, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use crate::clients::legal_owner::{
    legal_owner_flags, unmatched_owners, OwnerIdentity, RosterIdentity,
};
use crate::clients::people::PersonAssignment;
use crate::clients::repository::{
    edit_person_and_facility_link, heal_person_in_place, unlink_person_from_facility,
    upsert_person_and_link_to_facility,
};

const SOURCES: &[&str] = &["process_street", "manual"];

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

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

#[derive(sqlx::FromRow)]
struct FacilityIdentity {
    ps_intake_run_id: Option<String>,
}

/// A facility's own Merchant Account owners with a name -- the only
/// ones that can say who owns anything.
async fn merchant_account_owners(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    facility_id: Uuid,
) -> Result<Vec<OwnerIdentity>, sqlx::Error> {
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT display_name, email, phone
           FROM clients.facility_merchant_account_parties
          WHERE facility_id = $1 AND party_role = 'owner'",
    )
    .bind(facility_id)
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(display_name, email, phone)| OwnerIdentity {
            display_name,
            email,
            phone,
        })
        .collect())
}

fn has_a_named_owner(owners: &[OwnerIdentity]) -> bool {
    owners.iter().any(|owner| {
        owner
            .display_name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty())
    })
}

/// Owners from a **sister facility's** Merchant Account form, for a
/// facility that has no owners of its own yet. Boris, 2026-10-02: every
/// facility will eventually need its own Merchant form, but until then
/// "we can relatively safely pick any that has information about owners
/// (i.e. merch form filled out)" -- the same company's facilities share
/// the same legal owners far more often than not (Affordable Storage's
/// Beau and Brad Ryan own all nine). Picks the sister with the most
/// named owners, then the most recently synced, then by name, so the
/// choice is stable between page loads.
async fn sister_facility_owners(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    company_id: Uuid,
    facility_id: Uuid,
) -> Result<Option<(LegalOwnerSource, Vec<OwnerIdentity>)>, sqlx::Error> {
    let source: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT f.id, f.name
           FROM clients.facilities f
           JOIN clients.facility_merchant_account_parties p ON p.facility_id = f.id
          WHERE f.company_id = $1 AND f.id <> $2
            AND p.party_role = 'owner'
            AND btrim(coalesce(p.display_name, '')) <> ''
          GROUP BY f.id, f.name
          ORDER BY count(*) DESC, max(p.last_synced_at) DESC NULLS LAST, f.name
          LIMIT 1",
    )
    .bind(company_id)
    .bind(facility_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some((source_id, source_name)) = source else {
        return Ok(None);
    };

    let owners = merchant_account_owners(tx, source_id).await?;
    Ok(Some((
        LegalOwnerSource {
            facility_id: source_id,
            facility_name: source_name,
        },
        owners,
    )))
}

/// Any authenticated caller -- same reasoning as `clients_elavon`'s own
/// GET: RLS's own SELECT policies (authenticated-only, no role check) are
/// the real backstop, matching every other read-only facility tab.
pub async fn get_facility_people(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for facility people");
            return internal_error("Could not load this facility's Users tab");
        }
    };

    let facility: Option<FacilityIdentity> = match sqlx::query_as(
        "SELECT ps_intake_run_id FROM clients.facilities WHERE id = $1 AND company_id = $2",
    )
    .bind(facility_id)
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for people tab failed");
            return internal_error("Could not load this facility's Users tab");
        }
    };
    let Some(facility) = facility else {
        let _ = tx.commit().await;
        return not_found("not_found", "No such facility.".to_string());
    };

    let roster: Vec<FacilityPerson> = match sqlx::query_as(
        "SELECT p.id AS person_id, p.full_name, p.email::text AS email, p.phone, fp.role, fp.source
           FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1
          ORDER BY p.full_name",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility_people roster lookup failed");
            return internal_error("Could not load this facility's Users tab");
        }
    };

    // Same `workflow = 'intake'` scoping `clients_search`'s own
    // facility-person lookup uses -- a Merchant Account/Contract Order
    // person isn't part of this facility's own owner/DM/manager roster.
    let candidates: Vec<PersonAssignment> = match &facility.ps_intake_run_id {
        None => Vec::new(),
        Some(run_id) => match sqlx::query_as(
            "SELECT full_name, email, phone, role
               FROM clients.ps_person_index
              WHERE workflow = 'intake' AND ps_run_id = $1
              ORDER BY full_name",
        )
        .bind(run_id)
        .fetch_all(&mut *tx)
        .await
        {
            Ok(rows) => rows,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "ps_person_index candidate lookup failed");
                return internal_error("Could not load this facility's Users tab");
            }
        },
    };

    // Self-heal: a roster row whose stored name/phone disagrees with a
    // fresh candidate gets corrected in place before the transaction
    // commits -- see this module's own doc comment. Matched
    // case-insensitively on email since `clients.people.email` is CITEXT
    // but `ps_person_index.email` is plain TEXT.
    //
    // Prefers an exact name match among same-email, same-role candidates
    // -- safe even when a shared family inbox means several distinct
    // people share the same email and role (real Dubuqueland/Soppe
    // data, 2026-09-08). Only falls back to a bare email+role match when
    // it's unambiguous (exactly one such candidate) -- that's the
    // genuine "PS corrected this person's own name" case (Sand-Sto's own
    // "Irene Chen"). Two or more different-named candidates sharing that
    // email+role is left alone rather than guessed at -- corrects
    // `heal_person_in_place`'s own known `person_id` directly, so unlike
    // `upsert_person_and_link_to_facility` there's no risk of colliding
    // this row with a *different* real person who happens to share the
    // same email.
    let mut roster = roster;
    for person in &mut roster {
        // A 'manual' person -- whether added by hand from scratch, or a
        // Process Street person a manager chose to protect while
        // editing -- is permanently exempt from this pass, same
        // reasoning as a QSX-exempt policy category.
        if person.source != "process_street" {
            continue;
        }
        let Some(email) = person.email.as_deref() else {
            continue;
        };

        let same_email_role: Vec<&PersonAssignment> = candidates
            .iter()
            .filter(|c| {
                c.role == person.role
                    && c.email
                        .as_deref()
                        .is_some_and(|e| e.eq_ignore_ascii_case(email))
            })
            .collect();

        let candidate = same_email_role
            .iter()
            .find(|c| c.full_name.eq_ignore_ascii_case(&person.full_name))
            .copied()
            .or_else(|| (same_email_role.len() == 1).then(|| same_email_role[0]));

        let Some(candidate) = candidate else { continue };

        if candidate.full_name == person.full_name && candidate.phone == person.phone {
            continue;
        }

        if let Err(err) = heal_person_in_place(
            &mut tx,
            person.person_id,
            &candidate.full_name,
            candidate.phone.as_deref(),
        )
        .await
        {
            tracing::error!(
                error = %err,
                user_id = %user.user_id,
                person_id = %person.person_id,
                "failed to self-heal a stale facility person"
            );
            continue;
        }

        person.full_name = candidate.full_name.clone();
        person.phone = candidate.phone.clone();
    }

    // "Legal Owner" column: owners listed on the Merchant Account
    // Pre-App, matched against the (now self-healed) roster. The party
    // table's RLS SELECT policy is narrower than this tab's, so a viewer
    // without that access just sees no checkmarks rather than an error.
    //
    // A facility with no owners on a form of its own borrows a sister
    // facility's (see `sister_facility_owners`), and says so.
    let mut owners = match merchant_account_owners(&mut tx, facility_id).await {
        Ok(owners) => owners,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "merchant account owner lookup failed");
            return internal_error("Could not load this facility's Users tab");
        }
    };
    let mut legal_owner_source = None;
    if !has_a_named_owner(&owners) {
        match sister_facility_owners(&mut tx, company_id, facility_id).await {
            Ok(Some((source, sister_owners))) => {
                owners = sister_owners;
                legal_owner_source = Some(source);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "sister facility owner lookup failed");
                return internal_error("Could not load this facility's Users tab");
            }
        }
    }
    {
        let roster_identities: Vec<RosterIdentity> = roster
            .iter()
            .map(|p| RosterIdentity {
                full_name: &p.full_name,
                email: p.email.as_deref(),
            })
            .collect();
        let flags = legal_owner_flags(&roster_identities, &owners);
        for (person, is_legal_owner) in roster.iter_mut().zip(flags) {
            person.legal_owner = is_legal_owner;
        }
    }

    // Owners the (now-flagged) roster above has no row for AND that
    // don't already have an unadded Intake "Add" chip in `candidates` --
    // see `clients::legal_owner`'s own doc comment for why both are
    // checked (a real legal owner can otherwise vanish entirely,
    // Freeland's Serene Armstrong, 2026-09-30). Rebuilt from `roster`
    // fresh, after the mutable borrow above has already ended.
    let known_identities: Vec<RosterIdentity> = roster
        .iter()
        .map(|p| RosterIdentity {
            full_name: &p.full_name,
            email: p.email.as_deref(),
        })
        .chain(candidates.iter().map(|c| RosterIdentity {
            full_name: &c.full_name,
            email: c.email.as_deref(),
        }))
        .collect();
    let missing_legal_owners: Vec<MissingLegalOwner> = unmatched_owners(&known_identities, &owners)
        .into_iter()
        .map(|owner| MissingLegalOwner {
            full_name: owner
                .display_name
                .clone()
                .expect("unmatched_owners only returns owners with a non-blank display_name"),
            email: owner.email.clone(),
            phone: owner.phone.clone(),
        })
        .collect();

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit facility people transaction");
        return internal_error("Could not load this facility's Users tab");
    }

    Json(FacilityPeopleResponse {
        roster,
        candidates,
        missing_legal_owners,
        legal_owner_source,
    })
    .into_response()
}

/// Requires no special permission beyond authentication -- same as every
/// other `clients.*` write, gated by RLS itself
/// (`onboarding_manager`/`department_manager` only, enforced at the
/// database level by the INSERT/UPDATE policies those tables already
/// carry), matching `clients_create`'s own reasoning rather than
/// `clients_elavon`'s `client_ops.perform` gate (that permission is
/// specific to actions this domain considers "performing a client
/// operation"; linking a person to a facility's own roster is closer to
/// the create-time confirmation screen's own People chips, which carry
/// no separate permission check of their own either).
#[derive(Debug, Deserialize)]
pub struct AddPersonRequest {
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
    /// "process_street" for an "Add User" chip click, "manual" for a
    /// brand-new person typed in by hand and never touched by a future
    /// policy-style sync.
    pub source: String,
}

pub async fn add_facility_person(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<AddPersonRequest>,
) -> Response {
    let user_agent = request_context(&headers);

    if request.full_name.trim().is_empty() {
        return bad_request(
            "invalid_request",
            "full_name is required and must not be blank.".to_string(),
        );
    }
    if !SOURCES.contains(&request.source.as_str()) {
        return bad_request(
            "invalid_request",
            format!("\"{}\" is not a recognized source.", request.source),
        );
    }

    let assignment = PersonAssignment {
        full_name: request.full_name,
        email: request.email,
        phone: request.phone,
        role: request.role,
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for add facility person");
            return internal_error("Could not add this person");
        }
    };

    let facility_exists: Option<(Uuid,)> = match sqlx::query_as(
        "SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2",
    )
    .bind(facility_id)
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility existence check for add person failed");
            return internal_error("Could not add this person");
        }
    };
    if facility_exists.is_none() {
        let _ = tx.rollback().await;
        return not_found("not_found", "No such facility.".to_string());
    }

    if let Err(err) =
        upsert_person_and_link_to_facility(&mut tx, facility_id, &assignment, &request.source).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, facility_id = %facility_id, "failed to upsert facility person");
        return internal_error("Could not add this person");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit add facility person transaction");
        return internal_error("Could not add this person");
    }

    // After the commit, not before -- see fees.rs's update_fees for why.
    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_PERSON_ADDED,
        user.user_id,
        "facility_person",
        Some(&facility_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!(null),
            serde_json::json!({
                "full_name": assignment.full_name,
                "email": assignment.email,
                "phone": assignment.phone,
                "role": assignment.role,
                "source": request.source,
            }),
        ),
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct EditPersonRequest {
    pub old_role: String,
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
    /// Only meaningful when this person's current link is
    /// 'process_street' -- see `repository::edit_person_and_facility_link`'s
    /// own doc comment. Ignored (already permanently protected) for an
    /// already-'manual' person.
    pub protect_from_resync: bool,
}

/// Same no-extra-permission reasoning as `add_facility_person` above.
/// The frontend already knows this person's current `source` (from the
/// roster `GET`), so it can decide whether to show the "protect from
/// resync" choice before ever submitting this request -- this handler
/// just carries out whatever was decided.
pub async fn edit_facility_person(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id, person_id)): Path<(Uuid, Uuid, Uuid)>,
    Json(request): Json<EditPersonRequest>,
) -> Response {
    let user_agent = request_context(&headers);

    if request.full_name.trim().is_empty() {
        return bad_request(
            "invalid_request",
            "full_name is required and must not be blank.".to_string(),
        );
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for edit facility person");
            return internal_error("Could not save this person");
        }
    };

    let facility_exists: Option<(Uuid,)> = match sqlx::query_as(
        "SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2",
    )
    .bind(facility_id)
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility existence check for edit person failed");
            return internal_error("Could not save this person");
        }
    };
    if facility_exists.is_none() {
        let _ = tx.rollback().await;
        return not_found("not_found", "No such facility.".to_string());
    }

    let previous: Option<(String, Option<String>, Option<String>)> = match sqlx::query_as(
        "SELECT p.full_name, p.email::text, p.phone
           FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1 AND fp.person_id = $2 AND fp.role = $3",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(&request.old_role)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, person_id = %person_id, "failed to read prior facility person state");
            return internal_error("Could not save this person");
        }
    };

    let result = edit_person_and_facility_link(
        &mut tx,
        facility_id,
        person_id,
        &request.old_role,
        &request.full_name,
        request.email.as_deref(),
        request.phone.as_deref(),
        &request.role,
        request.protect_from_resync,
    )
    .await;

    match result {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = tx.rollback().await;
            return not_found(
                "not_found",
                "No such person on this facility's roster.".to_string(),
            );
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, person_id = %person_id, "failed to edit facility person");
            return internal_error("Could not save this person");
        }
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit edit facility person transaction");
        return internal_error("Could not save this person");
    }

    // After the commit, not before -- see fees.rs's update_fees for why.
    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_PERSON_UPDATED,
        user.user_id,
        "facility_person",
        Some(&person_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!(previous.map(|(full_name, email, phone)| {
                serde_json::json!({ "full_name": full_name, "email": email, "phone": phone, "role": request.old_role })
            })),
            serde_json::json!({
                "full_name": request.full_name,
                "email": request.email,
                "phone": request.phone,
                "role": request.role,
            }),
        ),
        user_agent,
        None,
        serde_json::json!({ "facility_id": facility_id }),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct UnlinkFacilityPersonQuery {
    pub role: String,
}

/// Same no-extra-permission reasoning as `add_facility_person` above --
/// removing one link row is the same "this facility's own roster"
/// concern as adding one, not a `client_ops.perform`-gated action. No
/// live PS call, same restraint as `clients_elavon::unlink_facility_elavon`:
/// this only ever deletes `clients.facility_people`'s own link row (see
/// `repository::unlink_person_from_facility`'s own doc comment on why
/// `clients.people` itself is never touched).
pub async fn unlink_facility_person(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id, person_id)): Path<(Uuid, Uuid, Uuid)>,
    axum::extract::Query(query): axum::extract::Query<UnlinkFacilityPersonQuery>,
) -> Response {
    let user_agent = request_context(&headers);

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for unlink facility person");
            return internal_error("Could not remove this person");
        }
    };

    let facility_exists: Option<(Uuid,)> = match sqlx::query_as(
        "SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2",
    )
    .bind(facility_id)
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility existence check for unlink person failed");
            return internal_error("Could not remove this person");
        }
    };
    if facility_exists.is_none() {
        let _ = tx.rollback().await;
        return not_found("not_found", "No such facility.".to_string());
    }

    let removed: Option<(String, Option<String>, Option<String>)> = match sqlx::query_as(
        "SELECT p.full_name, p.email::text, p.phone
           FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1 AND fp.person_id = $2 AND fp.role = $3",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(&query.role)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, person_id = %person_id, "failed to read facility person state before unlink");
            return internal_error("Could not remove this person");
        }
    };

    if let Err(err) =
        unlink_person_from_facility(&mut tx, facility_id, person_id, &query.role).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, facility_id = %facility_id, person_id = %person_id, "failed to unlink facility person");
        return internal_error("Could not remove this person");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit unlink facility person transaction");
        return internal_error("Could not remove this person");
    }

    // After the commit, not before -- see fees.rs's update_fees for why.
    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_PERSON_UNLINKED,
        user.user_id,
        "facility_person",
        Some(&person_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!(removed.map(|(full_name, email, phone)| {
                serde_json::json!({ "full_name": full_name, "email": email, "phone": phone, "role": query.role })
            })),
            serde_json::json!(null),
        ),
        user_agent,
        None,
        serde_json::json!({ "facility_id": facility_id }),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn get_facility_people_reaches_the_database() {
        let response = get_facility_people(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn add_facility_person_rejects_a_blank_full_name_without_touching_the_database() {
        let response = add_facility_person(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(AddPersonRequest {
                full_name: "   ".to_string(),
                email: Some("someone@example.com".to_string()),
                phone: None,
                role: "owner".to_string(),
                source: "process_street".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_facility_person_rejects_an_unrecognized_source_without_touching_the_database() {
        let response = add_facility_person(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(AddPersonRequest {
                full_name: "Irene Chen".to_string(),
                email: Some("irene@chenlawgroup.com".to_string()),
                phone: None,
                role: "owner".to_string(),
                source: "not_a_real_source".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_facility_person_reaches_the_database() {
        let response = add_facility_person(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(AddPersonRequest {
                full_name: "Irene Chen".to_string(),
                email: Some("irene@chenlawgroup.com".to_string()),
                phone: Some("(301) 787-9221".to_string()),
                role: "owner".to_string(),
                source: "manual".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn edit_facility_person_rejects_a_blank_full_name_without_touching_the_database() {
        let response = edit_facility_person(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
            Json(EditPersonRequest {
                old_role: "owner".to_string(),
                full_name: "   ".to_string(),
                email: None,
                phone: None,
                role: "owner".to_string(),
                protect_from_resync: false,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn edit_facility_person_reaches_the_database() {
        let response = edit_facility_person(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
            Json(EditPersonRequest {
                old_role: "owner".to_string(),
                full_name: "Irene Chen".to_string(),
                email: Some("irene@chenlawgroup.com".to_string()),
                phone: None,
                role: "owner".to_string(),
                protect_from_resync: true,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn unlink_facility_person_reaches_the_database() {
        let response = unlink_facility_person(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
            axum::extract::Query(UnlinkFacilityPersonQuery {
                role: "owner".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
