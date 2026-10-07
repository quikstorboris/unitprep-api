//! ClickUp Copy: copying a comment from a task in one facility's ClickUp
//! list to its counterpart task in another facility's list.
//!
//! The facility dialog's endpoints are on the **target** facility (the
//! page the person is on), with the **source** facility chosen per request
//! and defaulting to the company's designated parent:
//!
//! - `pairs`: the source and target lists' tasks in the Set Up and
//!   Migration phases, paired by name/phase/parent (suggestions only);
//! - `comments`: one row's source comment (the prefill) and whether the
//!   target looks as if it already has it -- fetched per row so the
//!   dialog does not read every task's comments up front;
//! - `copy`: posts the (edited) comments, plus -- once per target task --
//!   a generic pointer comment naming the company's main list.
//!
//! Orchestrator stores nothing about what was copied: ClickUp is the
//! record, and the "already copied" / "pointer already there" checks read
//! the target task's comments (see `clickup::copy_text`). Everything runs
//! with the caller's own ClickUp token, so ClickUp attributes the
//! comments to the person who clicked, and every ClickUp call goes through
//! that user's rate limiter (`exec`).

//!
//! The client's bulk copy (`bulk`) copies one comment to many facilities,
//! and runs as a background job (`jobs`) when it is too big to finish
//! inside the request.

mod bulk;
mod comments;
mod copy;
mod exec;
mod jobs;
mod lists;
mod pairs;

pub use bulk::{bulk_comment, bulk_copy, bulk_pairs, bulk_tasks};
pub use comments::copy_comments;
pub use copy::copy_comments_to_tasks;
pub use jobs::{get_copy_job, list_copy_jobs};
pub use pairs::copy_pairs;

// The request types are built only by the endpoint tests (axum builds them
// from the HTTP request in the real server).
#[cfg(test)]
pub use bulk::{
    BulkCommentQuery, BulkCopyRequest, BulkDestination, BulkPairsQuery, BulkTasksQuery,
};
#[cfg(test)]
pub use comments::CommentsQuery;
#[cfg(test)]
pub use copy::{CopyItem, CopyRequest};
#[cfg(test)]
pub use pairs::PairsQuery;
