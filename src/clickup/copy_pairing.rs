//! ClickUp Copy: pairing each task in a source facility's list with its
//! counterpart in a target facility's list, so a comment can be copied
//! across.
//!
//! Both lists come from the same onboarding template, so a counterpart
//! has (nearly) the same name and sits in the same **Onboarding Phase**.
//! Names still drift (verbs, numbering, emoji, "ADD" vs "CONFIGURE") and
//! repeat within a list ("ADD Recurring Fees" appears under several
//! parents), so a pair is scored on the task's name *and* its parent's
//! name, and is only ever a *suggestion*: the person confirms or
//! overrides every row, like the duplicate-check task lookup.
//!
//! Only tasks in the phases ClickUp Copy handles ([`COPY_PHASES`]) take
//! part. Name normalization is shared with `task_matching`.

use std::collections::{HashMap, HashSet};

use super::task_matching::{dice, tokens};
use super::tasks::{compact_label, ClickUpTask};

/// The custom field that groups a template's tasks ("Set Up",
/// "Migration", ...).
///
/// The field and phase names below are the template's, found in the
/// vault's ClickUp design log; they are compared by [`compact_label`]
/// (case, spacing, emoji ignored). They are constants for now -- if the
/// template wording changes, or more phases need copying, they should
/// become data like `integrations.clickup_task_steps`.
pub const PHASE_FIELD: &str = "Onboarding Phase";

/// The phases whose comments ClickUp Copy handles.
pub const COPY_PHASES: &[&str] = &["Set Up", "Migration"];

/// The Corporate / Facility custom field: whether a task is done once
/// per client or once per facility. Used as a filter.
pub const SCOPE_FIELD: &str = "Corp/Fac";

/// A pair scoring below this is not offered.
const MIN_SCORE: f64 = 0.5;

/// How much of the score is the task's own name; the rest is its
/// parent's name, when both have one. Name matters most, but the parent
/// is what tells apart a name that repeats under several parents.
const NAME_WEIGHT: f64 = 0.7;

/// At most this many alternative targets are returned per source.
const MAX_ALTERNATIVES: usize = 3;

/// Whether a task is done once per client or once per facility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Corporate,
    Facility,
}

/// The task's Corp/Fac value, when it has one this code recognizes.
pub fn scope(task: &ClickUpTask) -> Option<Scope> {
    let label = compact_label(&task.dropdown(SCOPE_FIELD)?.option_name);
    if label.starts_with("corp") {
        Some(Scope::Corporate)
    } else if label.starts_with("fac") {
        Some(Scope::Facility)
    } else {
        None
    }
}

/// The task's phase, as the template spells it ("Set Up"), when it is
/// one of [`COPY_PHASES`].
pub fn copy_phase(task: &ClickUpTask) -> Option<&str> {
    let phase = task.dropdown(PHASE_FIELD)?;
    let label = compact_label(&phase.option_name);
    COPY_PHASES
        .iter()
        .any(|wanted| compact_label(wanted) == label)
        .then_some(phase.option_name.as_str())
}

/// The tasks ClickUp Copy handles: those in [`COPY_PHASES`], optionally
/// narrowed to one [`Scope`].
pub fn eligible(tasks: &[ClickUpTask], only: Option<Scope>) -> Vec<&ClickUpTask> {
    tasks
        .iter()
        .filter(|task| copy_phase(task).is_some())
        .filter(|task| only.is_none_or(|wanted| scope(task) == Some(wanted)))
        .collect()
}

#[derive(Debug, Clone)]
pub struct Candidate<'a> {
    pub task: &'a ClickUpTask,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct Pairing<'a> {
    pub source: &'a ClickUpTask,
    /// The suggested counterpart; `None` when nothing in the same phase
    /// scored well enough (shown as "no match").
    pub target: Option<Candidate<'a>>,
    /// Other plausible counterparts, best first, for the override.
    pub alternatives: Vec<Candidate<'a>>,
}

fn parent_names(tasks: &[ClickUpTask]) -> HashMap<&str, &str> {
    tasks
        .iter()
        .map(|task| (task.id.as_str(), task.name.as_str()))
        .collect()
}

fn parent_of<'a>(task: &ClickUpTask, names: &HashMap<&str, &'a str>) -> Option<&'a str> {
    names.get(task.parent_id.as_deref()?).copied()
}

/// Two tasks are in the same phase when their option ids match or their
/// labels agree ignoring case, spacing and emoji. (Option ids are shared
/// by lists built from one template, but names are the safer fallback.)
fn same_phase(a: &ClickUpTask, b: &ClickUpTask) -> bool {
    match (a.dropdown(PHASE_FIELD), b.dropdown(PHASE_FIELD)) {
        (Some(a), Some(b)) => {
            a.option_id == b.option_id
                || compact_label(&a.option_name) == compact_label(&b.option_name)
        }
        _ => false,
    }
}

