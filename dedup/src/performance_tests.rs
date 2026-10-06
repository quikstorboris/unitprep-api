//! Speed guard for the whole dedup pipeline at a realistic facility size.
//!
//! Every other test here uses a handful of records, far too few to expose
//! a pass that is quadratic in the number of tenants -- the typo-variant
//! pass was exactly that, took ~9 s on a real 759-row file in a debug
//! build, and no test noticed. This runs the full `report::run` over a
//! deterministic synthetic dataset (no real data) with a time budget set
//! well above today's cost but far below what a quadratic regression
//! costs, so reintroducing an unpruned pairwise pass fails the suite.
//!
//! The budgets are for a DEBUG build, which is what `cargo test` and the
//! dev server use and where the cost shows first. They are deliberately
//! generous (>=15x today's time) so a slow or busy CI machine does not
//! flake; if one ever fails, measure before raising it.

use std::time::{Duration, Instant};

use crate::report::run;
use crate::synthetic::synthetic_facility;

/// Times the whole run in both modes for tenants that have no customer id
/// (the synthetic facility has some): the default listing, and the heavier
/// by-name matching a user can ask for afterwards.
fn time_run(rows: usize) -> (Duration, Duration, usize) {
    let records = synthetic_facility(rows);
    assert_eq!(records.len(), rows);

    let started = Instant::now();
    let report = run(records.clone());
    let default_run = started.elapsed();

    let started = Instant::now();
    let _ = crate::run_with_options(
        records,
        &crate::TemplateNoteComposer,
        crate::UnidentifiedMode::MatchedByName,
    );
    let matched_run = started.elapsed();

    let held_out = report.unidentified.map(|u| u.tenants.len()).unwrap_or(0);
    (default_run, matched_run, report.unique_tenants + held_out)
}

#[test]
fn a_realistic_facility_is_analyzed_within_the_speed_budget() {
    // Today (debug build): ~0.1 s. The unpruned pairwise pass: ~9 s.
    let (elapsed, matched, tenants) = time_run(800);

    assert!(
        tenants > 500,
        "the synthetic facility should have hundreds of tenants, got {tenants}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "analyzing 800 rows took {elapsed:?} (budget 2s) -- a pass over the tenants has probably gone quadratic without pruning"
    );
    assert!(
        matched < Duration::from_secs(2),
        "matching the tenants without a customer id by name took {matched:?} (budget 2s)"
    );
}

#[test]
fn a_large_facility_still_scales_within_budget() {
    // 3x the rows is ~9x the pairs for a pairwise pass: the pruned pass
    // stays cheap, an unpruned one would take ~80 s.
    let (elapsed, _, _) = time_run(2400);

    assert!(
        elapsed < Duration::from_secs(8),
        "analyzing 2400 rows took {elapsed:?} (budget 8s)"
    );
}

/// Prints the report-stage timings for the efficiency-refactor baseline
/// (`cargo test --release -p unitprep-dedup -- --ignored --nocapture print_baseline`,
/// and the same without `--release`). Best of three per size.
#[test]
#[ignore = "prints timings; run with --nocapture"]
fn print_baseline() {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    for rows in [800, 2400, 5000] {
        let runs: Vec<_> = (0..3).map(|_| time_run(rows)).collect();
        let default_run = runs.iter().map(|r| r.0).min().unwrap();
        let matched_run = runs.iter().map(|r| r.1).min().unwrap();
        println!(
            "BASELINE {profile} rows={rows} default={default_run:?} matched_by_name={matched_run:?}"
        );
    }
}
