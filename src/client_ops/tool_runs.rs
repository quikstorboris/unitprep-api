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
    /// The normalized tenant records the check ran on, kept (encrypted)
    /// so the run can be re-checked later with a different choice for
    /// tenants that had no customer id. See `seal_records`.
    ///
    /// Owned (not borrowed) because serializing and encrypting them is
    /// CPU-bound and runs on the blocking pool, which needs `'static` data.
    /// Callers have no use for the records once the run is recorded, so
    /// moving them in costs nothing.
    pub records: Vec<unitprep_dedup::TenantRecord>,
}

/// What `create_dedup_run` stores for the source file: ciphertext bound to
/// the run's `session_id` (so a blob cannot be moved to another run's row),
/// or -- when the encryption key is not configured -- nothing at all.
/// Never plaintext: the upload can carry card data and SSNs, and losing
/// the downloadable copy is the safer failure than storing it unprotected.
pub(crate) struct SealedSource {
    pub bytes: Option<Vec<u8>>,
    pub content_type: Option<String>,
    pub encrypted: bool,
}

pub(crate) fn seal_source(session_id: &str, bytes: &[u8], content_type: &str) -> SealedSource {
    match crate::clients::encryption::encrypt(&source_aad(session_id), bytes) {
        Ok(blob) => SealedSource {
            bytes: Some(blob),
            content_type: Some(content_type.to_string()),
            encrypted: true,
        },
        Err(err) => {
            tracing::error!(error = %err, session_id, "could not encrypt the tool run source file; the run is recorded without a stored copy");
            SealedSource {
                bytes: None,
                content_type: None,
                encrypted: false,
            }
        }
    }
}

/// Reverses `seal_source` for a stored row. Rows written before
/// `source_encrypted` existed hold the plaintext upload and pass through.
pub(crate) fn open_source(
    session_id: &str,
    stored: Vec<u8>,
    encrypted: bool,
) -> Result<Vec<u8>, String> {
    if !encrypted {
        return Ok(stored);
    }

    crate::clients::encryption::decrypt(&source_aad(session_id), &stored).map_err(|e| e.to_string())
}

fn source_aad(session_id: &str) -> Vec<u8> {
    format!("client_ops.tool_runs.source:{session_id}").into_bytes()
}

/// The records a run was computed from, as encrypted JSON bound to the
/// run's `session_id`. `None` (nothing stored) when the key is not
/// configured or serialization fails: the run is then recorded as usual
/// but cannot be rematched later.
pub(crate) fn seal_records(
    session_id: &str,
    records: &[unitprep_dedup::TenantRecord],
) -> Option<Vec<u8>> {
    let json = match serde_json::to_vec(records) {
        Ok(json) => json,
        Err(err) => {
            tracing::error!(error = %err, session_id, "could not serialize tool run records");
            return None;
        }
    };

    match crate::clients::encryption::encrypt(&records_aad(session_id), &json) {
        Ok(blob) => Some(blob),
        Err(err) => {
            tracing::error!(error = %err, session_id, "could not encrypt the tool run records; the run cannot be rematched later");
            None
        }
    }
}

pub(crate) fn open_records(
    session_id: &str,
    blob: &[u8],
) -> Result<Vec<unitprep_dedup::TenantRecord>, String> {
    let json = crate::clients::encryption::decrypt(&records_aad(session_id), blob)
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&json).map_err(|e| e.to_string())
}

fn records_aad(session_id: &str) -> Vec<u8> {
    format!("client_ops.tool_runs.records:{session_id}").into_bytes()
}

