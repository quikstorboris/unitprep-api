//! Resolves a raw Process Street staff identifier (an email for "Who is
//! the conductor", a name for "Fill in Rep Only fields") to a real
//! `auth.users.id` -- the one shared piece of matching logic behind
//! Implementation Manager/Sales Rep assignment, meant to be called both
//! at company-creation time (`src/clients/create.rs`) and by the
//! one-time backfill binary. Both of those callers are a follow-up once
//! the exact PS field shape for the conductor/rep steps is confirmed
//! (see the plan's own "PS field mapping" item) -- this module exists on
//! its own now so the resolution rule itself can be built, reviewed, and
//! tested independently of that mapping work.
//!
//! Precedence: `clients.staff_identity_alias` first (the one sanctioned
//! way to redirect a raw identifier that no longer maps correctly -- a
//! former employee, a typo PS keeps repeating), then an exact
//! (case-insensitive) `auth.users.email` match -- the shape "Who is the
//! conductor" is expected to answer with -- then a case-insensitive
//! `first_name || ' ' || last_name` match -- the shape "Fill in Rep Only
//! fields" is expected to answer with, a plain name, no email. `None` is
//! a legitimate, expected outcome, not an error: per the approved plan,
//! sales rep matching will mostly resolve to nothing until Dan/Shaina/
//! Sarah/Randy have real `auth.users` rows.
//!
//! Reads through `auth.staff_directory()` (see migration
//! `20260911130000_create_staff_identity_alias`), a narrow
//! `SECURITY DEFINER` function, rather than a plain `SELECT * FROM
//! auth.users` -- that table's own RLS (`users_select_own_or_admin`)
//! only lets a caller see their own row, or every row if admin, which
//! would silently break this matcher for the two real non-admin callers
//! that need it (an onboarding_manager/department_manager creating a
//! client via `client_ops.perform`, and the backfill job).

// No caller yet -- create.rs and the backfill binary are both a
// follow-up (see this module's own doc comment above). Remove once a
// real caller exists, same convention as intake_mapping.rs's own
// "Phase 1 only" header.
#![allow(dead_code)]

use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::begin_rls_transaction;

