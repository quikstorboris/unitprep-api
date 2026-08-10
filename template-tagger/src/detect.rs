/// One known value to search for, paired with the QMS merge tag it
/// should become if found literally in the document. The caller (not
/// this crate) is responsible for resolving which `client_ops.qms_tag`
/// rows are safe to pass in -- see the module doc's scope note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagValue {
    pub tag_key: String,
    pub value: String,
}

/// One literal occurrence of a [`TagValue`]'s value found in the
/// document, proposed for substitution -- never applied by this crate
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub tag_key: String,
    /// The exact text found in the document -- kept alongside the
    /// candidate rather than making the caller re-derive it from
    /// `tag_key`, since a value can occur more than once and this is
    /// the specific copy matched at `start`/`end`.
    pub matched_text: String,
    /// Byte offsets into `document_text`, both guaranteed valid UTF-8
    /// char boundaries. Byte-oriented rather than line/column, since
    /// that's what every downstream consumer (a `.docx` range, a diff)
    /// ultimately needs; a caller rendering this for display does its
    /// own line/column translation.
    pub start: usize,
    pub end: usize,
}

/// Finds every literal, word-bounded occurrence of each [`TagValue`]'s
/// `value` in `document_text` and returns one [`Candidate`] per match,
/// sorted by position.
///
/// - **Case-insensitive, ASCII only.** An address typed in Title Case in
///   a lease and in ALL CAPS on a mailing label should still match, but
///   folding is done with [`str::to_ascii_lowercase`], not full Unicode
///   case-folding ([`str::to_lowercase`]) -- deliberately, so that byte
///   offsets computed against the folded copy stay valid against the
///   original `document_text` (`to_ascii_lowercase` only ever rewrites
///   single-byte ASCII code points in place; it can never change a
///   string's byte length or UTF-8 structure the way full Unicode
///   case-folding can for some characters). The cost is real and
///   intentional: a non-ASCII letter's case variants (`é`/`É`, `ß`/`SS`)
///   will not match each other. Fine for this crate's locked scope
///   (name/address/phone/email/DL#/unit number); revisit if that scope
///   ever widens to fields where this would bite in practice.
/// - **Word-bounded on both sides**, so a short value (e.g. a 3-digit
///   unit number) can't match as a substring of a longer, unrelated
///   token -- "word" means [`char::is_alphanumeric`]; a boundary is any
///   position where the neighboring character (or the string edge) is
///   not alphanumeric.
/// - **Skips a `TagValue` whose `value` is empty after trimming** --
///   nothing to search for, and an empty needle would otherwise match
///   everywhere.
/// - **Does not deduplicate across `TagValue`s.** If two supplied values
///   happen to overlap in the document (e.g. a first name that is also
///   a substring of a full name supplied separately), every real
///   occurrence of each is reported independently. Deduplication is the
///   human reviewer's job, per the propose-never-modify rule this crate
///   exists to serve -- collapsing candidates here would be a silent
///   judgment call this crate isn't scoped to make.
pub fn detect_candidates(document_text: &str, values: &[TagValue]) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    for tag_value in values {
        let needle = tag_value.value.trim();
        if needle.is_empty() {
            continue;
        }

        for (start, end) in find_word_bounded_matches(document_text, needle) {
            candidates.push(Candidate {
                tag_key: tag_value.tag_key.clone(),
                matched_text: document_text[start..end].to_string(),
                start,
                end,
            });
        }
    }

    candidates.sort_by_key(|candidate| candidate.start);
    candidates
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// Every byte-range `(start, end)` in `haystack` where `needle` occurs,
/// ASCII-case-insensitively and word-bounded on both sides. See
/// [`detect_candidates`]'s doc comment for why folding is ASCII-only.
fn find_word_bounded_matches(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let haystack_folded = haystack.to_ascii_lowercase();
    let needle_folded = needle.to_ascii_lowercase();

    let mut matches = Vec::new();
    let mut search_from = 0;

    while let Some(relative_start) = haystack_folded[search_from..].find(&needle_folded) {
        let start = search_from + relative_start;
        let end = start + needle_folded.len();

        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);
        let after_ok = haystack[end..]
            .chars()
            .next()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);

        if before_ok && after_ok {
            matches.push((start, end));
        }

        // Advance by one byte, not by `needle_folded.len()` -- a
        // rejected match (failed a boundary check) must still make
        // progress, or a needle that only ever appears as a substring
        // of a longer word would loop forever re-finding the same
        // rejected position. One byte is always safe here since `start`
        // is already a valid char boundary (returned by `find`).
        search_from = start + 1;
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(tag_key: &str, value: &str) -> TagValue {
        TagValue {
            tag_key: tag_key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn finds_a_single_literal_match() {
        let text = "Tenant: John Smith, unit 204.";
        let candidates = detect_candidates(text, &[tag("e.fname", "John Smith")]);

        assert_eq!(
            candidates,
            vec![Candidate {
                tag_key: "e.fname".to_string(),
                matched_text: "John Smith".to_string(),
                start: 8,
                end: 18,
            }]
        );
    }

    #[test]
    fn is_case_insensitive_for_ascii() {
        let text = "MAILING LABEL: JOHN SMITH";
        let candidates = detect_candidates(text, &[tag("e.fname", "John Smith")]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].matched_text, "JOHN SMITH");
    }

    #[test]
    fn does_not_fold_case_for_non_ascii_letters() {
        // Documents the intentional limitation from the doc comment --
        // "É" and "é" are treated as distinct characters.
        let text = "Tenant: JOSÉ GARCIA";
        let candidates = detect_candidates(text, &[tag("e.fname", "José Garcia")]);

        assert!(candidates.is_empty());
    }

    #[test]
    fn respects_word_boundaries_on_both_sides() {
        // "555" must not match inside "5551234" or "12345550".
        let text = "Case 5551234, ref 12345550, phone 555-0100.";
        let candidates = detect_candidates(text, &[tag("e.phone", "555")]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].start, 34);
    }

    #[test]
    fn finds_every_occurrence_of_a_repeated_value() {
        let text = "204 is available. Unit 204 rent is current.";
        let candidates = detect_candidates(text, &[tag("u.num", "204")]);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].start, 0);
        assert_eq!(candidates[1].start, 23);
    }

    #[test]
    fn skips_a_blank_or_whitespace_only_value() {
        let text = "Nothing to see here.";
        let candidates = detect_candidates(text, &[tag("e.fname", "   ")]);

        assert!(candidates.is_empty());
    }

    #[test]
    fn reports_no_candidate_when_the_value_is_absent() {
        let text = "This document never mentions the tenant's name.";
        let candidates = detect_candidates(text, &[tag("e.fname", "Priya Patel")]);

        assert!(candidates.is_empty());
    }

    #[test]
    fn does_not_deduplicate_overlapping_values_across_tags() {
        // "John" (first name) is a literal substring of "John Smith"
        // (full name) supplied as a second, independent TagValue --
        // both are real, valid candidates and both must be reported.
        let text = "Tenant: John Smith.";
        let candidates =
            detect_candidates(text, &[tag("e.fname", "John"), tag("e.name", "John Smith")]);

        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().any(|c| c.tag_key == "e.fname"));
        assert!(candidates.iter().any(|c| c.tag_key == "e.name"));
    }

    #[test]
    fn sorts_results_by_position_regardless_of_input_order() {
        let text = "Unit 204, tenant John Smith.";
        let candidates =
            detect_candidates(text, &[tag("e.fname", "John Smith"), tag("u.num", "204")]);

        assert_eq!(candidates[0].tag_key, "u.num");
        assert_eq!(candidates[1].tag_key, "e.fname");
    }

    #[test]
    fn handles_multiple_distinct_tags_with_no_matches_at_all() {
        let candidates = detect_candidates(
            "Blank document.",
            &[tag("e.fname", "Alex"), tag("e.lname", "Rivera")],
        );

        assert!(candidates.is_empty());
    }
}
