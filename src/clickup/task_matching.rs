//! Finds the ClickUp task that stands for an onboarding step -- today the
//! 1st/2nd duplicate check -- among a facility list's tasks.
//!
//! Task names are not reliable: the verb changes (PERFORM / COMPLETE /
//! RUN), numbering and emoji prefixes vary by template ("2. 👥 CONFIGURE
//! Users"), and the same step is sometimes worded differently. So this
//! never demands an exact name: each step carries a few *phrases* (data,
//! `integrations.clickup_task_steps`), every task is scored against them
//! and the person picks from the ranked list -- even when only one task
//! qualifies, because a wrong silent match would write to the wrong
//! task under their name.

use std::collections::{HashMap, HashSet};

use super::tasks::ClickUpTask;

/// Tasks scoring below this are not offered.
const MIN_SCORE: f64 = 0.4;

/// At most this many candidates are returned.
const MAX_CANDIDATES: usize = 10;

/// Applied when a task names a different ordinal ("2nd") than the step
/// being updated, so a 2nd-check task is not offered for the 1st check
/// on the strength of the words they share.
const ORDINAL_MISMATCH_FACTOR: f64 = 0.4;

/// Words that carry no identity: the action verbs templates swap freely,
/// and filler.
const FILLER: &[&str] = &[
    "perform",
    "complete",
    "completed",
    "run",
    "do",
    "the",
    "a",
    "an",
    "and",
    "of",
    "for",
    "to",
];

/// One onboarding step as stored in `integrations.clickup_task_steps`.
#[derive(Debug, Clone)]
pub struct StepDefinition {
    pub step_key: String,
    pub label: String,
    /// 1 for the first check, 2 for the second (and later) ones.
    pub ordinal: i32,
    pub phrases: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RankedTask<'a> {
    pub task: &'a ClickUpTask,
    pub score: f64,
}

/// Lowercased words of `name` with filler and bare numbering ("2.")
/// removed, ordinal words unified ("second" -> "2nd") and a plural "s"
/// trimmed, so "Corrections" and "correction" compare equal.
pub(super) fn tokens(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .map(|word| match word.as_str() {
            "first" => "1st".to_string(),
            "second" => "2nd".to_string(),
            "third" => "3rd".to_string(),
            _ => word,
        })
        .filter(|word| !FILLER.contains(&word.as_str()))
        .filter(|word| !word.chars().all(|c| c.is_ascii_digit()))
        .map(|word| {
            if word.len() > 3 && word.ends_with('s') && !is_ordinal(&word) {
                word[..word.len() - 1].to_string()
            } else {
                word
            }
        })
        .collect()
}

fn is_ordinal(token: &str) -> bool {
    ordinal_value(token).is_some()
}

fn ordinal_value(token: &str) -> Option<i32> {
    let digits: String = token.chars().take_while(char::is_ascii_digit).collect();
    let suffix = &token[digits.len()..];
    if digits.is_empty() || !matches!(suffix, "st" | "nd" | "rd" | "th") {
        return None;
    }
    digits.parse().ok()
}

/// Sorensen-Dice overlap of two token sets.
pub(super) fn dice(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let a: HashSet<&String> = a.iter().collect();
    let b: HashSet<&String> = b.iter().collect();
    2.0 * a.intersection(&b).count() as f64 / (a.len() + b.len()) as f64
}

fn score(step: &StepDefinition, task_name: &str) -> f64 {
    let task_tokens = tokens(task_name);

    let best = step
        .phrases
        .iter()
        .map(|phrase| dice(&task_tokens, &tokens(phrase)))
        .fold(0.0, f64::max);

    let task_ordinal = task_tokens.iter().find_map(|t| ordinal_value(t));
    match task_ordinal {
        // Any ordinal from 2 up is "a later check"; step 2 stands for all of them.
        Some(found) if found.min(2) != step.ordinal.min(2) => best * ORDINAL_MISMATCH_FACTOR,
        _ => best,
    }
}

/// The tasks worth offering for `step`, best first (ties by name).
pub fn rank<'a>(step: &StepDefinition, tasks: &'a [ClickUpTask]) -> Vec<RankedTask<'a>> {
    let mut ranked: Vec<RankedTask<'a>> = tasks
        .iter()
        .map(|task| RankedTask {
            task,
            score: score(step, &task.name),
        })
        .filter(|ranked| ranked.score >= MIN_SCORE)
        .collect();

    // Equal scores: a subtask (the actionable item) before a parent
    // section that happens to share its words, then by name.
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.task.parent_id.is_some().cmp(&a.task.parent_id.is_some()))
            .then_with(|| a.task.name.cmp(&b.task.name))
    });
    ranked.truncate(MAX_CANDIDATES);
    ranked
}

