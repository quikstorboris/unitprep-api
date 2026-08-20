// Group Prep's own canonical target-field list and the manual-mapping
// machinery built around it. Vendor *recognition* (signature headers,
// detect_vendor, per-vendor default mappings) used to live in this file
// as hardcoded QSX/Storage Commander/DoorSwap consts; it's now shared,
// DB-backed data in `client_ops.vendor_format`, read through
// `unitprep_core::vendor_format` (re-exported below) so both this crate
// and `unitprep-dedup` use the exact same recognition mechanics instead
// of each growing its own copy. See that migration and
// `core::vendor_format`'s doc comment for the full reasoning.
//
// What's left here is genuinely tool-specific: which canonical fields
// Group Prep's own pipeline requires, and the "every target field, with
// an explicit `None` for anything unmapped" shape the manual-mapping UI
// needs (`FieldMapping`) — a different shape from
// `core::VendorFormat::field_mapping`, which only ever lists fields a
// vendor actually supplies.

use unitprep_core::csv_document::CsvDocument;
pub use unitprep_core::vendor_format::{detect_vendor, ContentType, VendorFormat};

/// The union of every known unit-file vendor's real, distinct raw
/// headers — QSX first (its headers already equal today's canonical
/// names, since QSX is the format the canonical vocabulary was
/// originally bootstrapped from), then Storage Commander's two extra
/// fields, then DoorSwap's additional fields. No overlap between the
/// lists. This stays a Rust constant (rather than also moving into the
/// DB) because it isn't vendor data — it's this crate's own pipeline
/// requirement, the same fact `REQUIRED_TARGET_FIELDS` below encodes; a
/// self-service "add a vendor" flow maps a new vendor's headers *onto*
/// this list, it never grows the list itself.
pub const CANONICAL_TARGET_FIELDS: &[&str] = &[
    "Number",
    "UnitGroup",
    "Category",
    "StandardRate",
    "Active",
    "Damaged",
    "Width",
    "Length",
    "Height",
    "InsideOutside",
    "Covered",
    "DoorType",
    "DoorWidth",
    "DoorHeight",
    "NearElevator",
    "BottleCapacity",
    "Floor",
    "ClimateControlled",
    "Class",
    "Power",
    "Alarm",
    "DriveUpAccess",
    "Furnished",
    "Lighting",
    "Area",
    "DoorCount",
    "ConversionType",
    "MonitoringEnabled",
    "SmartLockEnabled",
    "Unit",
    "Status",
    "Unit Type",
    "Customer",
    "Phone",
    "Cell Phone",
    "Email",
    "Balance",
];

/// The only two fields the pipeline actually consumes downstream — every
/// other canonical field is optional/informational. The manual-mapping UI
/// should refuse to submit until both of these have a real selection.
pub const REQUIRED_TARGET_FIELDS: &[&str] = &["Number", "UnitGroup"];

/// A resolved field mapping: one entry per canonical target field, with
/// the source header (exact spelling as it appears in the document being
/// mapped) that supplies it, or `None` if that target has nothing
/// mapped. Distinct from `VendorFormat::field_mapping` (which only lists
/// fields the vendor actually supplies) because the manual-mapping UI
/// needs to show and store a decision for every canonical field,
/// including "nothing," not just the ones a detected vendor happened to
/// declare.
pub type FieldMapping = Vec<(String, Option<String>)>;

/// Builds the field mapping a "confirm this vendor" action applies:
/// every canonical target field, mapped to that vendor's declared source
/// header where it has one declared, `None` otherwise.
pub fn mapping_from_vendor(vendor: &VendorFormat) -> FieldMapping {
    CANONICAL_TARGET_FIELDS
        .iter()
        .map(|target| {
            let source = vendor
                .field_mapping
                .iter()
                .find(|(t, _)| t == target)
                .map(|(_, source)| source.clone());

            (target.to_string(), source)
        })
        .collect()
}

/// Builds a new `CsvDocument` containing only the canonical target fields
/// that `mapping` actually maps to a real source column — each row's
/// values pulled from that source column in `document`. Unmapped targets
/// are dropped entirely, not included as a blank column: validation's
/// optional-column checks (width/length/locality/climate — see
/// `validation::ColumnIndices::discover`) treat a present-but-blank
/// column as "real data, and it's invalid" rather than "this vendor
/// never had this column," so a vendor that never supplies dimensions
/// (DoorSwap folds them into its UnitGroup-mapped descriptor string
/// instead) would otherwise have every row flagged for "Invalid
/// dimensions" purely because the column exists and is empty. Mirrors
/// `corrections::apply_corrections` in shape: a pure function producing a
/// new document rather than mutating the original, so the raw upload
/// stays a stable record of what was actually received.
pub fn apply_field_mapping(document: &CsvDocument, mapping: &FieldMapping) -> CsvDocument {
    let mapped: Vec<(&str, usize)> = mapping
        .iter()
        .filter_map(|(target, source)| {
            let source = source.as_ref()?;
            let index = document.header_index(source)?;
            Some((target.as_str(), index))
        })
        .collect();

    let source_indices: Vec<usize> = mapped.iter().map(|(_, index)| *index).collect();

    let headers: Vec<String> = mapped
        .iter()
        .map(|(target, _)| target.to_string())
        .collect();

    let rows: Vec<Vec<String>> = document
        .rows
        .iter()
        .map(|row| {
            source_indices
                .iter()
                .map(|&index| row.get(index).cloned().unwrap_or_default())
                .collect()
        })
        .collect();

    CsvDocument {
        file_name: document.file_name.clone(),
        headers,
        rows,
        modified_at: document.modified_at,
    }
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
