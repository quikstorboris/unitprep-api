use axum::http::StatusCode;
use serde_json::Value;
use unitprep_core::vendor_format::{ContentType, VendorFormat};

use super::*;
use crate::api::test_support::{empty_state, test_user};

fn vendor(name: &str, signature: &[&str]) -> VendorFormat {
    VendorFormat {
        name: name.to_string(),
        content_type: ContentType::Tenants,
        signature_headers: signature.iter().map(|s| s.to_string()).collect(),
        field_mapping: Vec::new(),
        transform_key: None,
    }
}

fn meta(name: &str, pms: &str, role: FileRole, priority: i32, guidance: &str) -> FileFormatMeta {
    FileFormatMeta {
        name: name.to_string(),
        pms: pms.to_string(),
        report_name: name.to_string(),
        role,
        selection_priority: priority,
        guidance: guidance.to_string(),
    }
}

const COMMON: [&str; 3] = ["sUnitName", "LedgerID", "sFName"];

/// A state whose registry snapshots hold a SiteLink Directory / Rent Roll
/// pair (Directory first, as seeded) and QSX.
fn state_with_registry() -> AppState {
    let state = empty_state();
    let mut directory = COMMON.to_vec();
    directory.push("TenantName");
    *state.tenant_vendors.write() = vec![
        vendor("QSX", &["FirtLast", "CustNumb"]),
        vendor("SiteLink Directory", &directory),
        vendor("SiteLink Rent Roll", &COMMON),
    ];
    *state.tenant_file_meta.write() = vec![
        meta("QSX", "QSX", FileRole::Primary, 0, "QSX guidance"),
        meta(
            "SiteLink Directory",
            "SiteLink",
            FileRole::Primary,
            20,
            "Preferred",
        ),
        meta(
            "SiteLink Rent Roll",
            "SiteLink",
            FileRole::Primary,
            10,
            "Fallback",
        ),
    ];
    state
}

async fn json_body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn input(name: &str, headers: Option<&[&str]>) -> ClassifyFileInput {
    ClassifyFileInput {
        file_name: name.to_string(),
        headers: headers.map(|h| h.iter().map(|s| s.to_string()).collect()),
    }
}

#[tokio::test]
async fn classify_files_preselects_the_preferred_file_and_reports_the_rest() {
    let mut directory = COMMON.to_vec();
    directory.push("TenantName");

    let response = classify_files(
        State(state_with_registry()),
        test_user(),
        Json(ClassifyFilesRequest {
            files: vec![
                input("Rent Roll.xlsx", Some(&COMMON)),
                input("Directory.xlsx", Some(&directory)),
                input("Credit Card Roll.xlsx", Some(&["Unit", "CardNumber"])),
                input("old.xls", None),
            ],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    assert_eq!(body["suggested"]["pms"], "SiteLink");
    assert_eq!(
        body["suggested"]["selected"],
        serde_json::json!(["Directory.xlsx"])
    );
    assert_eq!(
        body["suggested"]["alternatives"],
        serde_json::json!({"Rent Roll.xlsx": "Directory.xlsx"})
    );

    let files = body["files"].as_array().unwrap();
    assert_eq!(files[0]["format_name"], "SiteLink Rent Roll");
    assert_eq!(files[1]["format_name"], "SiteLink Directory");
    assert_eq!(files[1]["status"], "recognized");
    assert_eq!(files[1]["role"], "primary");
    assert_eq!(files[1]["selection_priority"], 20);
    assert_eq!(files[2]["status"], "unrecognized");
    assert_eq!(files[2]["format_name"], Value::Null);
    assert_eq!(files[3]["status"], "unreadable");
}

#[tokio::test]
async fn classify_files_rejects_an_oversized_request() {
    let files = (0..=MAX_FILES)
        .map(|i| input(&format!("f{i}.csv"), Some(&["a"])))
        .collect();

    let response = classify_files(
        State(state_with_registry()),
        test_user(),
        Json(ClassifyFilesRequest { files }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn classify_dropbox_folder_rejects_a_path_outside_the_configured_root() {
    let response = classify_dropbox_folder(
        State(empty_state()),
        test_user(),
        Json(ClassifyDropboxFolderRequest {
            path: "/Not/Under/The/Configured/Root".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn requirements_group_by_pms_in_registry_order_with_supporting_files_last() {
    let metas = vec![
        meta("QSX", "QSX", FileRole::Primary, 0, "q"),
        meta("QC Alt", "QuikStor Cloud", FileRole::Supporting, 0, "alt"),
        meta("QC", "QuikStor Cloud", FileRole::Primary, 0, "main"),
        meta("SiteLink Directory", "SiteLink", FileRole::Primary, 20, "d"),
        meta("SiteLink Rent Roll", "SiteLink", FileRole::Primary, 10, "r"),
    ];

    let response = requirements_from(&metas);

    let pms: Vec<&str> = response.vendors.iter().map(|v| v.pms.as_str()).collect();
    assert_eq!(pms, vec!["QSX", "QuikStor Cloud", "SiteLink"]);

    let qc: Vec<&str> = response.vendors[1]
        .formats
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(
        qc,
        vec!["QC", "QC Alt"],
        "the usable file comes before the supporting one"
    );

    let sl: Vec<&str> = response.vendors[2]
        .formats
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(sl, vec!["SiteLink Directory", "SiteLink Rent Roll"]);
}

#[tokio::test]
async fn file_requirements_serves_the_registry_snapshot() {
    let response = file_requirements(State(state_with_registry()), test_user()).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let vendors = body["vendors"].as_array().unwrap();
    assert_eq!(vendors.len(), 2);
    assert_eq!(vendors[1]["pms"], "SiteLink");
    assert_eq!(vendors[1]["formats"][0]["guidance"], "Preferred");
    assert_eq!(vendors[1]["formats"][0]["role"], "primary");
}
