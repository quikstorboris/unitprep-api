//! Tests of the batched fetch-then-write pipeline (`sync_workflow_runs`).
//! The pure ones need nothing; the `sync_db_*` ones are `#[ignore]`d real-DB
//! tests (local `test-db` only -- see `api::clickup_db_tests`' module doc for
//! the run command) that drive the WHOLE pipeline with a fake `fetch`, so
//! no Process Street is involved.

use std::sync::atomic::{AtomicUsize, Ordering};

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::begin_rls_transaction;
use crate::clients::person_index::extract_merchant_account_people;
use crate::process_street::{FormField, ProcessStreetError, WorkflowRun};

use super::super::progress::SyncError;
use super::*;

fn run(id: &str, updated: &str) -> WorkflowRun {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": format!("Run {id}"),
        "status": "Active",
        "workflowId": "wf",
        "audit": { "updatedDate": updated },
    }))
    .unwrap()
}

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn text_field(key: &str, value: &str) -> FormField {
    serde_json::from_value(serde_json::json!({
        "id": "test", "taskId": "test", "key": key, "label": key,
        "fieldType": "Text", "data": {"value": value}
    }))
    .unwrap()
}

/// A Merchant Account run's fields: one listed owner, one distinct
/// signer (so two people), and a `Business_DBA`.
fn merchant_account_fields(run_id: &str) -> Vec<FormField> {
    vec![
        text_field("Owner_1_-_First_Name", "Owner"),
        text_field("Owner_1_-_Last_Name", run_id),
        text_field("Signer_Name", &format!("Signer {run_id}")),
        text_field("Business_DBA", &format!("DBA {run_id}")),
    ]
}

#[test]
fn only_new_or_moved_runs_need_a_refresh_and_an_unchanged_one_does_not() {
    let runs = vec![
        run("new", "2026-03-01T00:00:00Z"),
        run("moved", "2026-03-01T00:00:00Z"),
        run("same", "2026-02-01T00:00:00Z"),
        run("older-in-ps", "2026-01-01T00:00:00Z"),
    ];
    let existing: HashMap<String, DateTime<Utc>> = [
        ("moved".to_string(), at("2026-02-01T00:00:00Z")),
        ("same".to_string(), at("2026-02-01T00:00:00Z")),
        ("older-in-ps".to_string(), at("2026-02-01T00:00:00Z")),
    ]
    .into_iter()
    .collect();

    let ids: Vec<&str> = runs_needing_refresh(&runs, &existing, false)
        .iter()
        .map(|r| r.id.as_str())
        .collect();

    assert_eq!(ids, vec!["new", "moved"]);
}

#[test]
fn force_refreshes_every_run_and_a_repeated_run_id_only_once() {
    let runs = vec![
        run("a", "2026-02-01T00:00:00Z"),
        run("b", "2026-02-01T00:00:00Z"),
        run("a", "2026-02-01T00:00:00Z"),
    ];
    let existing: HashMap<String, DateTime<Utc>> = [
        ("a".to_string(), at("2026-02-01T00:00:00Z")),
        ("b".to_string(), at("2026-02-01T00:00:00Z")),
    ]
    .into_iter()
    .collect();

    assert!(runs_needing_refresh(&runs, &existing, false).is_empty());

    let ids: Vec<&str> = runs_needing_refresh(&runs, &existing, true)
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["a", "b"],
        "forced: everything, but a repeated id once (two rows with one key in a single batched upsert would be an error)"
    );
}

async fn index_rows(db: &PgPool, run_id: &str) -> Vec<(String, String, Option<String>, String)> {
    // Through a system-role RLS transaction, exactly like the sync
    // itself: the plain pool carries no identity, so row-level security
    // would hide every row.
    let mut tx = begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
        .await
        .unwrap();
    let rows = sqlx::query_as(
        "SELECT full_name, run_name, email, role FROM clients.ps_person_index
          WHERE workflow = 'merchant_account' AND ps_run_id = $1 ORDER BY full_name",
    )
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    rows
}

