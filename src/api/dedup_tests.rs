use axum::http::StatusCode;

use super::*;
use crate::api::test_support::empty_state;
use crate::api::dedup_test_support::dedup_state_with_report;

fn sample_record(unit: &str, email: &str) -> unitprep_dedup::TenantRecord {
    unitprep_dedup::TenantRecord {
        unit_number: unit.to_string(),
        first_last: "smith".to_string(),
        first_name: "John".to_string(),
        last_name: "Smith".to_string(),
        email: email.to_string(),
        ..Default::default()
    }
}

#[tokio::test]
async fn report_returns_404_for_missing_session() {
    let response = report(
        State(empty_state()),
        Json(DedupSessionRequest { session_id: "missing".to_string() }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn report_returns_the_stored_report() {
    let records = vec![sample_record("101", "a@example.com"), sample_record("204", "")];
    let dedup_report = unitprep_dedup::run(records.clone());
    assert_eq!(dedup_report.flagged_groups.len(), 1, "fixture should produce one flagged group");

    let state = dedup_state_with_report("s1", records, dedup_report);

    let response =
        report(State(state), Json(DedupSessionRequest { session_id: "s1".to_string() })).await;

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["flagged_groups"].as_array().unwrap().len(), 1);
    assert!(body["flagged_groups"][0]["note"]
        .as_str()
        .unwrap()
        .contains("units 101, 204"));
}

#[tokio::test]
async fn export_returns_404_for_missing_session() {
    let response = export(
        State(empty_state()),
        Json(DedupSessionRequest { session_id: "missing".to_string() }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn export_produces_csv_containing_the_flagged_group() {
    let records = vec![sample_record("101", "a@example.com"), sample_record("204", "")];
    let dedup_report = unitprep_dedup::run(records.clone());
    let state = dedup_state_with_report("s1", records, dedup_report);

    let response =
        export(State(state), Json(DedupSessionRequest { session_id: "s1".to_string() })).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/csv"
    );

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let csv = String::from_utf8(bytes.to_vec()).unwrap();

    assert!(csv.contains("CustNumb,UnitNumber,CorrectionNote"));
    assert!(csv.contains("units 101, 204"));
}
