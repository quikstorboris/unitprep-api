//! The client's bulk copy: one comment copied from a task in a source
//! facility's list to the counterpart task in several other facilities'
//! lists at once.
//!
//! - `bulk-tasks`: the source list's tasks in every Onboarding Phase, and
//!   which other facilities could be destinations;
//! - `bulk-pairs`: for the chosen source task, each destination's
//!   suggested counterpart (and its tasks, to choose another by hand);
//! - `bulk-comment`: the source task's latest comment, to prefill;
//! - `bulk-copy`: posts the comment to the chosen destinations -- inside
//!   the request when it is small, otherwise as a background job the
//!   person is told about when it finishes (see `jobs`).
//!
//! Like the facility dialog it runs on the caller's own ClickUp token,
//! through that user's rate limiter, and posts the once-per-task pointer.

mod comment;
mod context;
mod copy;
mod pairs;
mod runner;
mod tasks;

pub use comment::bulk_comment;
pub use copy::bulk_copy;
pub use pairs::bulk_pairs;
pub use tasks::bulk_tasks;

#[cfg(test)]
pub use comment::BulkCommentQuery;
#[cfg(test)]
pub use copy::{BulkCopyRequest, BulkDestination};
#[cfg(test)]
pub use pairs::BulkPairsQuery;
#[cfg(test)]
pub use tasks::BulkTasksQuery;
