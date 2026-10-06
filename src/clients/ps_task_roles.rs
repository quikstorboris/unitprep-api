//! Which Process Street task names mean what, as data.
//!
//! The Elavon tab, the Onboarding Summary and the facility views all
//! need to know "has this run's credentials step happened" -- but the
//! step's *name* is Process Street's, not ours, and it changed: a
//! 2026-10 template change added "Document Credentials" and left the old
//! "Add Credentials to QMS" task in place but hidden (PS conditional
//! logic), so new runs and old runs name the same step differently.
//! Rather than a hardcoded string (it was one, in five places), code asks
//! for a *role* (`QMS_CREDENTIALS_ROLE`) and this module resolves it
//! through `integrations.ps_task_role_name`, an admin-editable table
//! (Process Street settings page, "Task mapping"). A role can map to
//! several names so old and new runs both resolve, and the next rename
//! is a data edit.
//!
//! Hidden tasks (`Task::hidden` / `ps_task_status.hidden`) never
//! participate: a task the coordinator can't see is not a step anybody
//! is waiting on.

use sqlx::PgConnection;

use crate::process_street::Task;

/// "A coordinator has gotten this facility's QMS credentials in" -- the
/// step the Elavon tab's `credentials_added_to_qms` flag and the
/// Onboarding Summary's "awaiting credentials" cap both key on.
pub const QMS_CREDENTIALS_ROLE: &str = "qms_credentials";

/// A role the settings page can edit. Roles are fixed here because code
/// has to know what each one *means*; the task names under them are the
/// data.
pub struct KnownRole {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

pub const KNOWN_ROLES: &[KnownRole] = &[KnownRole {
    key: QMS_CREDENTIALS_ROLE,
    label: "Credentials added to QMS",
    description: "The Merchant Account checklist step that marks a facility's Elavon/QMS \
         credentials as in hand. Drives the Elavon tab's \"credentials added\" status and the \
         Onboarding Summary's \"awaiting credentials\" step.",
}];

/// The task names currently mapped to `role`, in the order they were added.
///
/// Goes through whatever connection the caller already has (an RLS
/// transaction in every real caller) because this table's SELECT policy
/// requires `app.current_user_id` -- a bare pool query would return zero
/// rows, which reads as "no task matches" rather than a wiring error.
pub async fn load_task_names(
    conn: &mut PgConnection,
    role: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT task_name FROM integrations.ps_task_role_name WHERE role = $1 ORDER BY created_at, id",
    )
    .bind(role)
    .fetch_all(conn)
    .await
}

/// `load_task_names` in its own short RLS transaction, for the handlers
/// that fetch from Process Street before they open their write
/// transaction and so have no connection to borrow yet.
pub async fn load_task_names_as(
    db: &sqlx::PgPool,
    user_id: uuid::Uuid,
    role_keys: &[String],
    role: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let mut tx = crate::auth::begin_rls_transaction(db, user_id, role_keys).await?;
    let names = load_task_names(&mut tx, role).await?;
    tx.commit().await?;
    Ok(names)
}

fn name_matches(task_name: &str, names: &[String]) -> bool {
    let task_name = task_name.trim();
    names
        .iter()
        .any(|name| name.trim().eq_ignore_ascii_case(task_name))
}

/// True when the run has at least one *visible* task mapped to the role
/// and every such task is Completed. "Every", not "any": if a template
/// ever shows both the old and the new step, the old one being done
/// doesn't mean the new one is.
pub fn role_is_satisfied(tasks: &[Task], names: &[String]) -> bool {
    let mut matched = tasks
        .iter()
        .filter(|task| !task.hidden && name_matches(&task.name, names))
        .peekable();
    matched.peek().is_some() && matched.all(|task| task.status == "Completed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(name: &str, status: &str, hidden: bool) -> Task {
        Task {
            id: "t1".to_string(),
            name: name.to_string(),
            status: status.to_string(),
            hidden,
        }
    }

    fn names() -> Vec<String> {
        vec![
            "Document Credentials".to_string(),
            "Add Credentials to QMS".to_string(),
        ]
    }

    #[test]
    fn old_template_run_resolves_through_the_old_name() {
        let tasks = vec![task("Add Credentials to QMS", "Completed", false)];
        assert!(role_is_satisfied(&tasks, &names()));
    }

    #[test]
    fn new_template_run_resolves_through_the_new_name() {
        let tasks = vec![
            task("Document Credentials", "Completed", false),
            task("Add Credentials to QMS", "NotCompleted", true),
        ];
        assert!(role_is_satisfied(&tasks, &names()));
    }

    #[test]
    fn hidden_task_never_counts_even_when_completed() {
        let tasks = vec![task("Add Credentials to QMS", "Completed", true)];
        assert!(!role_is_satisfied(&tasks, &names()));
    }

    #[test]
    fn incomplete_visible_task_is_not_satisfied() {
        let tasks = vec![
            task("Document Credentials", "NotCompleted", false),
            task("Add Credentials to QMS", "NotCompleted", true),
        ];
        assert!(!role_is_satisfied(&tasks, &names()));
    }

    #[test]
    fn every_visible_mapped_task_must_be_completed() {
        let tasks = vec![
            task("Document Credentials", "Completed", false),
            task("Add Credentials to QMS", "NotCompleted", false),
        ];
        assert!(!role_is_satisfied(&tasks, &names()));
    }

    #[test]
    fn run_with_no_mapped_task_is_not_satisfied() {
        let tasks = vec![task("Twilio Information", "Completed", false)];
        assert!(!role_is_satisfied(&tasks, &names()));
    }

    #[test]
    fn matching_ignores_case_and_surrounding_whitespace() {
        let tasks = vec![task("  document credentials ", "Completed", false)];
        assert!(role_is_satisfied(&tasks, &names()));
    }

    #[test]
    fn empty_mapping_matches_nothing() {
        let tasks = vec![task("Document Credentials", "Completed", false)];
        assert!(!role_is_satisfied(&tasks, &[]));
    }
}
