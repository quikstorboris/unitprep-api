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
use crate::types::TenantRecord;

const FIRST_NAMES: [&str; 40] = [
    "James",
    "Mary",
    "Robert",
    "Patricia",
    "John",
    "Jennifer",
    "Michael",
    "Linda",
    "David",
    "Elizabeth",
    "William",
    "Barbara",
    "Richard",
    "Susan",
    "Joseph",
    "Jessica",
    "Thomas",
    "Sarah",
    "Charles",
    "Karen",
    "Daniel",
    "Nancy",
    "Matthew",
    "Lisa",
    "Anthony",
    "Betty",
    "Mark",
    "Helen",
    "Donald",
    "Sandra",
    "Steven",
    "Donna",
    "Paul",
    "Carol",
    "Andrew",
    "Ruth",
    "Joshua",
    "Sharon",
    "Kenneth",
    "Michelle",
];

const LAST_NAMES: [&str; 40] = [
    "Smith",
    "Johnson",
    "Williams",
    "Brown",
    "Jones",
    "Garcia",
    "Miller",
    "Davis",
    "Rodriguez",
    "Martinez",
    "Hernandez",
    "Lopez",
    "Gonzalez",
    "Wilson",
    "Anderson",
    "Thomas",
    "Taylor",
    "Moore",
    "Jackson",
    "Martin",
    "Lee",
    "Perez",
    "Thompson",
    "White",
    "Harris",
    "Sanchez",
    "Clark",
    "Ramirez",
    "Lewis",
    "Robinson",
    "Walker",
    "Young",
    "Allen",
    "King",
    "Wright",
    "Scott",
    "Torres",
    "Nguyen",
    "Hill",
    "Flores",
];

/// A tiny deterministic generator, so the dataset (and therefore the work
/// the pipeline does) is identical on every machine and run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound
    }
}

/// `rows` unit records for roughly `rows * 0.8` distinct tenants: a mix of
/// single- and multi-unit tenants, some tenants carrying a vendor tenant
/// id and some not, a few near-duplicate names (typos), and contact
/// details that mostly agree.
fn synthetic_facility(rows: usize) -> Vec<TenantRecord> {
    let mut rng = Lcg(0x5EED);
    let mut records = Vec::with_capacity(rows);
    let mut tenant = 0usize;

    while records.len() < rows {
        tenant += 1;
        let first = FIRST_NAMES[rng.next(FIRST_NAMES.len())];
        let mut last = LAST_NAMES[rng.next(LAST_NAMES.len())].to_string();
        if rng.next(30) == 0 {
            last.push('s'); // a typo-style near duplicate of another tenant
        }
        let units = if rng.next(5) == 0 { 2 } else { 1 };
        let has_id = rng.next(2) == 0;

        for unit in 0..units {
            if records.len() >= rows {
                break;
            }
            records.push(TenantRecord {
                cust_numb: format!("C{}", records.len()),
                unit_number: format!("{}-{}", tenant, unit),
                tenant_id: if has_id {
                    format!("T{tenant}")
                } else {
                    String::new()
                },
                first_last: format!("{first} {last}").to_lowercase(),
                first_name: first.to_string(),
                last_name: last.clone(),
                phone_number: format!("575555{:04}", tenant % 10_000),
                email: format!(
                    "{}.{}{}@example.com",
                    first.to_lowercase(),
                    last.to_lowercase(),
                    tenant
                ),
                address_street1: format!("{} Main St", 100 + tenant),
                address_city: "Alamogordo".to_string(),
                address_state: "NM".to_string(),
                address_postal_code: "88310".to_string(),
                ..Default::default()
            });
        }
    }
    records
}

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