#[tokio::test]
#[ignore = "needs the local test-db -- see this module's doc comment"]
async fn sync_db_refreshes_only_changed_runs_in_batches_and_leaves_the_rest_alone() {
    let _ = dotenvy::from_filename(".env.local");
    let db = crate::db::connect_test();
    let tag = Uuid::new_v4().simple().to_string();
    let id = |name: &str| format!("sync-{tag}-{name}");

    let (new1, new2, moved, same) = (id("new1"), id("new2"), id("moved"), id("same"));
    let runs = vec![
        run(&new1, "2026-03-01T00:00:00Z"),
        run(&new2, "2026-03-01T00:00:00Z"),
        run(&moved, "2026-03-01T00:00:00Z"),
        run(&same, "2026-02-01T00:00:00Z"),
    ];

    // Existing state: `moved` was synced before PS's newer edit, with a
    // stale person; `same` is current, with a person that must survive.
    let mut seed = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
        .await
        .unwrap();
    for (run_id, updated, person) in [
        (&moved, "2026-01-01T00:00:00Z", "Stale Person"),
        (&same, "2026-02-01T00:00:00Z", "Untouched Person"),
    ] {
        sqlx::query(
            "INSERT INTO clients.ps_sync_state
                 (workflow, ps_run_id, run_name, ps_updated_at, last_synced_at)
             VALUES ('merchant_account', $1, 'Old Name', $2::timestamptz, now())",
        )
        .bind(run_id)
        .bind(updated)
        .execute(&mut *seed)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO clients.ps_person_index
                 (workflow, ps_run_id, run_name, full_name, email, phone, role)
             VALUES ('merchant_account', $1, 'Old Name', $2, NULL, NULL, 'owner')",
        )
        .bind(run_id)
        .bind(person)
        .execute(&mut *seed)
        .await
        .unwrap();
    }
    seed.commit().await.unwrap();

    let fetch_calls = Arc::new(AtomicUsize::new(0));
    let mut processed = 0usize;
    let stats = sync_workflow_runs(
        &db,
        {
            let fetch_calls = fetch_calls.clone();
            move |run_id: String| {
                let fetch_calls = fetch_calls.clone();
                async move {
                    fetch_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(merchant_account_fields(&run_id))
                }
            }
        },
        "merchant_account",
        &runs,
        extract_merchant_account_people,
        false,
        2, // three runs to refresh -> a batch of two, then a batch of one
        || processed += 1,
    )
    .await
    .expect("the sync must succeed");

    assert_eq!(stats.runs_seen, 4);
    assert_eq!(stats.runs_changed, 3);
    assert_eq!(
        stats.people_indexed, 6,
        "two people (owner + signer) per refreshed run"
    );
    assert_eq!(
        fetch_calls.load(Ordering::SeqCst),
        3,
        "the unchanged run must not be fetched from Process Street at all"
    );
    assert_eq!(
        processed, 4,
        "every run is reported processed, skipped or not"
    );

    // A new run: freshly indexed, carrying the PS run's own name.
    let rows = index_rows(&db, &new1).await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.1 == format!("Run {new1}")));
    assert!(rows
        .iter()
        .any(|r| r.0 == format!("Owner {new1}") && r.3 == "owner"));
    assert!(rows
        .iter()
        .any(|r| r.0 == format!("Signer {new1}") && r.3 == "signer"));

    // The moved run: the stale person is gone, replaced by the fresh two.
    let moved_rows = index_rows(&db, &moved).await;
    assert_eq!(moved_rows.len(), 2);
    assert!(moved_rows.iter().all(|r| r.0 != "Stale Person"));

    // The unchanged run: untouched.
    let same_rows = index_rows(&db, &same).await;
    assert_eq!(same_rows.len(), 1);
    assert_eq!(same_rows[0].0, "Untouched Person");

    // ps_sync_state: updated (with business_dba) for refreshed runs only.
    let mut read = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
        .await
        .unwrap();
    let state: (Option<String>, DateTime<Utc>) = sqlx::query_as(
        "SELECT business_dba, ps_updated_at FROM clients.ps_sync_state
          WHERE workflow = 'merchant_account' AND ps_run_id = $1",
    )
    .bind(&moved)
    .fetch_one(&mut *read)
    .await
    .unwrap();
    read.commit().await.unwrap();
    assert_eq!(state.0.as_deref(), Some(format!("DBA {moved}").as_str()));
    assert_eq!(state.1, at("2026-03-01T00:00:00Z"));
}

