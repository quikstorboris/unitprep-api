//! Which Users-tab roster people are a facility's *legal owners*.
//!
//! The Users tab's "Access Level" (owner / district manager / manager)
//! comes from the Intake form's "Owner/District Manager/Manager Level
//! Users" fields, which decide what a person can do inside QMS -- NOT
//! who owns the business. A facility's actual owners are the ones listed
//! explicitly on the Merchant Account Pre-App (`Owner_1..4`), stored in
//! `clients.facility_merchant_account_parties` as `party_role = 'owner'`.
//! The two lists are entered separately and can disagree: LG Squared's
//! Pre-App owner "Laura Cathryn Grace" is "Cathy Grace" on the Intake
//! form (same email, different name), and most Intake users are not
//! legal owners at all.
//!
//! Matching is deliberately conservative -- a wrong checkmark on a legal
//! ownership column is worse than a missing one:
//! 1. Email, case-insensitive. One roster person with that email: match.
//!    Several (a shared family inbox -- real Dubuqueland/Soppe data):
//!    only those whose name also matches the owner's, else none.
//! 2. If the owner has no email, or no roster person shares it, an exact
//!    (case/whitespace-insensitive) name match -- but only when exactly
//!    one roster row has that name, so a name two people share is never
//!    guessed at.
//!
//! `unmatched_owners` answers the complementary question: a Pre-App
//! owner who isn't a name/email match for anyone *already known* for
//! this facility -- not just the saved roster, but also its still-
//! unadded Intake candidates (`clients.ps_person_index`) -- so an owner
//! who really is the same person under a name already pending an "Add"
//! chip (e.g. Freeland's two PS forms both naming the same person) is
//! never reported as missing and never produces a second, duplicate way
//! to add them. Real case this exists for (2026-09-30, Freeland
//! Warehousing & Storage): Intake's "Owner Level Users" text listed
//! Jessica Bradshaw and Matt Armstrong only -- the Merchant Account
//! Pre-App's second owner, Serene Armstrong, was never typed into
//! Intake at all, so she had no roster row and no candidate chip to
//! find her by.

/// One Merchant Pre-App owner (`party_role = 'owner'`).
pub struct OwnerIdentity {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
}

/// One Users-tab roster row.
pub struct RosterIdentity<'a> {
    pub full_name: &'a str,
    pub email: Option<&'a str>,
}

fn normalized_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Which `roster` indices a single Pre-App owner matches, under the
/// same-conservative rule described in this module's own doc comment.
/// Shared by `legal_owner_flags` (flags every index this returns) and
/// `unmatched_owners` (an owner is "missing" exactly when this is
/// empty) so the two can never quietly disagree on what counts as a
/// match.
fn owner_match_indices(roster: &[RosterIdentity], owner: &OwnerIdentity) -> Vec<usize> {
    let owner_name = non_blank(owner.display_name.as_deref()).map(normalized_name);

    let same_email: Vec<usize> = match non_blank(owner.email.as_deref()) {
        Some(owner_email) => roster
            .iter()
            .enumerate()
            .filter(|(_, person)| {
                non_blank(person.email).is_some_and(|e| e.eq_ignore_ascii_case(owner_email))
            })
            .map(|(i, _)| i)
            .collect(),
        None => Vec::new(),
    };

    match same_email.as_slice() {
        [only] => return vec![*only],
        [] => {}
        several => {
            return match &owner_name {
                Some(name) => several
                    .iter()
                    .copied()
                    .filter(|&i| normalized_name(roster[i].full_name) == *name)
                    .collect(),
                None => Vec::new(),
            };
        }
    }

    if let Some(name) = &owner_name {
        let same_name: Vec<usize> = roster
            .iter()
            .enumerate()
            .filter(|(_, person)| normalized_name(person.full_name) == *name)
            .map(|(i, _)| i)
            .collect();
        if let [only] = same_name.as_slice() {
            return vec![*only];
        }
    }

    Vec::new()
}

/// One flag per roster row, in `roster`'s own order.
pub fn legal_owner_flags(roster: &[RosterIdentity], owners: &[OwnerIdentity]) -> Vec<bool> {
    let mut flags = vec![false; roster.len()];

    for owner in owners {
        for i in owner_match_indices(roster, owner) {
            flags[i] = true;
        }
    }

    flags
}

