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
        Json(DedupExportRequest { session_id: "missing".to_string(), format: ExportFormat::Csv }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn export_defaults_to_csv_when_format_is_omitted() {
    // #[serde(default)] on ExportFormat -- an existing caller that
    // doesn't send `format` at all must keep getting a CSV, not an
    // error or a different default.
    let body: DedupExportRequest = serde_json::from_str(r#"{"session_id": "s1"}"#).unwrap();
    assert_eq!(body.format, ExportFormat::Csv);
}

#[tokio::test]
async fn export_produces_csv_containing_the_flagged_group() {
    let records = vec![sample_record("101", "a@example.com"), sample_record("204", "")];
    let dedup_report = unitprep_dedup::run(records.clone());
    let state = dedup_state_with_report("s1", records, dedup_report);

    let response = export(
        State(state),
        Json(DedupExportRequest { session_id: "s1".to_string(), format: ExportFormat::Csv }),
    )
    .await;

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

#[tokio::test]
async fn export_produces_xlsx_with_the_right_content_type() {
    let records = vec![sample_record("101", "a@example.com"), sample_record("204", "")];
    let dedup_report = unitprep_dedup::run(records.clone());
    let state = dedup_state_with_report("s1", records, dedup_report);

    let response = export(
        State(state),
        Json(DedupExportRequest { session_id: "s1".to_string(), format: ExportFormat::Xlsx }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    );

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    // A real xlsx is a zip archive -- confirm the magic bytes rather
    // than just "some bytes came back".
    assert_eq!(&bytes[0..2], b"PK");
}

#[tokio::test]
async fn export_produces_a_zip_containing_both_formats() {
    let records = vec![sample_record("101", "a@example.com"), sample_record("204", "")];
    let dedup_report = unitprep_dedup::run(records.clone());
    let state = dedup_state_with_report("s1", records, dedup_report);

    let response = export(
        State(state),
        Json(DedupExportRequest { session_id: "s1".to_string(), format: ExportFormat::Both }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get(header::CONTENT_TYPE).unwrap(), "application/zip");

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("valid zip");
    let names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect();

    assert!(names.contains(&"duplicate_tenant_check.csv".to_string()));
    assert!(names.contains(&"duplicate_tenant_check.xlsx".to_string()));
}