/// Parent task names by id, for showing which section of the list a
/// candidate sits under ("Duplicate Tenant Corrections" appears under
/// several parents in a real template).
pub fn parent_names(tasks: &[ClickUpTask]) -> HashMap<&str, &str> {
    tasks
        .iter()
        .map(|task| (task.id.as_str(), task.name.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(ordinal: i32, phrases: &[&str]) -> StepDefinition {
        StepDefinition {
            step_key: format!("dedup_{ordinal}"),
            label: format!("{ordinal} check"),
            ordinal,
            phrases: phrases.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn first() -> StepDefinition {
        step(
            1,
            &[
                "complete duplicate tenant corrections",
                "perform duplicate check",
                "perform 1st duplicate check",
            ],
        )
    }

    fn second() -> StepDefinition {
        step(2, &["perform 2nd duplicate check"])
    }

    fn task(id: &str, name: &str) -> ClickUpTask {
        ClickUpTask {
            id: id.to_string(),
            name: name.to_string(),
            status: "to do".to_string(),
            status_type: "open".to_string(),
            parent_id: None,
            assignees: Vec::new(),
            url: String::new(),
            list_id: None,
            dropdowns: Vec::new(),
        }
    }

    fn names<'a>(ranked: &[RankedTask<'a>]) -> Vec<&'a str> {
        ranked.iter().map(|r| r.task.name.as_str()).collect()
    }

    #[test]
    fn the_exact_names_rank_first_for_their_step() {
        let tasks = [
            task("1", "COMPLETE Duplicate Tenant Corrections"),
            task("2", "PERFORM 2nd Duplicate Check"),
            task("3", "CONFIGURE Default User Roles"),
        ];

        assert_eq!(
            names(&rank(&first(), &tasks))[0],
            "COMPLETE Duplicate Tenant Corrections"
        );
        assert_eq!(
            names(&rank(&second(), &tasks)),
            vec!["PERFORM 2nd Duplicate Check"]
        );
    }

    #[test]
    fn a_second_check_task_is_not_the_best_offer_for_the_first_check() {
        let tasks = [
            task("1", "PERFORM 2nd Duplicate Check"),
            task("2", "COMPLETE Duplicate Tenant Corrections"),
        ];
        let ranked = rank(&first(), &tasks);

        assert_eq!(ranked[0].task.id, "2");
        // It may still be listed (names are unreliable) but well behind.
        assert!(ranked.iter().all(|r| r.task.id == "2" || r.score < 0.5));
    }

    #[test]
    fn emoji_numbering_verbs_plurals_and_ordinal_words_do_not_matter() {
        let tasks = [
            task("1", "4. 🦕 RUN Duplicate Tenant Correction"),
            task("2", "🦕PERFORM Second Duplicate Check"),
        ];

        assert_eq!(
            names(&rank(&first(), &tasks))[0],
            "4. 🦕 RUN Duplicate Tenant Correction"
        );
        assert_eq!(
            names(&rank(&second(), &tasks))[0],
            "🦕PERFORM Second Duplicate Check"
        );
    }

    #[test]
    fn unrelated_tasks_are_never_offered_and_a_lone_match_still_is() {
        let tasks = [
            task("1", "ADD Recurring Fees"),
            task("2", "CONFIGURE Users"),
            task("3", "Duplicate Tenant Corrections"),
        ];
        assert_eq!(
            names(&rank(&first(), &tasks)),
            vec!["Duplicate Tenant Corrections"]
        );
        assert!(rank(&second(), &[task("1", "ADD Recurring Fees")]).is_empty());
    }

    #[test]
    fn the_evaluate_contact_list_wording_matches_when_it_is_a_phrase() {
        let step = step(1, &["Evaluate Tenant Contact List for Duplicates"]);
        let tasks = [
            task("1", "EVALUATE Tenant Contact List for Duplicates"),
            task("2", "ADD Recurring Fees"),
        ];
        let ranked = rank(&step, &tasks);

        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].task.id, "1");
    }

    #[test]
    fn a_subtask_outranks_a_parent_section_with_the_same_words() {
        let mut subtask = task("2", "COMPLETE Duplicate Tenant Corrections");
        subtask.parent_id = Some("1".to_string());
        let tasks = [task("1", "4. Duplicate Tenant Corrections"), subtask];

        assert_eq!(rank(&first(), &tasks)[0].task.id, "2");
    }

    #[test]
    fn a_third_or_later_check_counts_as_a_later_check() {
        let tasks = [task("1", "PERFORM 3rd Duplicate Check")];
        assert!(!rank(&second(), &tasks).is_empty());
        assert!(rank(&first(), &tasks).iter().all(|r| r.score < 0.5));
    }

    #[test]
    fn candidates_are_capped_and_tie_broken_by_name() {
        let tasks: Vec<ClickUpTask> = (0..25)
            .map(|i| {
                task(
                    &i.to_string(),
                    &format!("Duplicate Tenant Corrections {i:02}"),
                )
            })
            .collect();
        let ranked = rank(&first(), &tasks);

        assert_eq!(ranked.len(), MAX_CANDIDATES);
        assert_eq!(ranked[0].task.name, "Duplicate Tenant Corrections 00");
    }

    #[test]
    fn parent_names_map_task_ids_to_names() {
        let tasks = [task("p1", "Parent")];
        assert_eq!(parent_names(&tasks).get("p1"), Some(&"Parent"));
    }
}