/// Pre-App owners that match nobody in `known` -- the saved roster
/// *plus* any still-unadded Intake candidates, so an owner already
/// reachable through an existing "Add" chip is never also reported
/// here (see this module's own doc comment). An owner with no name at
/// all is never reported either -- there would be nothing to label an
/// "Add" action with, and `has_any_data` already guarantees every real
/// party row has at least one field, so a nameless one is a facility/
/// signer-only slot, not a person to surface.
pub fn unmatched_owners<'a>(
    known: &[RosterIdentity],
    owners: &'a [OwnerIdentity],
) -> Vec<&'a OwnerIdentity> {
    owners
        .iter()
        .filter(|owner| non_blank(owner.display_name.as_deref()).is_some())
        .filter(|owner| owner_match_indices(known, owner).is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person<'a>(full_name: &'a str, email: Option<&'a str>) -> RosterIdentity<'a> {
        RosterIdentity { full_name, email }
    }

    fn owner(name: Option<&str>, email: Option<&str>) -> OwnerIdentity {
        OwnerIdentity {
            display_name: name.map(str::to_string),
            email: email.map(str::to_string),
            phone: None,
        }
    }

    #[test]
    fn matches_on_email_even_when_the_names_differ_and_the_case_differs() {
        // The LG Squared shape: the Pre-App says "Laura Cathryn Sample",
        // the Intake roster says "Cathy Sample", same inbox, different case.
        let roster = [
            person("Pat Sample", Some("pat.sample@example.com")),
            person("Cathy Sample", Some("LSample1977@example.com")),
            person("Sam Example", Some("sam@example.com")),
        ];
        let owners = [
            owner(Some("Pat Sample"), Some("pat.sample@example.com")),
            owner(
                Some("Laura Cathryn Sample"),
                Some("lsample1977@example.com"),
            ),
        ];
        assert_eq!(legal_owner_flags(&roster, &owners), [true, true, false]);
    }

    #[test]
    fn a_shared_inbox_only_flags_the_person_whose_name_also_matches() {
        let roster = [
            person("Barb Family", Some("family@example.com")),
            person("Chad Family", Some("family@example.com")),
        ];
        let owners = [owner(Some("Barb Family"), Some("family@example.com"))];
        assert_eq!(legal_owner_flags(&roster, &owners), [true, false]);
    }

    #[test]
    fn a_shared_inbox_with_no_name_match_flags_nobody() {
        let roster = [
            person("Barb Family", Some("family@example.com")),
            person("Chad Family", Some("family@example.com")),
        ];
        let owners = [owner(Some("Someone Else"), Some("family@example.com"))];
        assert_eq!(legal_owner_flags(&roster, &owners), [false, false]);
    }

    #[test]
    fn falls_back_to_an_exact_name_when_the_owner_has_no_email() {
        let roster = [
            person("Pat  Sample", Some("a@example.com")),
            person("Sam", None),
        ];
        let owners = [owner(Some("pat sample"), None)];
        assert_eq!(legal_owner_flags(&roster, &owners), [true, false]);
    }

    #[test]
    fn falls_back_to_name_when_the_email_matches_nobody() {
        let roster = [person("Pat Sample", Some("work@example.com"))];
        let owners = [owner(Some("Pat Sample"), Some("home@example.com"))];
        assert_eq!(legal_owner_flags(&roster, &owners), [true]);
    }

    #[test]
    fn a_name_two_roster_rows_share_is_never_guessed_at() {
        let roster = [
            person("Pat Sample", Some("a@example.com")),
            person("Pat Sample", Some("b@example.com")),
        ];
        let owners = [owner(Some("Pat Sample"), None)];
        assert_eq!(legal_owner_flags(&roster, &owners), [false, false]);
    }

    #[test]
    fn no_owners_or_blank_owner_data_flags_nobody() {
        let roster = [person("Pat Sample", Some("a@example.com"))];
        assert_eq!(legal_owner_flags(&roster, &[]), [false]);
        let blank = [owner(Some("  "), Some(" "))];
        assert_eq!(legal_owner_flags(&roster, &blank), [false]);
    }

    #[test]
    fn an_owner_matching_nobody_known_is_reported_missing() {
        // Freeland's real shape: Intake's roster only ever named Jessica
        // Bradshaw and Matt Armstrong -- Serene Armstrong (Owner_1 on the
        // Merchant Account Pre-App) has no roster row and no Intake
        // candidate to be found by either.
        let known = [
            person("Jessica Bradshaw", Some("freeland.storage@gmail.com")),
            person("Matt Armstrong", Some("freeland.storage@gmail.com")),
        ];
        let owners = [owner(
            Some("Serene Armstrong"),
            Some("serene62772@yahoo.com"),
        )];
        let missing = unmatched_owners(&known, &owners);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].display_name.as_deref(), Some("Serene Armstrong"));
    }

    #[test]
    fn an_owner_already_on_the_roster_is_never_reported_missing() {
        let known = [person("Pat Sample", Some("pat.sample@example.com"))];
        let owners = [owner(Some("Pat Sample"), Some("pat.sample@example.com"))];
        assert_eq!(unmatched_owners(&known, &owners).len(), 0);
    }

    #[test]
    fn an_owner_already_an_unadded_intake_candidate_is_never_reported_missing() {
        // The exact concern this function exists to avoid: the same
        // person named on both PS forms must never produce a second,
        // duplicate "Add" entry alongside their existing Intake chip.
        let known = [person("Serene Armstrong", Some("serene62772@yahoo.com"))];
        let owners = [owner(
            Some("Serene Armstrong"),
            Some("serene62772@yahoo.com"),
        )];
        assert_eq!(unmatched_owners(&known, &owners).len(), 0);
    }

    #[test]
    fn a_nameless_owner_is_never_reported_missing() {
        let known: [RosterIdentity; 0] = [];
        let owners = [owner(None, Some("nobody@example.com"))];
        assert_eq!(unmatched_owners(&known, &owners).len(), 0);
    }
}
