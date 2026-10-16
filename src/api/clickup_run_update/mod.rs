//! Posting a finished tool run to the facility's ClickUp task: once the
//! run's results file is saved to the facility's Dropbox folder,
//! Orchestrator looks in the facility's linked ClickUp list for the task
//! that stands for this run, lets the person confirm which one, then
//! comments (the wording's last words, *here*, link the saved file), adds
//! the person as an assignee and sets the task to its list's complete
//! status.
//!
//! Which task a run stands for, and what the comment says, are rows of
//! `integrations.clickup_task_steps` (matched by the run's tool and its
//! position among the facility's runs of that tool: a duplicate check has
//! a 1st and a 2nd step, Unit Groups and the Template Tagger one each),
//! never Rust constants -- so a renamed task or new wording is a data
//! edit, and a new tool is a row plus the tool in `tool_runs`.
//!
//! Everything about the run is read from the database, never taken from
//! the browser: which step this is (the run's position among the
//! facility's runs of its tool), where its file was saved, and which list
//! the facility is linked to. The one thing the browser names is the task,
//! and that is re-read from ClickUp and refused unless it really lives
//! in the facility's linked list.
//!
//! All calls use the caller's own ClickUp token, so ClickUp's activity
//! history shows who did it.
use serde::{Deserialize, Serialize};

use crate::clickup::task_matching::StepDefinition;

mod candidates;
mod link;
mod post;
mod prepare;

pub use candidates::run_update_tasks;
pub use post::post_run_update;

const PERMISSION: &str = "integrations.clickup";

/// Everything the two endpoints need, read from the database.
struct Prepared {
    list_id: String,
    list_name: String,
    list_url: String,
    /// The run's own session id, whichever of its two ids the request used.
    session_id: String,
    sequence_number: i64,
    /// Where the summary file was saved in Dropbox, if it was.
    output_path: Option<String>,
    /// The share link captured when the file was saved, if it was ready.
    output_link: Option<String>,
    step: StepDefinition,
    /// The step's comment wording: the lead-in, the linked word, and the
    /// whole comment used when there is no file to link (the results were
    /// only downloaded, or nothing was saved yet -- the person adds the
    /// file to the task by hand).
    comment: CommentWording,
    /// The run position from which an update only adds a comment: the
    /// task is already assigned and complete.
    comment_only_from_sequence: i64,
    /// The run's tool (`dedup`, `unit_group`, `tagger`).
    tool: String,
}

struct CommentWording {
    lead: String,
    link_text: String,
    without_link: String,
}

impl Prepared {
    fn comment_only(&self) -> bool {
        self.sequence_number >= self.comment_only_from_sequence
    }
}

#[derive(Debug, Deserialize)]
pub struct CandidatesQuery {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct TaskCandidate {
    pub task_id: String,
    pub name: String,
    /// The task's parent, when it is a subtask -- the same step name
    /// repeats under several parents in a real list.
    pub parent_name: Option<String>,
    pub status: String,
    pub is_finished: bool,
    pub assignees: Vec<String>,
    pub url: String,
    pub score: f64,
}

#[derive(Debug, Serialize)]
pub struct CandidatesResponse {
    /// "1st Duplicate Check", "2nd Duplicate Check", "Unit Groups", ...
    pub step_label: String,
    pub sequence_number: i64,
    pub list_name: String,
    pub list_url: String,
    /// Whether the summary file is saved in Dropbox, so the comment can
    /// link it. When false the comment has no link and the person adds
    /// the file by hand.
    pub file_link_available: bool,
    /// Later runs only add a comment (no assignee, no status).
    pub comment_only: bool,
    pub candidates: Vec<TaskCandidate>,
}

#[derive(Debug, Deserialize)]
pub struct PostResultsRequest {
    pub session_id: String,
    pub task_id: String,
}

/// How one of the three ClickUp writes went.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    pub ok: bool,
    pub message: Option<String>,
}

impl StepOutcome {
    fn done() -> Self {
        Self {
            ok: true,
            message: None,
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: Some(message.into()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PostResultsResponse {
    pub task_name: String,
    pub task_url: String,
    /// What the comment links: `shared` (a Dropbox share link), `path`
    /// (Dropbox would not make one, so the file's plain Dropbox web path,
    /// which opens for anyone with the folder) or `none` (no Dropbox
    /// file; the person adds it by hand).
    pub link_kind: &'static str,
    pub comment: StepOutcome,
    /// `None` when not part of this update (later runs).
    pub assignee: Option<StepOutcome>,
    pub status: Option<StepOutcome>,
}