#[derive(Debug, Clone, sqlx::FromRow)]
struct AliasRow {
    raw_identifier: String,
    resolved_user_id: Uuid,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct StaffRow {
    id: Uuid,
    email: String,
    first_name: String,
    last_name: String,
}

/// The actual matching rule, over already-fetched candidate data --
/// split out from `resolve_staff_identifier`'s DB fetch so the
/// precedence itself is testable with plain fixtures, the same split
/// `clients_search::derive_facilities_from_person_matches` uses for its
/// own pure matching logic elsewhere in this codebase.
fn resolve_from_candidates(
    raw_identifier: &str,
    aliases: &[AliasRow],
    staff: &[StaffRow],
) -> Option<Uuid> {
    let raw = raw_identifier.trim();
    if raw.is_empty() {
        return None;
    }

    if let Some(alias) = aliases.iter().find(|a| a.raw_identifier == raw) {
        return Some(alias.resolved_user_id);
    }

    if let Some(user) = staff.iter().find(|u| u.email.eq_ignore_ascii_case(raw)) {
        return Some(user.id);
    }

    staff
        .iter()
        .find(|u| format!("{} {}", u.first_name, u.last_name).eq_ignore_ascii_case(raw))
        .map(|u| u.id)
}

/// DB-backed entry point: fetches every alias row and every non-deleted
/// user (both small tables -- internal staff only, never customers),
/// then applies `resolve_from_candidates`. Goes through
/// `begin_rls_transaction` like every other read in this codebase, even
/// though `auth.staff_directory()` itself only requires an authenticated
/// GUC (not a particular role) -- `clients.staff_identity_alias`'s own
/// SELECT policy needs the same `app.current_user_id` setting.
pub async fn resolve_staff_identifier(
    db: &PgPool,
    caller_user_id: Uuid,
    caller_role_keys: &[String],
    raw_identifier: &str,
) -> anyhow::Result<Option<Uuid>> {
    let raw = raw_identifier.trim();
    if raw.is_empty() {
        return Ok(None);
    }

    let mut tx = begin_rls_transaction(db, caller_user_id, caller_role_keys).await?;

    let aliases: Vec<AliasRow> =
        sqlx::query_as("SELECT raw_identifier, resolved_user_id FROM clients.staff_identity_alias")
            .fetch_all(&mut *tx)
            .await?;

    let staff: Vec<StaffRow> =
        sqlx::query_as("SELECT id, email, first_name, last_name FROM auth.staff_directory()")
            .fetch_all(&mut *tx)
            .await?;

    tx.commit().await?;

    Ok(resolve_from_candidates(raw, &aliases, &staff))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(raw_identifier: &str, resolved_user_id: Uuid) -> AliasRow {
        AliasRow {
            raw_identifier: raw_identifier.to_string(),
            resolved_user_id,
        }
    }

    fn staff(id: Uuid, email: &str, first_name: &str, last_name: &str) -> StaffRow {
        StaffRow {
            id,
            email: email.to_string(),
            first_name: first_name.to_string(),
            last_name: last_name.to_string(),
        }
    }

    #[test]
    fn blank_identifier_never_matches() {
        let boris = Uuid::new_v4();
        let aliases = vec![alias("imasse@quikstor.com", boris)];
        let staff_rows = vec![staff(boris, "bmaksimov@quikstor.com", "Boris", "Maksimov")];

        assert_eq!(resolve_from_candidates("   ", &aliases, &staff_rows), None);
    }

    #[test]
    fn an_alias_is_checked_before_a_direct_email_match_and_wins() {
        // The real Ian Masse -> Boris case: imasse@quikstor.com is not
        // itself anyone's email in auth.users, only an alias.
        let boris = Uuid::new_v4();
        let aliases = vec![alias("imasse@quikstor.com", boris)];
        let staff_rows = vec![staff(boris, "bmaksimov@quikstor.com", "Boris", "Maksimov")];

        assert_eq!(
            resolve_from_candidates("imasse@quikstor.com", &aliases, &staff_rows),
            Some(boris)
        );
    }

    #[test]
    fn an_alias_wins_even_when_a_coincidental_direct_match_would_also_exist() {
        let alias_target = Uuid::new_v4();
        let direct_match = Uuid::new_v4();
        let aliases = vec![alias("shared@quikstor.com", alias_target)];
        let staff_rows = vec![staff(
            direct_match,
            "shared@quikstor.com",
            "Someone",
            "Else",
        )];

        assert_eq!(
            resolve_from_candidates("shared@quikstor.com", &aliases, &staff_rows),
            Some(alias_target),
            "the alias table is consulted first and wins over a coincidental direct email match"
        );
    }

    #[test]
    fn email_match_is_case_insensitive() {
        let boris = Uuid::new_v4();
        let staff_rows = vec![staff(boris, "bmaksimov@quikstor.com", "Boris", "Maksimov")];

        assert_eq!(
            resolve_from_candidates("BMaksimov@QuikStor.com", &[], &staff_rows),
            Some(boris)
        );
    }

    #[test]
    fn falls_back_to_a_case_insensitive_full_name_match() {
        let sarah = Uuid::new_v4();
        let staff_rows = vec![staff(sarah, "smcdougal@quikstor.com", "Sarah", "McDougal")];

        assert_eq!(
            resolve_from_candidates("sarah mcdougal", &[], &staff_rows),
            Some(sarah)
        );
    }

    #[test]
    fn full_name_match_requires_both_first_and_last_name_to_agree() {
        let staff_rows = vec![staff(
            Uuid::new_v4(),
            "bmaksimov@quikstor.com",
            "Boris",
            "Maksimov",
        )];

        assert_eq!(
            resolve_from_candidates("Boris Nobody", &[], &staff_rows),
            None
        );
    }

    #[test]
    fn no_match_anywhere_returns_none() {
        // The plan's own expected case: a named rep with no real
        // auth.users row yet -- not a bug.
        let staff_rows = vec![staff(
            Uuid::new_v4(),
            "bmaksimov@quikstor.com",
            "Boris",
            "Maksimov",
        )];

        assert_eq!(
            resolve_from_candidates("Randy Fountain", &[], &staff_rows),
            None
        );
    }
}
