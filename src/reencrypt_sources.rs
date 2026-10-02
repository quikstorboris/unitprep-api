//! One-off `reencrypt-tool-run-sources` subcommand: encrypts the stored
//! source files of `client_ops.tool_runs` rows written before
//! `source_encrypted` existed (migration 20261002130000), which still hold
//! the plaintext upload. Idempotent -- it only touches rows still marked
//! `source_encrypted = false` that have bytes -- so it is safe to re-run,
//! and new rows are already encrypted when written.
//!
//! Like `bootstrap-admin` it connects with `BOOTSTRAP_DATABASE_URL` (the
//! owner/direct connection) so row-level security does not hide rows from
//! it, and needs `CLIENT_PII_ENCRYPTION_KEY` in the environment.

use sqlx::postgres::PgPoolOptions;

pub const USAGE_LINE: &str = "    unitprep reencrypt-tool-run-sources    encrypt stored dedup source files written before encryption existed";

pub async fn run() -> Result<String, String> {
    if !crate::clients::encryption::is_configured() {
        return Err(
            "CLIENT_PII_ENCRYPTION_KEY is not set (or is malformed); refusing to run".to_string(),
        );
    }

    let database_url = std::env::var("BOOTSTRAP_DATABASE_URL").map_err(|_| {
        "BOOTSTRAP_DATABASE_URL is not set. It must be the OWNER/direct connection string."
            .to_string()
    })?;

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .map_err(|err| format!("could not connect using BOOTSTRAP_DATABASE_URL: {err}"))?;

    let rows: Vec<(uuid::Uuid, String, Vec<u8>)> = sqlx::query_as(
        "SELECT id, session_id, source_bytes
           FROM client_ops.tool_runs
          WHERE source_encrypted = false AND source_bytes IS NOT NULL",
    )
    .fetch_all(&pool)
    .await
    .map_err(|err| format!("could not list rows to encrypt: {err}"))?;

    let mut done = 0usize;
    for (id, session_id, plain) in rows {
        let sealed = crate::client_ops::tool_runs::seal_source(&session_id, &plain, "");
        let Some(blob) = sealed.bytes else {
            return Err("encryption failed; nothing further was changed".to_string());
        };

        sqlx::query(
            "UPDATE client_ops.tool_runs
                SET source_bytes = $1, source_encrypted = true
              WHERE id = $2 AND source_encrypted = false",
        )
        .bind(blob)
        .bind(id)
        .execute(&pool)
        .await
        .map_err(|err| format!("could not update run {id}: {err}"))?;

        done += 1;
    }

    Ok(format!(
        "Encrypted the stored source file of {done} tool run(s)."
    ))
}
