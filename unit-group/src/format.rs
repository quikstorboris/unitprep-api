// Vendor-format recognition and field mapping for unit-list source files.
//
// Discovery used to hard-code exactly one shape (QSX's own header names).
// Onboarding a second PMS export (DoorSwap) that uses a completely
// different vocabulary for the same two concepts — a unit identifier and
// a group/dimension descriptor — means every vendor now goes through the
// same recognize -> confirm-or-map flow, QSX included. Adding a third
// vendor later should mean adding one more `VendorFormat` entry here, not
// touching discovery's control flow again.
//
// The static target-field list below is deliberately the literal union of
// both known vendors' own raw headers, not an invented abstract schema —
// see the project's design notes. Only `Number` and `UnitGroup` are
// actually required by the rest of the pipeline (`validate_document`,
// `build_batch_from_documents`); everything else is carried through for
// the optional cross-check validations when a vendor happens to supply it.

use unitprep_core::csv_document::CsvDocument;

pub struct VendorFormat {
    pub name: &'static str,

    /// Headers that must all be present (via `CsvDocument::header_index`,
    /// so case/separator-insensitive) for a document to be recognized as
    /// this vendor's export.
    pub signature_headers: &'static [&'static str],

    /// (canonical target field, this vendor's own header for it) pairs.
    /// Hand-authored, not derived by matching names against
    /// `CANONICAL_TARGET_FIELDS` — a vendor's raw vocabulary is often
    /// *part of* that union list under its own literal name (DoorSwap's
    /// `Unit`/`Unit Type` are two such entries), so a name-matching rule
    /// would leave the canonical `Number`/`UnitGroup` columns empty. Every
    /// vendor must explicitly say which of its own headers is the unit
    /// identifier and which is the group/dimension descriptor.
    pub default_mapping: &'static [(&'static str, &'static str)],
}

/// The union of every known vendor's real, distinct raw headers — QSX
/// first (its headers already equal today's canonical names, since QSX is
/// the format the canonical vocabulary was originally bootstrapped from),
/// then DoorSwap's additional fields. No overlap between the two lists.
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

pub const QSX: VendorFormat = VendorFormat {
    name: "QSX",
    // Unchanged from discovery's original check — already proven against
    // the real export (`KAH_QMS_Units_Template.csv`).
    signature_headers: &["UnitGroup", "Number", "Category"],
    default_mapping: &[
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
        ("Covered", "Covered"),
        ("DoorType", "DoorType"),
        ("DoorWidth", "DoorWidth"),
        ("DoorHeight", "DoorHeight"),
        ("NearElevator", "NearElevator"),
        ("BottleCapacity", "BottleCapacity"),
        ("Floor", "Floor"),
        ("ClimateControlled", "ClimateControlled"),
        ("Class", "Class"),
        ("Power", "Power"),
        ("Alarm", "Alarm"),
        ("DriveUpAccess", "DriveUpAccess"),
        ("Furnished", "Furnished"),
        ("Lighting", "Lighting"),
        ("Area", "Area"),
        ("DoorCount", "DoorCount"),
        ("ConversionType", "ConversionType"),
    ],
};

pub const DOOR_SWAP: VendorFormat = VendorFormat {
    name: "DoorSwap",
    signature_headers: &["Unit", "Unit Type", "Status", "Customer"],
    default_mapping: &[
        // The actual translation — DoorSwap's own identifier/descriptor
        // columns feed the canonical fields the pipeline requires.
        ("Number", "Unit"),
        ("UnitGroup", "Unit Type"),
        ("Status", "Status"),
        ("Customer", "Customer"),
        ("Phone", "Phone"),
        ("Cell Phone", "Cell Phone"),
        ("Email", "Email"),
        ("Balance", "Balance"),
    ],
};

pub const VENDOR_FORMATS: &[VendorFormat] = &[QSX, DOOR_SWAP];

/// A resolved field mapping: one entry per canonical target field, with
/// the source header (exact spelling as it appears in the document being
/// mapped) that supplies it, or `None` if that target has nothing mapped.
pub type FieldMapping = Vec<(String, Option<String>)>;

/// Returns the first registered vendor whose full signature is present in
/// `document`'s headers, or `None` if it matches none of them.
pub fn detect_vendor(
    document: &CsvDocument,
) -> Option<&'static VendorFormat> {
    VENDOR_FORMATS.iter().find(|vendor| {
        vendor
            .signature_headers
            .iter()
            .all(|header| {
                document.header_index(header).is_some()
            })
    })
}

/// Builds the field mapping a "confirm this vendor" action applies:
/// every canonical target field, mapped to that vendor's declared source
/// header where it has one declared, `None` otherwise.
pub fn mapping_from_vendor(vendor: &VendorFormat) -> FieldMapping {
    CANONICAL_TARGET_FIELDS
        .iter()
        .map(|target| {
            let source = vendor
                .default_mapping
                .iter()
                .find(|(t, _)| t == target)
                .map(|(_, source)| source.to_string());

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
pub fn apply_field_mapping(
    document: &CsvDocument,
    mapping: &FieldMapping,
) -> CsvDocument {
    let mapped: Vec<(&str, usize)> = mapping
        .iter()
        .filter_map(|(target, source)| {
            let source = source.as_ref()?;
            let index = document.header_index(source)?;
            Some((target.as_str(), index))
        })
        .collect();

    let source_indices: Vec<usize> =
        mapped.iter().map(|(_, index)| *index).collect();

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
                .map(|&index| {
                    row.get(index)
                        .cloned()
                        .unwrap_or_default()
                })
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
