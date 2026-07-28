//! `upload` takes a real `axum::extract::Multipart`, unlike most other
//! handlers in this crate (which separate a "testable core" that takes
//! already-parsed data — see `group_file_upload::apply_group_file_upload`
//! — from a thin Multipart-extracting wrapper). Upload's multipart
//! parsing itself (sidecar JSON, missing-filename handling,
//! partial-failure counting) is exactly the logic worth testing, so
//! these tests build a real multipart request body and drive it through
//! axum's own `Multipart` extractor instead of bypassing it.

use axum::body::{to_bytes, Body};
use axum::extract::{FromRequest, State};
use axum::http::{Request, StatusCode};

use super::*;
use crate::api::test_support::empty_state;

const BOUNDARY: &str = "UnitPrepTestBoundary";

fn file_part(field_name: &str, file_name: &str, content: &str) -> String {
    format!(
        "--{BOUNDARY}\r\n\
         Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{file_name}\"\r\n\
         Content-Type: text/csv\r\n\r\n\
         {content}\r\n"
    )
}

/// A plain form field with no `filename` attribute — what `upload`'s
/// `field.file_name()` sees as `None`, the "multipart field missing
/// filename" path.
fn field_with_no_filename(field_name: &str, value: &str) -> String {
    format!(
        "--{BOUNDARY}\r\n\
         Content-Disposition: form-data; name=\"{field_name}\"\r\n\r\n\
         {value}\r\n"
    )
}

fn closing_boundary() -> String {
    format!("--{BOUNDARY}--\r\n")
}

async fn multipart_from(body: String, state: &AppState) -> Multipart {
    let request = Request::builder()
        .method("POST")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();

    Multipart::from_request(request, state).await.unwrap()
}

async fn json_body(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn rejects_upload_with_no_successfully_uploaded_files() {
    let state = empty_state();
    let multipart = multipart_from(closing_boundary(), &state).await;

    let response = upload(State(state), multipart).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = json_body(response).await;
    assert_eq!(body["error"], "no_file_uploaded");
}

#[tokio::test]
async fn a_single_real_file_uploads_successfully() {
    let state = empty_state();
    let mut body = file_part(
        "file",
        "units.csv",
        "number,unitgroup\r\nA01,10x10 Inside Climate",
    );
    body.push_str(&closing_boundary());

    let multipart = multipart_from(body, &state).await;
    let response = upload(State(state), multipart).await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["files_uploaded"], 1);
    assert_eq!(body["files_failed"], 0);
    assert_eq!(body["multipart_errors"], 0);
}

/// Regression coverage for the partial-failure counting the audit
/// flagged as untested: a field with no filename must count as a
/// failure without blocking the other, valid file in the same upload.
#[tokio::test]
async fn a_field_missing_a_filename_is_counted_as_failed_without_blocking_the_rest() {
    let state = empty_state();
    let mut body = field_with_no_filename("file", "not a real file");
    body.push_str(&file_part(
        "file",
        "units.csv",
        "number,unitgroup\r\nA01,10x10 Inside Climate",
    ));
    body.push_str(&closing_boundary());

    let multipart = multipart_from(body, &state).await;
    let response = upload(State(state), multipart).await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["files_uploaded"], 1);
    assert_eq!(body["files_failed"], 1);
}

/// Two real files in one upload both get counted and land in the
/// resulting session.
#[tokio::test]
async fn multiple_real_files_are_all_counted() {
    let state = empty_state();
    let mut body = file_part(
        "file",
        "units.csv",
        "number,unitgroup\r\nA01,10x10 Inside Climate",
    );
    body.push_str(&file_part("file", "groups.csv", "Name,Description\r\nA,B"));
    body.push_str(&closing_boundary());

    let multipart = multipart_from(body, &state).await;
    let response = upload(State(state), multipart).await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["files_uploaded"], 2);
    assert_eq!(body["files_failed"], 0);
}