pub async fn create_dedup_run(db: &PgPool, run: ToolRunCreate<'_>) {
    // Encrypting the source file and serializing + encrypting every record
    // is CPU-bound (the records are the whole tenant list): do it first,
    // off the async workers, and BEFORE opening the transaction so a pooled
    // connection is not held through it.
    let session_id = run.session_id.to_string();
    let content_type = run.source_content_type.to_string();
    let source_bytes = run.source_bytes;
    let records = run.records;
    let (sealed, sealed_records) = match crate::blocking::spawn_blocking_in_span(move || {
        let sealed = seal_source(&session_id, &source_bytes, &content_type);
        let sealed_records = seal_records(&session_id, &records);
        (sealed, sealed_records)
    })
    .await
    {
        Ok(sealed) => sealed,
        Err(err) => {
            // Same degradation as a missing encryption key: the run is
            // still recorded, just without the stored copies.
            tracing::error!(error = %err, session_id = run.session_id, "tool run sealing task failed; the run is recorded without a stored copy or rematch records");
            (
                SealedSource {
                    bytes: None,
                    content_type: None,
                    encrypted: false,
                },
                None,
            )
        }
    };

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
              source_dropbox_path, source_bytes, source_content_type,
              source_encrypted, report_summary, records_encrypted)
         VALUES ('dedup', $1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(run.facility_id)
    .bind(run.session_id)
    .bind(run.actor_user_id)
    .bind(run.source_file_name)
    .bind(run.source_dropbox_path)
    .bind(&sealed.bytes)
    .bind(&sealed.content_type)
    .bind(sealed.encrypted)
    .bind(&run.report_summary)
    .bind(&sealed_records)
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

/// One run of a tool other than Dedup (Unit Groups, Template Tagger) for
/// `record_run`. Those tools have no stored source file or kept records;
/// the row carries the report summary and, later, the output file.
pub struct RunRecord<'a> {
    pub tool: &'a str,
    pub facility_id: Uuid,
    pub session_id: &'a str,
    pub actor_user_id: Uuid,
    pub role_keys: &'a [String],
    pub source_file_name: &'a str,
    pub report_summary: Value,
}

/// Records a run, or updates it when this session already has one (the
/// Unit Groups analysis can run again after a correction; the newest
/// result replaces the earlier one on the same row). Infallible from the
/// caller's side, like the other writers here.
pub async fn record_run(db: &PgPool, run: RunRecord<'_>) {
    let mut tx = match begin_rls_transaction(db, run.actor_user_id, run.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, facility_id = %run.facility_id, "failed to open transaction for record_run");
            return;
        }
    };

    let result = sqlx::query(
        "INSERT INTO client_ops.tool_runs
             (tool, facility_id, session_id, actor_user_id, source_file_name, report_summary)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (session_id) DO UPDATE
            SET report_summary = EXCLUDED.report_summary,
                source_file_name = EXCLUDED.source_file_name",
    )
    .bind(run.tool)
    .bind(run.facility_id)
    .bind(run.session_id)
    .bind(run.actor_user_id)
    .bind(run.source_file_name)
    .bind(&run.report_summary)
    .execute(&mut *tx)
    .await;

    if let Err(err) = result {
        tracing::error!(
            error = %err,
            tool = run.tool,
            facility_id = %run.facility_id,
            session_id = run.session_id,
            "failed to record client_ops.tool_runs row"
        );
        return;
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, session_id = run.session_id, "failed to commit record_run");
    }
}

/// Adds `patch`'s keys to a run's stored summary (existing keys it names
/// are replaced). For facts that only exist after the check, such as how
/// many substitutions a tagger run actually applied.
pub async fn merge_report_summary(
    db: &PgPool,
    actor_user_id: Uuid,
    role_keys: &[String],
    session_id: &str,
    patch: &Value,
) {
    let mut tx = match begin_rls_transaction(db, actor_user_id, role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, session_id, "failed to open transaction for merge_report_summary");
            return;
        }
    };

    let result = sqlx::query(
        "UPDATE client_ops.tool_runs
            SET report_summary = report_summary || $2::jsonb
          WHERE session_id = $1",
    )
    .bind(session_id)
    .bind(patch)
    .execute(&mut *tx)
    .await;

    if let Err(err) = result {
        tracing::error!(error = %err, session_id, "failed to merge into the tool run report");
        return;
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, session_id, "failed to commit merge_report_summary");
    }
}

/// Replaces a run's stored report (the on-screen summary) after the user
/// re-checked it, and -- when the run has a stored output file -- the
/// regenerated file too, so the downloadable workbook matches the report.
/// Infallible from the caller's side, like the other writers here.
pub async fn update_report(
    db: &PgPool,
    actor_user_id: Uuid,
    role_keys: &[String],
    session_id: &str,
    report_summary: &Value,
    output: Option<OutputFile<'_>>,
) {
    let mut tx = match begin_rls_transaction(db, actor_user_id, role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, session_id, "failed to open transaction for update_report");
            return;
        }
    };

    let result = match output {
        Some(file) => {
            sqlx::query(
                "UPDATE client_ops.tool_runs
                    SET report_summary = $2, output_bytes = $3,
                        output_content_type = $4, output_file_name = $5
                  WHERE session_id = $1",
            )
            .bind(session_id)
            .bind(report_summary)
            .bind(file.bytes)
            .bind(file.content_type)
            .bind(file.file_name)
            .execute(&mut *tx)
            .await
        }
        None => {
            sqlx::query("UPDATE client_ops.tool_runs SET report_summary = $2 WHERE session_id = $1")
                .bind(session_id)
                .bind(report_summary)
                .execute(&mut *tx)
                .await
        }
    };

    if let Err(err) = result {
        tracing::error!(error = %err, session_id, "failed to update the tool run report");
        return;
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, session_id, "failed to commit update_report");
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