/// Benchmark, not a pass/fail test: the DATABASE-write cost of syncing
/// 100 changed runs (two people each), batched (`sync_workflow_runs`)
/// versus a replay of the old statement pattern (inside one
/// transaction: per run a `DELETE`, one `INSERT` per person, an upsert).
/// The fake fetch is instant, so this isolates the write phase. Only
/// meaningful through `dev-tools/latency_proxy.py`:
///
/// ```text
/// python3 dev-tools/latency_proxy.py --delay-ms 10 &      # 20 ms round trip
/// TEST_DATABASE_URL=postgres://app_service:app_service@127.0.0.1:5434/unitprep_test \
///   cargo test --release --bin unitprep -- --ignored --nocapture sync_db_batching_benchmark
/// ```
#[tokio::test]
#[ignore = "benchmark; needs test-db behind dev-tools/latency_proxy.py"]
async fn sync_db_batching_benchmark() {
    let db = crate::db::connect_test();
    let tag = Uuid::new_v4().simple().to_string();
    const RUNS: usize = 100;

    let batched_runs: Vec<WorkflowRun> = (0..RUNS)
        .map(|n| run(&format!("bench-{tag}-b{n}"), "2026-03-01T00:00:00Z"))
        .collect();
    let old_runs: Vec<WorkflowRun> = (0..RUNS)
        .map(|n| run(&format!("bench-{tag}-o{n}"), "2026-03-01T00:00:00Z"))
        .collect();

    // Warm the pool.
    let mut warm = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
        .await
        .unwrap();
    sqlx::query("SELECT 1").execute(&mut *warm).await.unwrap();
    warm.commit().await.unwrap();

    // New: batched.
    let started = std::time::Instant::now();
    sync_workflow_runs(
        &db,
        |run_id: String| async move { Ok(merchant_account_fields(&run_id)) },
        "merchant_account",
        &batched_runs,
        extract_merchant_account_people,
        false,
        RUN_BATCH_SIZE,
        || {},
    )
    .await
    .unwrap();
    let batched_ms = started.elapsed().as_secs_f64() * 1000.0;

    // Old: replay the per-run statement pattern in one transaction.
    let started = std::time::Instant::now();
    let mut tx = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
        .await
        .unwrap();
    for old_run in &old_runs {
        let fields = merchant_account_fields(&old_run.id);
        let people = extract_merchant_account_people(&fields);
        sqlx::query("DELETE FROM clients.ps_person_index WHERE workflow = $1 AND ps_run_id = $2")
            .bind("merchant_account")
            .bind(&old_run.id)
            .execute(&mut *tx)
            .await
            .unwrap();
        for person in &people {
            sqlx::query(
                "INSERT INTO clients.ps_person_index
                     (workflow, ps_run_id, run_name, full_name, email, phone, role)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind("merchant_account")
            .bind(&old_run.id)
            .bind(&old_run.name)
            .bind(&person.full_name)
            .bind(&person.email)
            .bind(&person.phone)
            .bind(person.role)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO clients.ps_sync_state (workflow, ps_run_id, run_name, business_dba, ps_updated_at, last_synced_at)
             VALUES ($1, $2, $3, $4, $5, now())
             ON CONFLICT (workflow, ps_run_id) DO UPDATE SET
                 run_name = EXCLUDED.run_name, business_dba = EXCLUDED.business_dba,
                 ps_updated_at = EXCLUDED.ps_updated_at, last_synced_at = now()",
        )
        .bind("merchant_account")
        .bind(&old_run.id)
        .bind(&old_run.name)
        .bind(Some("dba"))
        .bind(old_run.updated_at())
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let old_ms = started.elapsed().as_secs_f64() * 1000.0;

    println!(
        "write phase for {RUNS} changed runs (2 people each): old per-row pattern {old_ms:.0} ms, batched {batched_ms:.0} ms ({:.1}x faster)",
        old_ms / batched_ms
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see this module's doc comment"]
async fn sync_db_a_failed_fetch_stops_the_sync_but_keeps_what_already_committed() {
    let _ = dotenvy::from_filename(".env.local");
    let db = crate::db::connect_test();
    let tag = Uuid::new_v4().simple().to_string();
    let (first, second) = (format!("sync-{tag}-first"), format!("sync-{tag}-second"));
    let runs = vec![
        run(&first, "2026-03-01T00:00:00Z"),
        run(&second, "2026-03-01T00:00:00Z"),
    ];

    let mut processed = 0usize;
    let failing = second.clone();
    let result = sync_workflow_runs(
        &db,
        move |run_id: String| {
            let failing = failing.clone();
            async move {
                if run_id == failing {
                    Err(ProcessStreetError::Api {
                        status: 500,
                        body: "boom".to_string(),
                    })
                } else {
                    Ok(merchant_account_fields(&run_id))
                }
            }
        },
        "merchant_account",
        &runs,
        extract_merchant_account_people,
        false,
        1, // one run per batch, so the first commits before the second's fetch fails
        || processed += 1,
    )
    .await;

    assert!(
        matches!(result, Err(SyncError::ProcessStreet(_))),
        "a failed Process Street fetch must stop the sync"
    );
    assert_eq!(
        processed, 1,
        "only the committed batch is reported processed"
    );
    assert_eq!(
        index_rows(&db, &first).await.len(),
        2,
        "the first run's batch was committed and must stay"
    );
    assert!(
        index_rows(&db, &second).await.is_empty(),
        "the failed run must not be recorded at all"
    );
    let mut read = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
        .await
        .unwrap();
    let first_state: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM clients.ps_sync_state WHERE workflow = 'merchant_account' AND ps_run_id = $1",
    )
    .bind(&first)
    .fetch_one(&mut *read)
    .await
    .unwrap();
    read.commit().await.unwrap();
    assert_eq!(first_state, 1, "so the next delta check will skip it");
}
