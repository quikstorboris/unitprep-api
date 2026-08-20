use super::*;

fn document(headers: Vec<&str>, rows: Vec<Vec<&str>>) -> CsvDocument {
    CsvDocument {
        file_name: "units.csv".to_string(),
        headers: headers.into_iter().map(str::to_string).collect(),
        rows: rows
            .into_iter()
            .map(|row| row.into_iter().map(str::to_string).collect())
            .collect(),
        modified_at: None,
    }
}

/// Hand-built fixtures mirroring the `client_ops.vendor_format` seed
/// data (registry migration) exactly -- vendor recognition itself moved
/// to DB-backed data, so these tests exercise the pure `detect_vendor`/
/// `mapping_from_vendor`/`apply_field_mapping` logic against a
/// representative `Vec<VendorFormat>` rather than a live database. Keep
/// this in sync with the migration's seed rows by hand; there's no
/// automated link between the two, the same trade-off any test fixture
/// mirroring seed data makes.
fn vendor(name: &str, signature: &[&str], mapping: &[(&str, &str)]) -> VendorFormat {
    VendorFormat {
        name: name.to_string(),
        content_type: ContentType::Units,
        signature_headers: signature.iter().map(|s| s.to_string()).collect(),
        field_mapping: mapping
            .iter()
            .map(|(t, s)| (t.to_string(), s.to_string()))
            .collect(),
        transform_key: None,
    }
}

fn qsx() -> VendorFormat {
    vendor(
        "QSX",
        &["UnitGroup", "Number", "Category"],
        &[
            ("Number", "Number"),
            ("UnitGroup", "UnitGroup"),
            ("Category", "Category"),
            ("StandardRate", "StandardRate"),
            ("Active", "Active"),
            ("Damaged", "Damaged"),
            ("Width", "Width"),
            ("Length", "Length"),
            ("Height", "Height"),
            ("InsideOutside", "InsideOutside"),
        ],
    )
}

fn storage_commander() -> VendorFormat {
    vendor(
        "Storage Commander",
        &["UnitGroup", "Number", "Category", "Locality"],
        &[
            ("Number", "Number"),
            ("UnitGroup", "UnitGroup"),
            ("Category", "Category"),
            ("InsideOutside", "Locality"),
            ("MonitoringEnabled", "MonitoringEnabled"),
            ("SmartLockEnabled", "SmartLockEnabled"),
        ],
    )
}

fn door_swap() -> VendorFormat {
    vendor(
        "DoorSwap",
        &["Unit", "Unit Type", "Status", "Customer"],
        &[
            ("Number", "Unit"),
            ("UnitGroup", "Unit Type"),
            ("Status", "Status"),
            ("Customer", "Customer"),
            ("Phone", "Phone"),
            ("Cell Phone", "Cell Phone"),
            ("Email", "Email"),
            ("Balance", "Balance"),
        ],
    )
}

/// Storage Commander before QSX, mirroring `VENDOR_FORMATS`' old
/// ordering and the registry migration's own seed-row order: Storage
/// Commander's real export also satisfies plain QSX's own (smaller)
/// signature, so it must be checked first or every Storage Commander
/// export would misclassify as QSX.
fn all_vendors() -> Vec<VendorFormat> {
    vec![storage_commander(), qsx(), door_swap()]
}

#[test]
fn detects_qsx_by_its_real_export_headers() {
    // The exact header set from the real QSX export
    // (`KAH_QMS_Units_Template.csv`), not a hypothetical subset.
    let doc = document(
        vec![
            "Number",
            "UnitGroup",
            "Category",
            "StandardRate",
            "Active",
            "Damaged",
            "Width",
            "Length",
            "Height",
        ],
        vec![],
    );

    let vendors = all_vendors();
    assert_eq!(
        detect_vendor(&doc, &vendors).map(|v| v.name.as_str()),
        Some("QSX")
    );
}

/// Storage Commander's signature (`UnitGroup`/`Number`/`Category`) is a
/// strict subset of QSX's own -- without the extra `Locality` header this
/// vendor requires, this exact fixture (the real "Absolute Storage of
/// Franklin Park" export, trimmed to its distinguishing columns) would
/// misclassify as QSX.
#[test]
fn detects_storage_commander_by_its_real_export_headers() {
    let doc = document(
        vec![
            "Number",
            "UnitGroup",
            "Category",
            "StandardRate",
            "Active",
            "Damaged",
            "Width",
            "Length",
            "Height",
            "Locality",
            "MonitoringEnabled",
            "SmartLockEnabled",
        ],
        vec![],
    );

    let vendors = all_vendors();
    assert_eq!(
        detect_vendor(&doc, &vendors).map(|v| v.name.as_str()),
        Some("Storage Commander")
    );
}

#[test]
fn storage_commander_default_mapping_translates_locality_to_inside_outside() {
    let storage_commander = storage_commander();
    let mapping = mapping_from_vendor(&storage_commander);

    let inside_outside_source = mapping
        .iter()
        .find(|(target, _)| target == "InsideOutside")
        .and_then(|(_, source)| source.clone());

    assert_eq!(inside_outside_source, Some("Locality".to_string()));
}

/// A true QSX export (no `Locality` column at all) must still detect as
/// QSX, not Storage Commander -- Storage Commander's extra required header
/// is what keeps the two apart in both directions.
#[test]
fn a_true_qsx_export_without_locality_still_detects_as_qsx() {
    let doc = document(
        vec!["Number", "UnitGroup", "Category", "InsideOutside"],
        vec![],
    );

    let vendors = all_vendors();
    assert_eq!(
        detect_vendor(&doc, &vendors).map(|v| v.name.as_str()),
        Some("QSX")
    );
}