/// The exported file itself, as kept in the database next to its Dropbox path.
pub struct OutputFile<'a> {
    pub bytes: Vec<u8>,
    pub content_type: &'a str,
    pub file_name: &'a str,
}

/// Stores the Dropbox share link for the file saved at `dropbox_path`.
/// Matches on the path too, so a link made for an earlier file is never
/// attached to a newer save of the same run. Best-effort: a failure only
/// means the link is made again when it is next needed.
pub async fn store_output_dropbox_link(
    db: &PgPool,
    actor_user_id: Uuid,
    role_keys: &[String],
    session_id: &str,
    dropbox_path: &str,
    link: &str,
) {
    let mut tx = match begin_rls_transaction(db, actor_user_id, role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::warn!(error = %err, session_id, "failed to open transaction to store a Dropbox share link");
            return;
        }
    };

    let result = sqlx::query(
        "UPDATE client_ops.tool_runs SET output_dropbox_link = $3
          WHERE session_id = $1 AND output_dropbox_path = $2",
    )
    .bind(session_id)
    .bind(dropbox_path)
    .bind(link)
    .execute(&mut *tx)
    .await;

    match result {
        Ok(_) => {
            if let Err(err) = tx.commit().await {
                tracing::warn!(error = %err, session_id, "failed to commit a stored Dropbox share link");
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, session_id, "failed to store a Dropbox share link");
        }
    }
}

/// Records that the run's output was saved to Dropbox AND keeps a copy of
/// the same file in the database, so it stays downloadable from the
/// Onboarding Work tab even if the Dropbox file is later moved or deleted.
pub async fn attach_output_dropbox(
    db: &PgPool,
    actor_user_id: Uuid,
    role_keys: &[String],
    session_id: &str,
    dropbox_path: &str,
    file: OutputFile<'_>,
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
            SET output_dropbox_path = $2, output_dropbox_link = NULL, output_bytes = $3,
                output_content_type = $4, output_file_name = $5,
                completed_at = now()
          WHERE session_id = $1",
    )
    .bind(session_id)
    .bind(dropbox_path)
    .bind(file.bytes)
    .bind(file.content_type)
    .bind(file.file_name)
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

        let db = crate::db::connect_test();

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

        let db = crate::db::connect_test();

        attach_output_dropbox(
            &db,
            Uuid::new_v4(),
            &[],
            "no-such-session-id",
            "/Some/Path/out.csv",
            OutputFile {
                bytes: b"x".to_vec(),
                content_type: "text/csv",
                file_name: "out.csv",
            },
        )
        .await;
    }

    #[test]
    #[serial_test::serial(client_pii_encryption_key_env)]
    fn a_sealed_source_is_not_the_plaintext_and_opens_back_to_it() {
        std::env::set_var(
            "CLIENT_PII_ENCRYPTION_KEY",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        let plain = b"sCreditCardNum,TenantID
abc,1
";
        let sealed = seal_source("run-1", plain, "text/csv");
        std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");

        assert!(sealed.encrypted);
        let blob = sealed.bytes.clone().expect("stored");
        assert_ne!(blob, plain.to_vec());
        assert!(!blob.windows(plain.len()).any(|w| w == plain));
        assert_eq!(sealed.content_type.as_deref(), Some("text/csv"));

        std::env::set_var(
            "CLIENT_PII_ENCRYPTION_KEY",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert_eq!(open_source("run-1", blob.clone(), true).unwrap(), plain);
        assert!(
            open_source("run-2", blob, true).is_err(),
            "a blob must not open under another run's session id"
        );
        std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");
    }

    #[test]
    #[serial_test::serial(client_pii_encryption_key_env)]
    fn without_a_key_no_plaintext_is_stored() {
        std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");
        let sealed = seal_source("run-1", b"secret", "text/csv");
        assert!(sealed.bytes.is_none() && sealed.content_type.is_none() && !sealed.encrypted);
    }

    #[test]
    fn a_row_written_before_encryption_existed_passes_through() {
        assert_eq!(
            open_source("run-1", b"old".to_vec(), false).unwrap(),
            b"old"
        );
    }
}
