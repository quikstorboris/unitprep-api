//! Baseline and speed guard for the stages that run AFTER the dedup report
//! exists: the export plan, the report view the UI renders, and the CSV /
//! XLSX bytes. The report itself is timed in `dedup/src/performance_tests.rs`;
//! this file adds the stages that live in this crate, on the same
//! deterministic synthetic facility, so the Phase C optimizations
//! (efficiency refactor C1-C3) can be measured before and after.
//!
//! The `#[ignore]`d `print_baseline` prints one line per stage and size:
//!
//! ```text
//! cargo test --release --bin unitprep -- --ignored --nocapture print_baseline
//! cargo test           --bin unitprep -- --ignored --nocapture print_baseline
//! ```
//!
//! The budget test runs in every `cargo test` (debug build); its limits
//! are deliberately generous so a slow CI machine does not flake. If one
//! fails, measure with `print_baseline` before raising it.

use std::time::{Duration, Instant};

use unitprep_dedup::synthetic::synthetic_facility;
use unitprep_dedup::{run, DedupReport, TenantRecord};

use super::dedup_view::build_report_view;
use crate::infrastructure::dedup_csv_export::generate_csv;
use crate::infrastructure::dedup_export_plan::build_export_plan;
use crate::infrastructure::dedup_xlsx_export::generate_xlsx;

struct Stages {
    report: Duration,
    export_plan: Duration,
    report_view: Duration,
    csv: Duration,
    xlsx: Duration,
}

fn timed<T>(work: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = work();
    (value, started.elapsed())
}

fn time_stages(rows: usize) -> (Stages, DedupReport, Vec<TenantRecord>) {
    let records = synthetic_facility(rows);
    let (report, report_time) = timed(|| run(records.clone()));
    let (plan, export_plan) = timed(|| build_export_plan(&report, &records));
    std::hint::black_box(plan);
    let (view, report_view) = timed(|| build_report_view(&report, &records));
    std::hint::black_box(view);
    let (csv_bytes, csv) = timed(|| generate_csv(&report, &records).expect("csv"));
    std::hint::black_box(csv_bytes);
    let (xlsx_bytes, xlsx) = timed(|| generate_xlsx(&report, &records).expect("xlsx"));
    std::hint::black_box(xlsx_bytes);
    (
        Stages {
            report: report_time,
            export_plan,
            report_view,
            csv,
            xlsx,
        },
        report,
        records,
    )
}

#[test]
fn the_stages_after_the_report_stay_within_budget_at_a_large_facility() {
    let (stages, report, records) = time_stages(2400);

    let held_out = report.unidentified.as_ref().map_or(0, |u| u.tenants.len());
    assert!(
        report.unique_tenants + held_out > 1000,
        "the synthetic facility should have well over a thousand tenants"
    );
    assert_eq!(records.len(), 2400);
    for (name, elapsed) in [
        ("export plan", stages.export_plan),
        ("report view", stages.report_view),
        ("csv", stages.csv),
        ("xlsx", stages.xlsx),
    ] {
        assert!(
            elapsed < Duration::from_secs(8),
            "{name} for 2400 rows took {elapsed:?} (budget 8s)"
        );
    }
}

#[test]
#[ignore = "prints timings for the efficiency-refactor baseline; run with --nocapture"]
fn print_baseline() {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    for rows in [800, 2400, 5000] {
        // Best of three keeps one noisy run from becoming the baseline.
        let mut best: Option<Stages> = None;
        for _ in 0..3 {
            let (stages, _, _) = time_stages(rows);
            best = Some(match best {
                None => stages,
                Some(b) => Stages {
                    report: b.report.min(stages.report),
                    export_plan: b.export_plan.min(stages.export_plan),
                    report_view: b.report_view.min(stages.report_view),
                    csv: b.csv.min(stages.csv),
                    xlsx: b.xlsx.min(stages.xlsx),
                },
            });
        }
        let s = best.unwrap();
        println!(
            "BASELINE {profile} rows={rows} report={:?} export_plan={:?} report_view={:?} csv={:?} xlsx={:?}",
            s.report, s.export_plan, s.report_view, s.csv, s.xlsx
        );
    }
}
