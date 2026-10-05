//! Real-database test for `client_ops::tool_runs::create_dedup_run`: the
//! source file and the kept records are encrypted off the async workers
//! (on the blocking pool) BEFORE the transaction opens, and must still land
//! in the row exactly as they did when sealing ran inline -- ciphertext
//! that opens back to the original bytes under the run's own session id.
//!
//! `#[ignore]`d like the other real-DB tests: needs the local `test-db`
//! (see `clickup_db_tests.rs` for the fixture helpers and run command),
//! never Neon:
//!
//! ```text
//! TEST_DATABASE_URL=postgres://app_service:app_service@127.0.0.1:5433/unitprep_test \
//!   cargo test --bin unitprep -- --ignored tool_run_create_db
//! ```

use serial_test::serial;
use uuid::Uuid;

use unitprep_dedup::TenantRecord;

use super::clickup_db_tests::{create_user, superuser_pool};
use crate::client_ops::tool_runs::{self, ToolRunCreate};

const KEY_ENV: &str = "CLIENT_PII_ENCRYPTION_KEY";
const KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::test]
#[serial(client_pii_encryption_key_env)]
#[ignore = "needs the local test-db -- see module doc"]
async fn tool_run_create_db_stores_the_source_and_records_encrypted_and_recoverable() {
    let _ = dotenvy::from_filename(".env.local");
    std::env::set_var(KEY_ENV, KEY);

    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    // A facility for the run to belong to (FK), and a real user as actor.
    let company_id: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.companies (legal_name, source) VALUES ('Sealing Co', 'manual') RETURNING id",
    )
    .fetch_one(&superuser)
    .await
    .unwrap();
    let facility_id: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.facilities (company_id, name, source)
         VALUES ($1, 'Sealing Facility', 'manual') RETURNING id",
    )
    .bind(company_id)
    .fetch_one(&superuser)
    .await
    .unwrap();
    let actor = create_user(&superuser, "sealing").await;

    let session_id = format!("seal-test-{}", Uuid::new_v4());
    let source = b"sCreditCardNum,TenantID\n4111111111111111,1\n".to_vec();
    let records = vec![TenantRecord {
        first_last: "Jane Doe".to_string(),
        ..Default::default()
    }];

    tool_runs::create_dedup_run(
        &app,
        ToolRunCreate {
            facility_id,
            session_id: &session_id,
            actor_user_id: actor,
            role_keys: &["admin".to_string()],
            source_file_name: "rent_roll.csv",
            source_dropbox_path: None,
            source_bytes: source.clone(),
            source_content_type: "text/csv",
            report_summary: serde_json::json!({}),
            records: records.clone(),
        },
    )
    .await;

    #[allow(clippy::type_complexity)]
    let row: (Option<Vec<u8>>, bool, Option<String>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT source_bytes, source_encrypted, source_content_type, records_encrypted
           FROM client_ops.tool_runs WHERE session_id = $1",
    )
    .bind(&session_id)
    .fetch_one(&superuser)
    .await
    .expect("the run row must have been inserted");
    let (stored_source, source_encrypted, content_type, stored_records) = row;

    let stored_source = stored_source.expect("a source copy must be stored when the key is set");
    assert!(
        source_encrypted,
        "the stored copy must be flagged encrypted"
    );
    assert_eq!(content_type.as_deref(), Some("text/csv"));
    assert!(
        !stored_source.windows(source.len()).any(|w| w == source),
        "the stored source must never be the plaintext (it can carry card numbers)"
    );
    assert_eq!(
        tool_runs::open_source(&session_id, stored_source, true).unwrap(),
        source,
        "the stored source must open back to the original under this run's session id"
    );

    let stored_records = stored_records.expect("records must be kept when the key is set");
    let reopened = tool_runs::open_records(&session_id, &stored_records).unwrap();
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].first_last, "Jane Doe");

    // Without a key the run is still recorded, but with no stored copy.
    std::env::remove_var(KEY_ENV);
    let keyless_session = format!("seal-test-{}", Uuid::new_v4());
    tool_runs::create_dedup_run(
        &app,
        ToolRunCreate {
            facility_id,
            session_id: &keyless_session,
            actor_user_id: actor,
            role_keys: &["admin".to_string()],
            source_file_name: "rent_roll.csv",
            source_dropbox_path: None,
            source_bytes: source,
            source_content_type: "text/csv",
            report_summary: serde_json::json!({}),
            records,
        },
    )
    .await;
    let keyless: (Option<Vec<u8>>, bool, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT source_bytes, source_encrypted, records_encrypted
           FROM client_ops.tool_runs WHERE session_id = $1",
    )
    .bind(&keyless_session)
    .fetch_one(&superuser)
    .await
    .expect("a keyless run is still recorded");
    assert!(keyless.0.is_none(), "no plaintext copy without a key");
    assert!(!keyless.1);
    assert!(keyless.2.is_none(), "no records blob without a key");
}
