use axum::http::StatusCode;

use super::*;
use crate::api::tagger_test_support::tagger_state_with_session;
use crate::api::test_support::empty_state;
use unitprep_template_tagger::Candidate;

const FIXTURE: &str = "docx-surgeon/tests/fixtures/atherton-storage-contract.docx";

fn sample_candidate(
    tag_key: &str,
    matched_text: &str,
    start: usize,
    end: usize,
) -> RegionCandidate {
    RegionCandidate {
        region: RegionRef::Body,
        candidate: Candidate {
            tag_key: tag_key.to_string(),
            matched_text: matched_text.to_string(),
            start,
            end,
        },
        tier: ConfidenceTier::Auto,
    }
}

#[tokio::test]
async fn report_returns_404_for_missing_session() {
    let response = report(
        State(empty_state()),
        crate::api::test_support::test_user(),
        Json(TaggerSessionRequest {
            session_id: "missing".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn report_returns_the_stored_candidates_with_a_snippet() {
    let original_bytes = std::fs::read(FIXTURE).expect("fixture must exist");
    let doc = read_docx(&original_bytes).expect("fixture should be a valid .docx");
    let start = doc
        .body
        .text
        .find("Atherton Storage")
        .expect("fixture should mention the facility name");
    let end = start + "Atherton Storage".len();

    let candidates = vec![sample_candidate("f.name", "Atherton Storage", start, end)];
    let state = tagger_state_with_session("s1", original_bytes, "atherton.docx", candidates);

    let response = report(
        State(state),
        crate::api::test_support::test_user(),
        Json(TaggerSessionRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["candidates"][0]["tag_key"], "f.name");
    assert_eq!(body["candidates"][0]["matched_text"], "Atherton Storage");
    assert_eq!(body["candidates"][0]["tier"], "auto");
    assert!(body["candidates"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("Atherton Storage"));
}

#[tokio::test]
async fn apply_returns_404_for_missing_session() {
    let response = apply(
        State(empty_state()),
        crate::api::test_support::test_user(),
        Json(TaggerApplyRequest {
            session_id: "missing".to_string(),
            confirmed: vec![],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn apply_returns_400_for_an_out_of_range_candidate_index() {
    let original_bytes = std::fs::read(FIXTURE).unwrap();
    let state = tagger_state_with_session("s1", original_bytes, "atherton.docx", vec![]);

    let response = apply(
        State(state),
        crate::api::test_support::test_user(),
        Json(TaggerApplyRequest {
            session_id: "s1".to_string(),
            confirmed: vec![ConfirmedSubstitution {
                candidate_index: 0,
                tag_key: "f.name".to_string(),
            }],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn apply_produces_a_docx_with_the_confirmed_substitution() {
    let original_bytes = std::fs::read(FIXTURE).unwrap();
    let doc = read_docx(&original_bytes).unwrap();
    let start = doc.body.text.find("Atherton Storage").unwrap();
    let end = start + "Atherton Storage".len();

    let candidates = vec![sample_candidate("f.name", "Atherton Storage", start, end)];
    let state = tagger_state_with_session("s1", original_bytes, "atherton.docx", candidates);

    let response = apply(
        State(state),
        crate::api::test_support::test_user(),
        Json(TaggerApplyRequest {
            session_id: "s1".to_string(),
            confirmed: vec![ConfirmedSubstitution {
                candidate_index: 0,
                tag_key: "f.name".to_string(),
            }],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-disposition").unwrap(),
        "attachment; filename=\"atherton-tagged.docx\""
    );

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let edited_doc = read_docx(&bytes).unwrap();
    assert!(edited_doc.body.text.starts_with("{{f.name}}"));
}
