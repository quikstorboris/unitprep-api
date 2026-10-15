//! The Template Tagger's real-document corpus.
//!
//! Every folder under `tests/corpus/` is one real document the tagger has
//! to handle, with the output it is expected to produce:
//!
//! - `input.docx`: the document (redacted if it holds personal data)
//! - `patterns.json`: the label patterns in force when the case was blessed,
//!   so the test needs no database
//! - `expected.json`: every candidate (tag, tier, region, context) and the
//!   tags that must appear in the applied output
//! - `README.md`: where the file came from and what is unusual in it
//!
//! Adding a case is adding a folder; this test never changes. After an
//! intended change in behavior, review the diff and re-bless with
//! `UPDATE_CORPUS=1 cargo test -p unitprep-tagger-pipeline --test corpus`.
//!
//! Besides matching `expected.json`, every case must satisfy invariants that
//! hold for *any* document: the edited `document.xml` is well-formed XML, every
//! other part of the .docx and every `mc:Fallback` copy of a shape is
//! byte-for-byte unchanged, and each Auto candidate is applied exactly once.
//! See the vault's "Real Documents Are the Spec".

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use docx_surgeon::{edit_docx_all, read_docx, Edit, RegionRef, UnderlineEdit};
use serde::{Deserialize, Serialize};
use unitprep_tagger_pipeline::{
    find_candidates, to_edit, AppliedEdit, ConfidenceTier, SubstitutionStyle,
};
use unitprep_template_tagger::LabelProximityPattern;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ExpectedCandidate {
    tier: String,
    tag_key: String,
    region: String,
    /// Up to 26 characters of the region's text just before the candidate.
    context: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Expected {
    candidates: Vec<ExpectedCandidate>,
    /// Every `{{tag}}` in the applied output, in document order.
    applied_tags: Vec<String>,
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn region_label(region: RegionRef) -> String {
    match region {
        RegionRef::Body => "body".to_string(),
        RegionRef::TableCell(index) => format!("cell:{index}"),
    }
}

fn zip_entry(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a valid zip");
    let mut entry = archive.by_name(name).expect("entry present");
    let mut out = Vec::new();
    entry.read_to_end(&mut out).unwrap();
    out
}

fn zip_names(bytes: &[u8]) -> Vec<String> {
    let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a valid zip");
    archive.file_names().map(str::to_string).collect()
}

/// Every `<mc:Fallback>...</mc:Fallback>` block, verbatim.
fn fallback_blocks(xml: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<mc:Fallback>") {
        let end = rest[start..].find("</mc:Fallback>").expect("balanced") + start;
        blocks.push(&rest[start..end]);
        rest = &rest[end..];
    }
    blocks
}

fn assert_well_formed(xml: &str, case: &str) {
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Eof) => return,
            Ok(_) => {}
            Err(err) => panic!("{case}: edited document.xml is not well-formed: {err}"),
        }
    }
}

fn applied_tags(doc: &docx_surgeon::FlatDocument) -> Vec<String> {
    let mut tags = Vec::new();
    for text in std::iter::once(&doc.body).chain(doc.table_cells.iter()) {
        let mut rest = text.text.as_str();
        while let Some(start) = rest.find("{{") {
            let end = rest[start..].find("}}").map(|e| start + e + 2).unwrap();
            tags.push(rest[start..end].to_string());
            rest = &rest[end..];
        }
    }
    tags
}

struct Outcome {
    expected: Expected,
    edited: Vec<u8>,
    auto_count: usize,
}

