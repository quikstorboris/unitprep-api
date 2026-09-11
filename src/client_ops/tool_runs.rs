//! Durable, per-facility record of a tool run -- starting with Dedup
//! only (`tool = 'dedup'`; Unit Groups/Template Tagger will add their
//! own `tool` value once they get wired up the same way, not before).
//!
//! Distinct from `client_ops::audit_log`, which stays exactly as it was
//! (a generic, facility-blind append-only event note fired on export):
//! this table is a real, queryable row per run -- created the moment a
//! check succeeds, later updated in place with output info once (if
//! ever) the user exports -- that the new Onboarding Work tab on the
//! facility page lists and joins against, not just an event trail.
//!
//! **Every writer here opens its own `begin_rls_transaction`, even
//! `create_dedup_run`'s plain INSERT which doesn't strictly need one.**
//! This is not optional for the two `attach_output_*` UPDATEs: Postgres
//! RLS requires an updated row to satisfy *both* the UPDATE policy's
//! USING clause and the table's SELECT policy's USING clause (see
//! "Row Security Policies" in the Postgres manual) -- and
//! `tool_runs_select_authenticated`'s policy depends on the
//! `app.current_user_id` GUC. A bare `.execute(db)` against the raw
//! pool never sets that GUC, so every UPDATE here silently affected
//! zero rows (confirmed live, 2026-09-10: a real check's row inserted
//! fine -- INSERT's own policy is unconditional -- but its later export
//! never attached output, with no visible error anywhere, because
//! `execute()` returning `Ok` with `rows_affected() == 0` isn't an
//! error). Both writers are otherwise infallible from the caller's
//! point of view, same reasoning as `audit_log::record`: a persistence
//! hiccup must never turn into a broken tool run.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::begin_rls_transaction;

pub struct ToolRunCreate<'a> {
    pub facility_id: Uuid,
    pub session_id: &'a str,
    pub actor_user_id: Uuid,
    pub role_keys: &'a [String],
    pub source_file_name: &'a str,
    pub source_dropbox_path: Option<&'a str>,
    /// The original uploaded/imported file's own bytes -- stored
    /// alongside `source_dropbox_path` (not instead of it) because a
    /// Dropbox-sourced file can later be moved, renamed, or deleted out
    /// from under that path; this is the one copy Onboarding Work can
    /// always still hand back regardless.
    pub source_bytes: Vec<u8>,
    pub source_content_type: &'a str,
    pub report_summary: Value,
}

pub async fn create_dedup_run(db: &PgPool, run: ToolRunCreate<'_>) {
    let mut tx = match begin_rls_transaction(db, run.actor_user_id, run.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, facility_id = %run.facility_id, "failed to open transaction for tool_runs insert");
            return;
        }
    };

    let result = sqlx::query(
        "INSERT INTO client_ops.tool_runs
             (tool, facility_id, session_id, actor_user_id, source_file_name,
              source_dropbox_path, source_bytes, source_content_type, report_summary)
         VALUES ('dedup', $1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(run.facility_id)
    .bind(run.session_id)
    .bind(run.actor_user_id)
    .bind(run.source_file_name)
    .bind(run.source_dropbox_path)
    .bind(&run.source_bytes)
    .bind(run.source_content_type)
    .bind(&run.report_summary)
    .execute(&mut *tx)
    .await;

    if let Err(err) = result {
        tracing::error!(
            error = %err,
            facility_id = %run.facility_id,
            session_id = run.session_id,
            "failed to create client_ops.tool_runs row"
        );
        return;
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, session_id = run.session_id, "failed to commit tool_runs insert");
    }
}

pub async fn attach_output_bytes(
    db: &PgPool,
    actor_user_id: Uuid,
    role_keys: &[String],
    session_id: &str,
    bytes: Vec<u8>,
    content_type: &str,
    file_name: &str,
) {
    let mut tx = match begin_rls_transaction(db, actor_user_id, role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, session_id, "failed to open transaction for attach_output_bytes");
            return;
        }
    };

    let result = sqlx::query(
        "UPDATE client_ops.tool_runs
            SET output_bytes = $2, output_content_type = $3, output_file_name = $4,
                completed_at = now()
          WHERE session_id = $1",
    )
    .bind(session_id)
    .bind(bytes)
    .bind(content_type)
    .bind(file_name)
    .execute(&mut *tx)
    .await;

    match result {
        Ok(outcome) if outcome.rows_affected() == 0 => {
            tracing::warn!(
                session_id,
                "attach_output_bytes affected no rows -- no matching tool_runs row for this session"
            );
        }
        Err(err) => {
            tracing::error!(error = %err, session_id, "failed to attach output bytes to tool_runs row");
            return;
        }
        Ok(_) => {}
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, session_id, "failed to commit attach_output_bytes");
    }
}

pub async fn attach_output_dropbox(
    db: &PgPool,
    actor_user_id: Uuid,
    role_keys: &[String],
    session_id: &str,
    dropbox_path: &str,
) {
    let mut tx = match begin_rls_transaction(db, actor_user_id, role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, session_id, "failed to open transaction for attach_output_dropbox");
            return;
        }
    };

    let result = sqlx::query(
        "UPDATE client_ops.tool_runs
            SET output_dropbox_path = $2, completed_at = now()
          WHERE session_id = $1",
    )
    .bind(session_id)
    .bind(dropbox_path)
    .execute(&mut *tx)
    .await;

    match result {
        Ok(outcome) if outcome.rows_affected() == 0 => {
            tracing::warn!(
                session_id,
                "attach_output_dropbox affected no rows -- no matching tool_runs row for this session"
            );
        }
        Err(err) => {
            tracing::error!(error = %err, session_id, "failed to attach output dropbox path to tool_runs row");
            return;
        }
        Ok(_) => {}
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, session_id, "failed to commit attach_output_dropbox");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the exact live incident this module's own doc
    /// comment describes (2026-09-10): `attach_output_bytes`/
    /// `attach_output_dropbox` ran their UPDATE against the raw pool with
    /// no RLS GUCs set, so `rows_affected()` was silently always 0 --
    /// `empty_state()`'s unreachable pool can't catch this class of bug
    /// (it fails on connection, never on a real query against real RLS
    /// policies), so this needs a real, reachable Postgres with every
    /// migration applied, same reasoning and same opt-in as
    /// `authenticated_user`'s own `query_sessions_own_sql_is_valid_
    /// against_the_real_schema`. A random `session_id` matches no real
    /// row -- `rows_affected() == 0` is the *expected*, safe outcome
    /// here (nothing to attach to), so this needs no fixture and leaves
    /// no data behind; what it actually proves is that the transaction
    /// opens and the UPDATE's own SQL is valid against the real schema
    /// and RLS policies, not blocked by them.
    ///
    /// Run explicitly with `cargo test -- --ignored attach_output`.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    async fn attach_output_bytes_runs_cleanly_against_the_real_schema() {
        let _ = dotenvy::from_filename(".env.local");

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");

        attach_output_bytes(
            &db,
            Uuid::new_v4(),
            &[],
            "no-such-session-id",
            b"probe".to_vec(),
            "text/csv",
            "probe.csv",
        )
        .await;

        // No panic and no error logged above is the pass condition for
        // this test -- there's no return value to assert on, since
        // attach_output_bytes is infallible-from-the-caller by design.
    }

    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    async fn attach_output_dropbox_runs_cleanly_against_the_real_schema() {
        let _ = dotenvy::from_filename(".env.local");

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");

        attach_output_dropbox(&db, Uuid::new_v4(), &[], "no-such-session-id", "/Some/Path/out.csv").await;
    }
}
