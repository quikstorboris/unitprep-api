//! Reading the facility's linked list, the run and the step definition.

use axum::response::Response;
use uuid::Uuid;

use super::{CommentWording, Prepared};
use crate::api::tool_runs::facility_belongs_to_company;
use crate::api::{conflict, internal_error, not_found, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clickup::task_matching::StepDefinition;

fn server_error(context: &'static str, err: &sqlx::Error, user: &AuthenticatedUser) -> Response {
    tracing::error!(error = %err, user_id = %user.user_id, "{context}");
    internal_error("Could not look up this run for ClickUp")
}

/// Reads the facility's linked list, the run and the step definition, or
/// the response to return instead.
pub(super) async fn prepare(
    state: &AppState,
    user: &AuthenticatedUser,
    company_id: Uuid,
    facility_id: Uuid,
    session_id: &str,
) -> Result<Prepared, Response> {
    let fail = |err: sqlx::Error| server_error("ClickUp run lookup failed", &err, user);

    let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys)
        .await
        .map_err(fail)?;

    if !facility_belongs_to_company(&mut tx, facility_id, company_id)
        .await
        .map_err(fail)?
    {
        let _ = tx.commit().await;
        return Err(not_found("not_found", "No such facility.".to_string()));
    }

    let link: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT clickup_list_id, clickup_list_name, clickup_list_url
           FROM clients.facilities WHERE id = $1",
    )
    .bind(facility_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(fail)?;

    // `session_id` may be the run's own session id (right after a check) or
    // the run's row id (what the Onboarding Work tab lists), so a check can
    // be posted later from either place.
    let run: Option<RunRow> = sqlx::query_as(
        "SELECT tool, sequence_number, session_id, output_dropbox_path, output_dropbox_link FROM (
             SELECT id, tool, session_id, output_dropbox_path, output_dropbox_link,
                    ROW_NUMBER() OVER (PARTITION BY facility_id, tool ORDER BY created_at ASC) AS sequence_number
               FROM client_ops.tool_runs
              WHERE facility_id = $1
         ) runs WHERE session_id = $2 OR id::text = $2",
    )
    .bind(facility_id)
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(fail)?;

    // The run's step is the tool's step with the highest ordinal not above
    // the run's position (a 3rd duplicate check uses the 2nd check's step).
    let step: Option<StepRow> = match &run {
        Some((tool, sequence_number, ..)) => sqlx::query_as(
            "SELECT step_key, label, ordinal, phrases, comment_lead, comment_link_text,
                    comment_without_link, comment_only_from_sequence
               FROM integrations.clickup_task_steps
              WHERE tool = $1 AND ordinal <= $2
              ORDER BY ordinal DESC LIMIT 1",
        )
        .bind(tool)
        .bind(*sequence_number as i32)
        .fetch_optional(&mut *tx)
        .await
        .map_err(fail)?,
        None => None,
    };

    tx.commit().await.map_err(fail)?;

    let (Some(list_id), Some(list_name), Some(list_url)) = link else {
        return Err(conflict(
            "facility_not_linked_to_clickup",
            "Link this facility to its ClickUp list first (the ClickUp section on its General tab)."
                .to_string(),
        ));
    };

    let Some((tool, sequence_number, session_id, output_path, output_link)) = run else {
        return Err(not_found(
            "not_found",
            "No run with that session was recorded for this facility.".to_string(),
        ));
    };

    let Some((
        step_key,
        label,
        ordinal,
        phrases,
        comment_lead,
        comment_link_text,
        comment_without_link,
        comment_only_from_sequence,
    )) = step
    else {
        tracing::warn!(tool, "no ClickUp task step is configured for this tool");
        return Err(conflict(
            "clickup_step_not_configured",
            "No ClickUp task is configured for this kind of run yet.".to_string(),
        ));
    };

    Ok(Prepared {
        list_id,
        list_name,
        list_url,
        session_id,
        sequence_number,
        output_path,
        output_link,
        step: StepDefinition {
            step_key,
            label,
            ordinal,
            phrases,
        },
        comment: CommentWording {
            lead: comment_lead,
            link_text: comment_link_text,
            without_link: comment_without_link,
        },
        comment_only_from_sequence: i64::from(comment_only_from_sequence),
        tool,
    })
}

/// One `integrations.clickup_task_steps` row: key, label, ordinal, task-name
/// phrases, comment lead-in, linked word, no-link comment, and the run
/// position from which only a comment is added.
/// One tool run: tool, position among the facility's runs of it, session id,
/// saved Dropbox path and its captured share link.
type RunRow = (String, i64, String, Option<String>, Option<String>);

type StepRow = (
    String,
    String,
    i32,
    Vec<String>,
    String,
    String,
    String,
    i32,
);