#[test]
fn detects_door_swap_by_its_real_export_headers() {
    let doc = document(
        vec![
            "Unit",
            "Status",
            "Unit Type",
            "Customer",
            "Phone",
            "Cell Phone",
            "Email",
            "Balance",
        ],
        vec![],
    );

    let vendors = all_vendors();
    assert_eq!(
        detect_vendor(&doc, &vendors).map(|v| v.name.as_str()),
        Some("DoorSwap")
    );
}

#[test]
fn detects_neither_for_unrelated_headers() {
    let doc = document(vec!["Foo", "Bar", "Baz"], vec![]);

    let vendors = all_vendors();
    assert!(detect_vendor(&doc, &vendors).is_none());
}

#[test]
fn door_swap_default_mapping_translates_unit_and_unit_type() {
    let door_swap = door_swap();
    let mapping = mapping_from_vendor(&door_swap);

    let number_source = mapping
        .iter()
        .find(|(target, _)| target == "Number")
        .and_then(|(_, source)| source.clone());

    let unit_group_source = mapping
        .iter()
        .find(|(target, _)| target == "UnitGroup")
        .and_then(|(_, source)| source.clone());

    assert_eq!(number_source, Some("Unit".to_string()));
    assert_eq!(unit_group_source, Some("Unit Type".to_string()));

    // A canonical field DoorSwap never supplies stays unmapped.
    let category_source = mapping
        .iter()
        .find(|(target, _)| target == "Category")
        .and_then(|(_, source)| source.clone());

    assert_eq!(category_source, None);
}

#[test]
fn qsx_default_mapping_is_identity() {
    let qsx = qsx();
    let mapping = mapping_from_vendor(&qsx);

    let unit_group_source = mapping
        .iter()
        .find(|(target, _)| target == "UnitGroup")
        .and_then(|(_, source)| source.clone());

    assert_eq!(unit_group_source, Some("UnitGroup".to_string()));
}

#[test]
fn apply_field_mapping_confirms_door_swap_into_canonical_columns() {
    let doc = document(
        vec!["Unit", "Status", "Unit Type", "Customer"],
        vec![vec![
            "1",
            "rented",
            "10x10 Non-Climate Controlled (10 x 10 x 8)",
            "Lexie Rodrigue",
        ]],
    );

    let door_swap = door_swap();
    let mapping = mapping_from_vendor(&door_swap);
    let normalized = apply_field_mapping(&doc, &mapping);

    let number_index = normalized.header_index("number").unwrap();
    let unit_group_index = normalized.header_index("unitgroup").unwrap();

    assert_eq!(normalized.rows[0][number_index], "1");
    assert_eq!(
        normalized.rows[0][unit_group_index],
        "10x10 Non-Climate Controlled (10 x 10 x 8)"
    );
}

/// Regression test for the actual bug this closes: DoorSwap never
/// supplies Width/Length (its dimensions live inside the UnitGroup-mapped
/// descriptor string instead), so those canonical fields must be absent
/// from the normalized document entirely — not present-but-blank. A
/// present-but-blank column reads to `validate_document`'s dimension
/// check as "this file has real width/length data, and it's invalid,"
/// which flagged every single DoorSwap unit as having invalid dimensions
/// the first time this was tried against the real export.
#[test]
fn apply_field_mapping_omits_canonical_fields_door_swap_never_maps() {
    let doc = document(
        vec!["Unit", "Status", "Unit Type", "Customer"],
        vec![vec![
            "1",
            "rented",
            "10x10 Non-Climate Controlled (10 x 10 x 8)",
            "Lexie Rodrigue",
        ]],
    );

    let door_swap = door_swap();
    let mapping = mapping_from_vendor(&door_swap);
    let normalized = apply_field_mapping(&doc, &mapping);

    assert_eq!(normalized.header_index("width"), None);
    assert_eq!(normalized.header_index("length"), None);
    assert_eq!(normalized.header_index("category"), None);

    // Only the 4 source columns this fixture's document actually has
    // survive — Phone/Cell Phone/Email/Balance are also part of
    // DoorSwap's default mapping, but absent from this row entirely.
    assert_eq!(
        normalized.headers,
        vec!["Number", "UnitGroup", "Status", "Customer"]
    );
}

#[test]
fn apply_field_mapping_omits_a_target_left_unmapped_in_a_partial_manual_mapping() {
    let doc = document(vec!["MyUnitId"], vec![vec!["A01"]]);

    let mapping: FieldMapping = vec![
        ("Number".to_string(), Some("MyUnitId".to_string())),
        ("UnitGroup".to_string(), None),
    ];

    let normalized = apply_field_mapping(&doc, &mapping);

    assert_eq!(normalized.headers, vec!["Number"]);
    assert_eq!(normalized.rows[0], vec!["A01"]);
    assert_eq!(normalized.header_index("unitgroup"), None);
}

#[test]
fn apply_field_mapping_omits_a_target_whose_source_header_is_not_actually_in_the_document() {
    let doc = document(vec!["Unit"], vec![vec!["A01"]]);

    let mapping: FieldMapping = vec![("Number".to_string(), Some("DoesNotExist".to_string()))];

    let normalized = apply_field_mapping(&doc, &mapping);

    assert!(normalized.headers.is_empty());
    assert!(normalized.rows[0].is_empty());
}
