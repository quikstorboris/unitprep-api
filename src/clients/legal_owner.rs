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

/// One Merchant Pre-App owner (`party_role = 'owner'`).
pub struct OwnerIdentity {
    pub display_name: Option<String>,
    pub email: Option<String>,
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

/// One flag per roster row, in `roster`'s own order.
pub fn legal_owner_flags(roster: &[RosterIdentity], owners: &[OwnerIdentity]) -> Vec<bool> {
    let mut flags = vec![false; roster.len()];

    for owner in owners {
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
            [only] => {
                flags[*only] = true;
                continue;
            }
            [] => {}
            several => {
                if let Some(name) = &owner_name {
                    for &i in several {
                        if normalized_name(roster[i].full_name) == *name {
                            flags[i] = true;
                        }
                    }
                }
                continue;
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
                flags[*only] = true;
            }
        }
    }

    flags
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
}
