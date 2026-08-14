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

/// Regression test for the session-ownership IDOR fix (see
/// core::session_store's with_owned_session): a tagger session
/// belonging to one user must be completely invisible to a different
/// authenticated caller. The tagger session store is entirely separate
/// from unit-group's and dedup's own -- and tagger.rs's report/apply
/// handlers were the one instance of this gap the original audit's
/// agent-scoping missed, so this store specifically is worth its own
/// direct proof, not just trusting the same fix generalizes.
#[tokio::test]
async fn report_returns_404_for_a_session_belonging_to_a_different_user() {
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

    let someone_else = crate::auth::AuthenticatedUser {
        user_id: uuid::Uuid::new_v4(),
        role_keys: Vec::new(),
        permission_keys: std::collections::HashSet::new(),
        token_hash: vec![0u8; 32],
        elevated_until: None,
        requires_step_up: false,
        passkey_reverified_until: None,
    };

    let response = report(
        State(state),
        someone_else,
        Json(TaggerSessionRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
async fn apply_without_preserve_blanks_deletes_a_blanks_underscores_and_underlines_the_tag() {
    let original_bytes = std::fs::read(FIXTURE).unwrap();
    let doc = read_docx(&original_bytes).unwrap();
    // The fixture's real "Name:  _____...Space #:" blank -- 33
    // underscores.
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

    // The underscores at this specific blank are genuinely gone -- not
    // hidden, deleted -- with the tag landing directly where they were.
    // (The fixture has other same-length underscore runs elsewhere for
    // unrelated fields, so this checks the exact position, not a
    // document-wide substring search.)
    assert!(edited_doc.body.text[blank_start..].starts_with("{{e.name}}"));
    // The tag itself is underlined so it still reads as a filled-in
    // blank -- immediately followed by the run's <w:t> (whatever
    // attributes that tag happens to carry, e.g. xml:space).
    assert!(raw_xml.contains(r#"<w:u w:val="single"/></w:rPr><w:t"#));
}

fn read_document_xml_from_docx(bytes: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut entry = zip.by_name("word/document.xml").unwrap();
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut entry, &mut contents).unwrap();
    contents
}

// `check` takes a real `axum::extract::Multipart` like `upload` does --
// see upload_tests.rs's own doc comment for why these tests build a real
// multipart body rather than bypassing the extractor.
mod check_tests {
    use axum::body::Body;
    use axum::extract::FromRequest;
    use axum::http::{Request, StatusCode};

    use super::*;
    use crate::api::test_support::empty_state;

    const BOUNDARY: &str = "UnitPrepTaggerCheckTestBoundary";

    fn file_part(field_name: &str, file_name: &str, content: &str) -> String {
        format!(
            "--{BOUNDARY}\r\n\
             Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{file_name}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n\
             {content}\r\n"
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

    #[tokio::test]
    async fn check_returns_400_when_no_file_was_uploaded() {
        let state = empty_state();
        let multipart = multipart_from(closing_boundary(), &state).await;

        let response = check(
            State(state),
            crate::api::test_support::test_user(),
            multipart,
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "no_file_uploaded");
    }

    // Reaches read_docx (no DB round trip needed to get here -- that only
    // happens after a file successfully parses as a .docx), so this
    // doesn't need a real Postgres connection despite `check` needing one
    // for the happy path.
    #[tokio::test]
    async fn check_returns_400_for_a_file_that_is_not_a_valid_docx() {
        let state = empty_state();
        let mut body = file_part(
            "file",
            "not-a-template.docx",
            "this is plain text, not a zip",
        );
        body.push_str(&closing_boundary());
        let multipart = multipart_from(body, &state).await;

        let response = check(
            State(state),
            crate::api::test_support::test_user(),
            multipart,
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "invalid_docx");
    }
}
