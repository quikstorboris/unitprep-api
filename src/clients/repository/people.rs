//! Linking, editing and unlinking the people on a facility.

use crate::clients::people::{ParsedPerson, PersonAssignment};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Finds an existing person by (email, full_name) -- both
/// case-insensitive, email via CITEXT -- or creates one, then links them
/// to the facility with the given role. Idempotent via `ON CONFLICT DO
/// NOTHING` on the (facility_id, person_id, role) primary key, since the
/// same person/role pair can legitimately be re-ingested.
///
/// **Full name is part of identity, not just email** (2026-09-08, real
/// bug: Dubuqueland's Soppe family): email alone isn't unique in real
/// data -- several genuinely distinct family members (Barb Soppe, Carrie
/// Krueger, Chad Soppe) share one family inbox at the same facility with
/// the same role. Matching on email alone would silently collapse them
/// into a single `clients.people` row, each overwriting the last one's
/// name. Matching by name+phone with no email, or fuzzy name matching
/// beyond an exact (case-insensitive) name, is still deliberately NOT
/// attempted here -- that's the same manual-review-worthy fuzzy-match
/// problem [[Dedup Tool Index|the dedup tool]] exists to solve, not
/// something to guess at inline.
pub(super) async fn link_person_to_facility(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    person: &ParsedPerson,
    role: &str,
) -> Result<(), sqlx::Error> {
    let existing: Option<(Uuid,)> = match &person.email {
        Some(email) => {
            sqlx::query_as("SELECT id FROM clients.people WHERE email = $1 AND full_name ILIKE $2")
                .bind(email)
                .bind(&person.full_name)
                .fetch_optional(&mut **tx)
                .await?
        }
        None => None,
    };

    let person_id = match existing {
        Some((id,)) => id,
        None => {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO clients.people (full_name, email, phone) VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(&person.full_name)
            .bind(&person.email)
            .bind(&person.phone)
            .fetch_one(&mut **tx)
            .await?;
            id
        }
    };

    sqlx::query(
        "INSERT INTO clients.facility_people (facility_id, person_id, role)
         VALUES ($1, $2, $3)
         ON CONFLICT (facility_id, person_id, role) DO NOTHING",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(role)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Same find-by-(email,name)-or-create-then-link shape as
/// `link_person_to_facility` above, for `api::clients_facility_people`'s
/// "Add User" chip click (a not-yet-linked candidate) and its "+ Add
/// Person Manually" form -- both add a person who isn't on this
/// facility's roster yet. When a matching `clients.people` row already
/// exists (same email AND same name -- see `link_person_to_facility`'s
/// own doc comment on why name is part of identity, not just email),
/// its `phone` is refreshed to the caller's value; `full_name` is
/// intentionally never touched here, since a match already means the
/// name agrees. Correcting a roster row whose *name itself* drifted
/// (e.g. Sand-Sto's own "Irene Chen - (301) 787-9221") is
/// `heal_person_in_place`'s job below, not this function's -- that path
/// already knows the exact existing row to correct (the roster's own
/// `person_id`), so it never needs this function's identity-resolution
/// step at all, which matters precisely when resolution would otherwise
/// be ambiguous (multiple distinct people sharing one email+role).
pub async fn upsert_person_and_link_to_facility(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    assignment: &PersonAssignment,
    source: &str,
) -> Result<(), sqlx::Error> {
    let existing: Option<(Uuid,)> = match &assignment.email {
        Some(email) => {
            sqlx::query_as("SELECT id FROM clients.people WHERE email = $1 AND full_name ILIKE $2")
                .bind(email)
                .bind(&assignment.full_name)
                .fetch_optional(&mut **tx)
                .await?
        }
        None => None,
    };

    let person_id = match existing {
        Some((id,)) => {
            sqlx::query("UPDATE clients.people SET phone = $1, updated_at = now() WHERE id = $2")
                .bind(&assignment.phone)
                .bind(id)
                .execute(&mut **tx)
                .await?;
            id
        }
        None => {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO clients.people (full_name, email, phone) VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(&assignment.full_name)
            .bind(&assignment.email)
            .bind(&assignment.phone)
            .fetch_one(&mut **tx)
            .await?;
            id
        }
    };

    // ON CONFLICT DO NOTHING deliberately leaves `source` untouched on an
    // already-existing link -- re-clicking a "Add User" chip for someone
    // a manager already protected (flipped to 'manual' while editing)
    // must never silently downgrade them back to 'process_street'.
    sqlx::query(
        "INSERT INTO clients.facility_people (facility_id, person_id, role, source)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (facility_id, person_id, role) DO NOTHING",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(&assignment.role)
    .bind(source)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Corrects a specific, already-known `clients.people` row's
/// `full_name`/`phone` in place, by `person_id` -- used only by
/// `get_facility_people`'s silent self-heal pass, which already knows
/// exactly which roster row it means to correct (its own `person_id`),
/// so it never has to re-resolve identity by email the way
/// `upsert_person_and_link_to_facility` does for a not-yet-linked
/// candidate. That matters precisely because a fresh email(+name)
/// lookup can be genuinely ambiguous -- several distinct real people
/// sharing one email+role (Dubuqueland's Soppe family) -- while an
/// update scoped to an id already known is never ambiguous at all.
pub async fn heal_person_in_place(
    tx: &mut Transaction<'_, Postgres>,
    person_id: Uuid,
    full_name: &str,
    phone: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE clients.people SET full_name = $1, phone = $2, updated_at = now() WHERE id = $3",
    )
    .bind(full_name)
    .bind(phone)
    .bind(person_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Removes one (facility_id, person_id, role) link -- the Users tab's
/// "unlink" action. Only ever deletes the link row, never
/// `clients.people` itself: the same person can legitimately be linked to
/// several of a company's facilities (a shared owner), so deleting their
/// identity row here would silently break those other links too. Mirrors
/// `api::clients_elavon::unlink_facility_elavon`'s own restraint (that
/// one deletes `facility_merchant_accounts` rows, never touches
/// `clients.people`-equivalent identity data either).
pub async fn unlink_person_from_facility(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    person_id: Uuid,
    role: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM clients.facility_people WHERE facility_id = $1 AND person_id = $2 AND role = $3")
        .bind(facility_id)
        .bind(person_id)
        .bind(role)
        .execute(&mut **tx)
        .await?;

    Ok(())
}

/// The Users tab's "Edit" action -- unlike `upsert_person_and_link_to_facility`
/// (an "Add User" chip, or the GET self-heal pass, both always sourced
/// from Process Street data), this is a human directly retyping a
/// person's own name/email/phone/role. Always writes `clients.people`'s
/// shared identity fields; a 'manual' person's link keeps its source
/// unconditionally, but a 'process_street' one only flips to 'manual'
/// when `protect_from_resync` is true -- the Users tab's own "this will
/// keep getting refreshed from Process Street unless you protect it"
/// choice, presented at edit time (2026-09-08) rather than silently
/// losing the edit on the next self-heal pass. Returns the row's
/// `source` *before* this call, so the caller can decide whether that
/// choice needed presenting at all.
// 8 real, independent fields of a human-edited form submission, not
// accidental parameter sprawl -- bundling into a struct would move the
// complexity rather than remove it, for 3 call sites (1 real, 2 test).
#[allow(clippy::too_many_arguments)]
pub async fn edit_person_and_facility_link(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    person_id: Uuid,
    old_role: &str,
    full_name: &str,
    email: Option<&str>,
    phone: Option<&str>,
    new_role: &str,
    protect_from_resync: bool,
) -> Result<Option<String>, sqlx::Error> {
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT source FROM clients.facility_people WHERE facility_id = $1 AND person_id = $2 AND role = $3",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(old_role)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((previous_source,)) = existing else {
        return Ok(None);
    };

    sqlx::query("UPDATE clients.people SET full_name = $1, email = $2, phone = $3, updated_at = now() WHERE id = $4")
        .bind(full_name)
        .bind(email)
        .bind(phone)
        .bind(person_id)
        .execute(&mut **tx)
        .await?;

    let new_source = if previous_source == "manual" || protect_from_resync {
        "manual"
    } else {
        "process_street"
    };

    if new_role == old_role {
        sqlx::query("UPDATE clients.facility_people SET source = $1 WHERE facility_id = $2 AND person_id = $3 AND role = $4")
            .bind(new_source)
            .bind(facility_id)
            .bind(person_id)
            .bind(old_role)
            .execute(&mut **tx)
            .await?;
    } else {
        sqlx::query("DELETE FROM clients.facility_people WHERE facility_id = $1 AND person_id = $2 AND role = $3")
            .bind(facility_id)
            .bind(person_id)
            .bind(old_role)
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "INSERT INTO clients.facility_people (facility_id, person_id, role, source) VALUES ($1, $2, $3, $4)
             ON CONFLICT (facility_id, person_id, role) DO UPDATE SET source = EXCLUDED.source",
        )
        .bind(facility_id)
        .bind(person_id)
        .bind(new_role)
        .bind(new_source)
        .execute(&mut **tx)
        .await?;
    }

    Ok(Some(previous_source))
}