fn run_case(dir: &Path) -> (Vec<u8>, Outcome) {
    let input = fs::read(dir.join("input.docx")).expect("input.docx");
    let patterns: Vec<LabelProximityPattern> =
        serde_json::from_slice(&fs::read(dir.join("patterns.json")).expect("patterns.json"))
            .expect("patterns.json parses");

    let doc = read_docx(&input).expect("input.docx reads");
    let mut found = find_candidates(&doc, &[], &patterns);
    found.sort_by_key(|c| {
        (
            region_label(c.region),
            c.candidate.start,
            c.candidate.tag_key.clone(),
        )
    });

    let mut candidates = Vec::new();
    let mut edits: Vec<Edit> = Vec::new();
    let mut underline: Vec<UnderlineEdit> = Vec::new();
    let mut auto_count = 0;
    for candidate in &found {
        let text = match candidate.region {
            RegionRef::Body => &doc.body.text,
            RegionRef::TableCell(i) => &doc.table_cells[i].text,
        };
        let from = text[..candidate.candidate.start]
            .char_indices()
            .rev()
            .nth(25)
            .map_or(0, |(i, _)| i);
        candidates.push(ExpectedCandidate {
            tier: format!("{:?}", candidate.tier),
            tag_key: candidate.candidate.tag_key.clone(),
            region: region_label(candidate.region),
            context: text[from..candidate.candidate.start].to_string(),
        });

        if candidate.tier == ConfidenceTier::Auto {
            auto_count += 1;
            let tag = format!("{{{{{}}}}}", candidate.candidate.tag_key);
            match to_edit(candidate, tag, SubstitutionStyle::Replace) {
                AppliedEdit::Plain(edit) => edits.push(edit),
                AppliedEdit::Underline(edit) => underline.push(edit),
            }
        }
    }

    let edited = edit_docx_all(&input, &edits, &underline).expect("edits apply");
    let applied = applied_tags(&read_docx(&edited).expect("edited output reads"));
    (
        input,
        Outcome {
            expected: Expected {
                candidates,
                applied_tags: applied,
            },
            edited,
            auto_count,
        },
    )
}

fn check_invariants(case: &str, input: &[u8], outcome: &Outcome) {
    // Each Auto candidate is applied exactly once.
    assert_eq!(
        outcome.expected.applied_tags.len(),
        outcome.auto_count,
        "{case}: applied tag count != Auto candidate count"
    );

    let before = String::from_utf8(zip_entry(input, "word/document.xml")).unwrap();
    let after = String::from_utf8(zip_entry(&outcome.edited, "word/document.xml")).unwrap();
    assert_well_formed(&after, case);

    // The legacy copy of every shape is never touched.
    assert_eq!(
        fallback_blocks(&before),
        fallback_blocks(&after),
        "{case}: an mc:Fallback copy changed"
    );

    // Nothing outside document.xml changes.
    assert_eq!(
        zip_names(input).len(),
        zip_names(&outcome.edited).len(),
        "{case}: the set of zip parts changed"
    );
    for name in zip_names(input) {
        if name != "word/document.xml" {
            assert_eq!(
                zip_entry(input, &name),
                zip_entry(&outcome.edited, &name),
                "{case}: zip part {name} changed"
            );
        }
    }
}

#[test]
fn every_corpus_case_matches_its_expected_output() {
    let bless = std::env::var_os("UPDATE_CORPUS").is_some();
    let mut cases: Vec<PathBuf> = fs::read_dir(corpus_dir())
        .expect("tests/corpus exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("input.docx").exists())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "the corpus has no cases");

    for dir in cases {
        let case = dir.file_name().unwrap().to_string_lossy().into_owned();
        let (input, outcome) = run_case(&dir);
        check_invariants(&case, &input, &outcome);

        let expected_path = dir.join("expected.json");
        if bless {
            let json = serde_json::to_string_pretty(&outcome.expected).unwrap();
            fs::write(&expected_path, format!("{json}\n")).unwrap();
            continue;
        }

        let expected: Expected =
            serde_json::from_slice(&fs::read(&expected_path).unwrap_or_else(|_| {
                panic!("{case}: no expected.json (bless with UPDATE_CORPUS=1)")
            }))
            .unwrap();
        if outcome.expected != expected {
            let missing: Vec<&ExpectedCandidate> = expected
                .candidates
                .iter()
                .filter(|c| !outcome.expected.candidates.contains(c))
                .collect();
            let unexpected: Vec<&ExpectedCandidate> = outcome
                .expected
                .candidates
                .iter()
                .filter(|c| !expected.candidates.contains(c))
                .collect();
            panic!(
                "{case}: output differs from expected.json\n\
                 expected but not produced: {missing:#?}\n\
                 produced but not expected: {unexpected:#?}\n\
                 applied tags expected {:?}\n\
                 applied tags actual   {:?}",
                expected.applied_tags, outcome.expected.applied_tags
            );
        }
    }
}
