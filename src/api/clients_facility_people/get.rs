//! `GET .../people` -- the Users tab: linked people, missing legal owners and add-person candidates.

use super::dto::{FacilityPeopleResponse, FacilityPerson, MissingLegalOwner};
use super::owners::{
    has_a_named_owner, merchant_account_owners, sister_facility_owners, FacilityIdentity,
};
use crate::api::{internal_error, not_found, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::legal_owner::{legal_owner_flags, unmatched_owners, RosterIdentity};
use crate::clients::people::PersonAssignment;
use crate::clients::repository::heal_person_in_place;
use axum::extract::{Json, Path, State};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

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
