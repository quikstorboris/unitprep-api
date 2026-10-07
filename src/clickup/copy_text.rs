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

/// What follows the comment body on every copied comment: three line
/// breaks, then this label and a link to the task it was copied from.
const FOOTER_LEAD: &str = "\n\n\nMain tracker task - ";

/// The footer's label as it reads in ClickUp (without the line breaks),
/// used to take the footer back off when comparing a posted comment.
const FOOTER_LABEL: &str = "main tracker task - ";

/// The parts of a copied comment: `comment`, then -- when the source task
/// is known -- the "Main tracker task - {source task}" footer, the task's
/// name linking to it.
pub fn comment_parts<'a>(
    comment: &'a str,
    source: Option<(&'a str, &'a str)>,
) -> Vec<(&'a str, Option<&'a str>)> {
    let mut parts = vec![(comment, None)];
    if let Some((name, url)) = source {
        parts.push((FOOTER_LEAD, None));
        parts.push((name, Some(url)));
    }
    parts
}

/// `comment` without a trailing "Main tracker task - ..." footer.
fn without_footer(comment: &str) -> &str {
    match comment.to_lowercase().rfind(FOOTER_LABEL) {
        // Lowercasing can change byte offsets only for characters whose
        // lowercase differs in length; the label itself is ASCII, so the
        // offset is only trusted when it lands on a char boundary.
        Some(at) if comment.is_char_boundary(at) => &comment[..at],
        _ => comment,
    }
}

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
    // A copy Orchestrator posted ends with the "Main tracker task" footer,
    // which the source comment never has.
    !candidate.is_empty() && compact_label(without_footer(existing)) == candidate
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
    fn a_copied_comment_ends_with_three_line_breaks_and_the_source_link() {
        let parts = comment_parts(
            "Fees done.",
            Some(("CONFIGURE Fees", "https://app.clickup.com/t/abc")),
        );

        assert_eq!(
            parts,
            vec![
                ("Fees done.", None),
                ("\n\n\nMain tracker task - ", None),
                ("CONFIGURE Fees", Some("https://app.clickup.com/t/abc")),
            ]
        );
        assert_eq!(parts[1].0.matches('\n').count(), 3);
    }

    #[test]
    fn without_a_known_source_the_comment_is_posted_as_is() {
        assert_eq!(
            comment_parts("Fees done.", None),
            vec![("Fees done.", None)]
        );
    }

    #[test]
    fn a_footered_copy_still_counts_as_already_copied() {
        assert!(looks_already_copied(
            "Fees done.\n\n\nMain tracker task - CONFIGURE Fees",
            "Fees done."
        ));
        // ...but a different comment with a footer does not.
        assert!(!looks_already_copied(
            "Specials done.\n\n\nMain tracker task - CONFIGURE Fees",
            "Fees done."
        ));
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
