//! How long `read_docx` takes on the real fixture documents -- the number
//! behind the decision NOT to cache the parsed document in a Tagger session
//! (the report and apply handlers each re-parse the stored bytes).
//!
//! Report-only (no assertion; the machine decides the numbers):
//!
//! ```text
//! cargo test --release -p docx-surgeon --test read_docx_timing -- --ignored --nocapture
//! ```

use std::time::Instant;

use docx_surgeon::read_docx;

const FIXTURES: &[(&str, &str)] = &[
    (
        "atherton-storage-contract",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/atherton-storage-contract.docx"
        ),
    ),
    (
        "longview-self-storage-rental-agreement",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tagger-pipeline/tests/corpus/longview-self-storage-rental-agreement/input.docx"
        ),
    ),
];

#[test]
#[ignore = "timing report only; run with --release --nocapture"]
fn print_read_docx_timing() {
    const RUNS: u32 = 50;

    for (name, path) in FIXTURES {
        let bytes = std::fs::read(path).expect("fixture should exist");

        // One warm-up, then the timed runs.
        read_docx(&bytes).expect("fixture should parse");
        let mut timings: Vec<f64> = (0..RUNS)
            .map(|_| {
                let started = Instant::now();
                read_docx(&bytes).expect("fixture should parse");
                started.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        timings.sort_by(|a, b| a.partial_cmp(b).unwrap());

        println!(
            "{name}: {} KB, read_docx p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms over {RUNS} runs",
            bytes.len() / 1024,
            timings[timings.len() / 2],
            timings[(timings.len() * 95) / 100],
            timings[timings.len() - 1],
        );
    }
}
