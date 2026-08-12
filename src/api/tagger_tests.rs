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
            preserve_blanks: false,
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
            preserve_blanks: false,
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
async fn apply_succeeds_for_a_candidate_spanning_multiple_runs() {
    let original_bytes = std::fs::read(FIXTURE).unwrap();
    let doc = read_docx(&original_bytes).unwrap();

    // A span deliberately crossing from the first real run into the
    // second -- exactly the real-world case (a blank's underscore run
    // split across multiple <w:t> elements) docx-surgeon now splices
    // across rather than refusing.
    let first = doc.body.runs[0];
    let second = doc.body.runs[1];
    let candidates = vec![sample_candidate(
        "e.name",
        "unused",
        first.flat_start,
        second.flat_end,
    )];
    let state = tagger_state_with_session("s1", original_bytes, "atherton.docx", candidates);

    let response = apply(
        State(state),
        crate::api::test_support::test_user(),
        Json(TaggerApplyRequest {
            session_id: "s1".to_string(),
            preserve_blanks: false,
            confirmed: vec![ConfirmedSubstitution {
                candidate_index: 0,
                tag_key: "e.name".to_string(),
            }],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn apply_reports_exactly_which_candidate_has_stale_coordinates() {
    let original_bytes = std::fs::read(FIXTURE).unwrap();

    // Coordinates that touch no run at all -- the one case that still
    // fails validation now that a multi-run span is applicable.
    let candidates = vec![sample_candidate("e.name", "unused", 999_999, 999_999 + 10)];
    let state = tagger_state_with_session("s1", original_bytes, "atherton.docx", candidates);

    let response = apply(
        State(state),
        crate::api::test_support::test_user(),
        Json(TaggerApplyRequest {
            session_id: "s1".to_string(),
            preserve_blanks: false,
            confirmed: vec![ConfirmedSubstitution {
                candidate_index: 0,
                tag_key: "e.name".to_string(),
            }],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["error"], "unappliable_substitutions");
    assert_eq!(body["failed"][0]["candidate_index"], 0);
    assert_eq!(body["failed"][0]["tag_key"], "e.name");
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
            preserve_blanks: false,
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

#[tokio::test]
async fn apply_with_preserve_blanks_centers_the_tag_inside_the_matched_text() {
    let original_bytes = std::fs::read(FIXTURE).unwrap();
    let doc = read_docx(&original_bytes).unwrap();
    let start = doc.body.text.find("Atherton Storage").unwrap();
    let end = start + "Atherton Storage".len();
    // "Atherton Storage" is 16 chars, "{{f.name}}" is 10 -- 6 chars of
    // padding split 3/3, so "Ath" and "age" survive on either side.

    let candidates = vec![sample_candidate("f.name", "Atherton Storage", start, end)];
    let state = tagger_state_with_session("s1", original_bytes, "atherton.docx", candidates);

    let response = apply(
        State(state),
        crate::api::test_support::test_user(),
        Json(TaggerApplyRequest {
            session_id: "s1".to_string(),
            preserve_blanks: true,
            confirmed: vec![ConfirmedSubstitution {
                candidate_index: 0,
                tag_key: "f.name".to_string(),
            }],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let edited_doc = read_docx(&bytes).unwrap();
    // The tag lands in the middle -- "Ath" and "age" (3 chars of the
    // original matched text on each side) survive around it.
    assert!(edited_doc.body.text.starts_with("Ath{{f.name}}age"));
}

#[tokio::test]
async fn apply_without_preserve_blanks_hides_a_blanks_underscores_instead_of_deleting_them() {
    let original_bytes = std::fs::read(FIXTURE).unwrap();
    let doc = read_docx(&original_bytes).unwrap();
    // The fixture's real "Name:  _____...Space #:" blank -- 33
    // underscores. "{{e.name}}" is 10 chars -- 23 chars of padding
    // split 11/12.
    let blank_start = doc.body.text.find("_____").unwrap();
    let blank_len = doc.body.text[blank_start..]
        .find(|c: char| c != '_')
        .unwrap();
    let blank_end = blank_start + blank_len;

    let candidates = vec![sample_candidate(
        "e.name",
        &"_".repeat(blank_len),
        blank_start,
        blank_end,
    )];
    let state = tagger_state_with_session("s1", original_bytes, "atherton.docx", candidates);

    let response = apply(
        State(state),
        crate::api::test_support::test_user(),
        Json(TaggerApplyRequest {
            session_id: "s1".to_string(),
            preserve_blanks: false,
            confirmed: vec![ConfirmedSubstitution {
                candidate_index: 0,
                tag_key: "e.name".to_string(),
            }],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let raw_xml = read_document_xml_from_docx(&bytes);
    let edited_doc = read_docx(&bytes).unwrap();

    // The underscores are still really there -- just invisible -- so
    // the flattened text reads exactly like the visible PreserveBlank
    // style would.
    assert!(edited_doc.body.text.contains("{{e.name}}"));
    assert!(edited_doc.body.text.contains(&"_".repeat(blank_len)));
    // But the raw XML proves they were actually hidden, not left visible.
    assert!(raw_xml.contains("<w:color w:val=\"FFFFFF\"/>"));
}

fn read_document_xml_from_docx(bytes: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut entry = zip.by_name("word/document.xml").unwrap();
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut entry, &mut contents).unwrap();
    contents
}
