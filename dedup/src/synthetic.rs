//! Deterministic synthetic facility data (no real tenants), shared by this
//! crate's speed guards and the root crate's pipeline benchmarks (enabled
//! there through the `test-support` feature).

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
pub fn synthetic_facility(rows: usize) -> Vec<TenantRecord> {
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
