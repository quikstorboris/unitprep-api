//! The wording ClickUp Copy writes, and the checks that recognize it
//! again.
//!
//! **There is no marker in these comments yet.** Orchestrator does not
//! store what it has copied (ClickUp is the record), so "was this already
//! copied?" and "is the pointer already there?" are answered by looking
//! at the target task's existing comments and matching their *wording*.
//! That is deliberate and temporary: the plan is to add a visible marker
//! such as "Auto-added by OO" to what Orchestrator posts, and match on
//! that instead.
//!
//! Every function that matches on wording carries a `MARKER-TODO` tag, so
//! `grep -rn MARKER-TODO src` lists exactly the places to change.

use super::tasks::compact_label;

/// The opening of the pointer comment. The list's name follows it as a
/// link.
const POINTER_LEAD: &str = "Main task list for this client is ";

/// The pointer comment: a generic note, posted once on a target task,
/// saying which facility's list is the client's main one. `list_url` makes
/// the list's name a link.
pub fn pointer_parts<'a>(list_name: &'a str, list_url: &'a str) -> Vec<(&'a str, Option<&'a str>)> {
    vec![(POINTER_LEAD, None), (list_name, Some(list_url))]
}

/// Whether `comment` is a pointer comment (this one or one posted
/// earlier), whichever list it names -- a task needs the note only once,
/// and a later change of parent is not a reason to post a second one.
///
/// MARKER-TODO: matches on wording because the comment carries no marker
/// yet. Match the marker instead once one is added.
pub fn is_pointer_comment(comment: &str) -> bool {
    comment
        .trim_start()
        .to_lowercase()
        .starts_with(&POINTER_LEAD.to_lowercase())
}

/// Whether `existing` is `candidate` already: equal ignoring case,
/// spacing and punctuation. Used to flag a target task that looks as if
/// the comment was copied before.
///
/// MARKER-TODO: matches on wording because copied comments carry no
/// marker yet. Match the marker (plus the source comment's identity)
/// instead once one is added.
pub fn looks_already_copied(existing: &str, candidate: &str) -> bool {
    let candidate = compact_label(candidate);
    !candidate.is_empty() && compact_label(existing) == candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_names_the_list_and_links_it() {
        let parts = pointer_parts("Acme Main St", "https://app.clickup.com/1/v/li/9");

        assert_eq!(parts[0], ("Main task list for this client is ", None));
        assert_eq!(
            parts[1],
            ("Acme Main St", Some("https://app.clickup.com/1/v/li/9"))
        );
    }

    #[test]
    fn a_pointer_comment_is_recognized_whatever_list_it_names() {
        assert!(is_pointer_comment(
            "Main task list for this client is Acme Main St"
        ));
        assert!(is_pointer_comment(
            "  main TASK list for this client is Some Other Facility"
        ));
    }

    #[test]
    fn an_ordinary_comment_is_not_a_pointer() {
        assert!(!is_pointer_comment("Delinquency is configured."));
        assert!(!is_pointer_comment(""));
        // Mentioning the phrase mid-comment is not the pointer.
        assert!(!is_pointer_comment(
            "FYI the main task list for this client is elsewhere"
        ));
    }

    #[test]
    fn a_copy_is_recognized_despite_case_spacing_and_punctuation() {
        assert!(looks_already_copied(
            "Delinquency  rules are set: 3 steps.",
            "delinquency rules are set 3 steps"
        ));
    }

    #[test]
    fn a_different_or_empty_comment_is_not_a_copy() {
        assert!(!looks_already_copied(
            "Fees are done.",
            "Specials are done."
        ));
        assert!(!looks_already_copied("anything", "  "));
        assert!(!looks_already_copied("", ""));
    }
}