fn score(
    source: &ClickUpTask,
    source_parent: Option<&str>,
    target: &ClickUpTask,
    target_parent: Option<&str>,
) -> f64 {
    let name = dice(&tokens(&source.name), &tokens(&target.name));
    match (source_parent, target_parent) {
        (Some(a), Some(b)) => {
            NAME_WEIGHT * name + (1.0 - NAME_WEIGHT) * dice(&tokens(a), &tokens(b))
        }
        _ => name,
    }
}

/// Pairs the eligible tasks of `source_tasks` with those of
/// `target_tasks`, one-to-one: the best-scoring pairs claim their tasks
/// first, so two source tasks never suggest the same target. Results
/// follow the source list's order.
pub fn pair<'a>(
    source_tasks: &'a [ClickUpTask],
    target_tasks: &'a [ClickUpTask],
    only: Option<Scope>,
) -> Vec<Pairing<'a>> {
    let sources = eligible(source_tasks, only);
    let targets = eligible(target_tasks, only);
    let source_parents = parent_names(source_tasks);
    let target_parents = parent_names(target_tasks);

    // Every plausible pairing, best first.
    let mut scored: Vec<(usize, usize, f64)> = Vec::new();
    for (s, source) in sources.iter().enumerate() {
        for (t, target) in targets.iter().enumerate() {
            if !same_phase(source, target) {
                continue;
            }
            let value = score(
                source,
                parent_of(source, &source_parents),
                target,
                parent_of(target, &target_parents),
            );
            if value >= MIN_SCORE {
                scored.push((s, t, value));
            }
        }
    }
    scored.sort_by(|a, b| {
        b.2.total_cmp(&a.2)
            .then_with(|| sources[a.0].name.cmp(&sources[b.0].name))
            .then_with(|| targets[a.1].name.cmp(&targets[b.1].name))
    });

    let mut claimed_sources: HashSet<usize> = HashSet::new();
    let mut claimed_targets: HashSet<usize> = HashSet::new();
    let mut chosen: HashMap<usize, (usize, f64)> = HashMap::new();
    for &(s, t, value) in &scored {
        if claimed_sources.contains(&s) || claimed_targets.contains(&t) {
            continue;
        }
        claimed_sources.insert(s);
        claimed_targets.insert(t);
        chosen.insert(s, (t, value));
    }

    sources
        .iter()
        .enumerate()
        .map(|(s, source)| {
            let target = chosen.get(&s).map(|&(t, value)| Candidate {
                task: targets[t],
                score: value,
            });
            let alternatives = scored
                .iter()
                .filter(|&&(from, t, _)| from == s && chosen.get(&s).map(|c| c.0) != Some(t))
                .take(MAX_ALTERNATIVES)
                .map(|&(_, t, value)| Candidate {
                    task: targets[t],
                    score: value,
                })
                .collect();

            Pairing {
                source,
                target,
                alternatives,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::tasks::TaskDropdown;
    use super::*;

    fn dropdown(field: &str, option_id: &str, option_name: &str) -> TaskDropdown {
        TaskDropdown {
            field_id: format!("f-{field}"),
            field_name: field.to_string(),
            option_id: option_id.to_string(),
            option_name: option_name.to_string(),
        }
    }

    fn task(id: &str, name: &str, parent: Option<&str>, phase: &str, scope: &str) -> ClickUpTask {
        ClickUpTask {
            id: id.to_string(),
            name: name.to_string(),
            status: "to do".to_string(),
            status_type: "open".to_string(),
            parent_id: parent.map(str::to_string),
            assignees: Vec::new(),
            url: String::new(),
            list_id: None,
            dropdowns: vec![
                dropdown(PHASE_FIELD, &format!("o-{phase}"), phase),
                dropdown(SCOPE_FIELD, &format!("o-{scope}"), scope),
            ],
        }
    }

    fn setup(id: &str, name: &str, parent: Option<&str>) -> ClickUpTask {
        task(id, name, parent, "Set Up", "Corporate")
    }

    fn target_id<'a>(pairings: &'a [Pairing<'_>], source_id: &str) -> Option<&'a str> {
        pairings
            .iter()
            .find(|p| p.source.id == source_id)?
            .target
            .as_ref()
            .map(|c| c.task.id.as_str())
    }

    #[test]
    fn same_named_tasks_in_the_same_phase_are_paired() {
        let source = [setup("s1", "CONFIGURE Delinquency", None)];
        let target = [setup("t1", "CONFIGURE Delinquency", None)];

        let pairings = pair(&source, &target, None);

        assert_eq!(target_id(&pairings, "s1"), Some("t1"));
    }

    #[test]
    fn emoji_numbering_and_a_swapped_verb_do_not_prevent_a_pair() {
        let source = [setup(
            "s1",
            "3.💲CONFIGURE Fees, Security Deposits & Taxes",
            None,
        )];
        let target = [setup("t1", "PERFORM Fees, Security Deposits & Taxes", None)];

        assert_eq!(target_id(&pair(&source, &target, None), "s1"), Some("t1"));
    }

    #[test]
    fn a_name_repeated_under_different_parents_is_told_apart_by_the_parent() {
        let source = [
            setup("p-fees", "Fees", None),
            setup("p-users", "Users", None),
            setup("s-fees", "ADD Recurring Fees", Some("p-fees")),
            setup("s-users", "ADD Recurring Fees", Some("p-users")),
        ];
        // Same two parents in the target, listed in the opposite order.
        let target = [
            setup("q-users", "Users", None),
            setup("q-fees", "Fees", None),
            setup("t-users", "ADD Recurring Fees", Some("q-users")),
            setup("t-fees", "ADD Recurring Fees", Some("q-fees")),
        ];

        let pairings = pair(&source, &target, None);

        assert_eq!(target_id(&pairings, "s-fees"), Some("t-fees"));
        assert_eq!(target_id(&pairings, "s-users"), Some("t-users"));
    }

    #[test]
    fn tasks_in_different_phases_never_pair() {
        let source = [setup("s1", "Import Tenants", None)];
        let target = [task("t1", "Import Tenants", None, "Migration", "Facility")];

        let pairings = pair(&source, &target, None);

        assert!(target_id(&pairings, "s1").is_none());
    }

    #[test]
    fn two_spellings_of_a_phase_count_as_the_same_phase() {
        let source = [task("s1", "Configure Users", None, "Setup", "Corporate")];
        let target = [task("t1", "Configure Users", None, "🛠 Set Up", "Corporate")];

        assert_eq!(target_id(&pair(&source, &target, None), "s1"), Some("t1"));
    }

    #[test]
    fn a_target_is_claimed_by_only_one_source() {
        let source = [
            setup("s1", "Configure Specials", None),
            setup("s2", "Configure Specials Promo", None),
        ];
        let target = [setup("t1", "Configure Specials", None)];

        let pairings = pair(&source, &target, None);

        // The exact name wins the single target; the other is unmatched.
        assert_eq!(target_id(&pairings, "s1"), Some("t1"));
        assert!(target_id(&pairings, "s2").is_none());
    }

    #[test]
    fn a_task_with_no_counterpart_is_unmatched_but_still_listed() {
        let source = [setup("s1", "Configure Delinquency", None)];
        let target = [setup("t1", "Order Door Hardware", None)];

        let pairings = pair(&source, &target, None);

        assert_eq!(pairings.len(), 1);
        assert!(pairings[0].target.is_none());
    }

    #[test]
    fn near_misses_are_offered_as_alternatives() {
        let source = [setup("s1", "Configure Delinquency Rules", None)];
        let target = [
            setup("t1", "Configure Delinquency Rules", None),
            setup("t2", "Configure Delinquency", None),
        ];

        let pairing = &pair(&source, &target, None)[0];

        assert_eq!(pairing.target.as_ref().unwrap().task.id, "t1");
        assert_eq!(pairing.alternatives.len(), 1);
        assert_eq!(pairing.alternatives[0].task.id, "t2");
    }

    #[test]
    fn tasks_outside_the_copy_phases_are_ignored() {
        let source = [task("s1", "Train Staff", None, "Training", "Facility")];
        let target = [task("t1", "Train Staff", None, "Training", "Facility")];

        assert!(pair(&source, &target, None).is_empty());
    }

    #[test]
    fn the_scope_filter_keeps_only_corporate_or_only_facility_tasks() {
        let source = [
            task("s-corp", "Configure Fees", None, "Set Up", "Corporate"),
            task("s-fac", "Configure Gate", None, "Set Up", "Facility"),
        ];
        let target = [
            task("t-corp", "Configure Fees", None, "Set Up", "Corporate"),
            task("t-fac", "Configure Gate", None, "Set Up", "Facility"),
        ];

        let corporate = pair(&source, &target, Some(Scope::Corporate));
        assert_eq!(corporate.len(), 1);
        assert_eq!(target_id(&corporate, "s-corp"), Some("t-corp"));

        let facility = pair(&source, &target, Some(Scope::Facility));
        assert_eq!(facility.len(), 1);
        assert_eq!(target_id(&facility, "s-fac"), Some("t-fac"));

        assert_eq!(pair(&source, &target, None).len(), 2);
    }
}
