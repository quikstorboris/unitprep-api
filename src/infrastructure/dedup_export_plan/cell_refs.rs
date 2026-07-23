//! Spreadsheet-style cell-reference annotation for correction notes —
//! a distinct concern from `dedup_export_plan`'s own row/column
//! structure (write order, blank separators): this module only
//! answers "which fields should a note cite, and what cell references
//! do they turn into." Split out once the CSV exporter crossed the
//! project's 250-line flag point and it became clear this was a
//! genuinely separable concept, not just a line-count trim; promoted
//! to a shared module once the xlsx exporter needed the exact same
//! answer (specifically `first_cell_ref`, for the hyperlink target).

use unitprep_dedup::comparison::find_differing_categories;
use unitprep_dedup::types::{FieldCategory, FieldMismatch, FieldName, TenantRecord, TypoVariantCandidate};

use super::COLUMNS;

/// Every field named across a flagged group's mismatches, in the same
/// (category-priority, then declaration) order `find_differing_categories`
/// already produced them in — no reordering/dedup needed since each field
/// belongs to exactly one category.
pub(super) fn cite_fields_for_mismatches(mismatches: &[FieldMismatch]) -> Vec<FieldName> {
    mismatches.iter().flat_map(|m| m.fields.iter().map(|f| f.field)).collect()
}

/// A typo-variant candidate always differs by name (that's the whole
/// premise), so `FirstName`/`LastName` are always cited. If contact info
/// doesn't already match, also cite every other differing field —
/// recomputed here from the combined records since `TypoVariantCandidate`
/// only carries a bool, not the field-level detail (unlike `FlaggedGroup`,
/// which already stores its mismatches).
pub(super) fn typo_variant_cite_fields(
    candidate: &TypoVariantCandidate,
    combined_records: &[TenantRecord],
) -> Vec<FieldName> {
    let mut fields = vec![FieldName::FirstName, FieldName::LastName];
    if !candidate.contact_info_matches {
        for mismatch in find_differing_categories(combined_records) {
            if mismatch.category != FieldCategory::Name {
                fields.extend(mismatch.fields.iter().map(|f| f.field));
            }
        }
    }
    fields
}

/// Appends spreadsheet-style cell references to `base_note` — one clause
/// per cited field, e.g. `"AlternateContactPhoneNumber:
/// S7=3605525629, S8=(blank)"` — computed from the *output* file's own
/// column layout (`COLUMNS`, not the source file's) and the row numbers
/// these particular records are about to be written at. Mirrors the
/// reference script's own `note_with_refs` bracket format exactly
/// (`note + "  [" + refs + "]"`).
pub(super) fn note_with_cell_refs(
    base_note: &str,
    records: &[TenantRecord],
    cite_fields: &[FieldName],
    first_row: usize,
) -> String {
    if base_note.is_empty() || cite_fields.is_empty() {
        return base_note.to_string();
    }

    let ref_clauses: Vec<String> = cite_fields
        .iter()
        .filter_map(|field| {
            let column_name = csv_column_name(*field);
            let column_index = COLUMNS.iter().position(|c| *c == column_name)?;
            let letter = col_letter(column_index);

            let cell_refs: Vec<String> = records
                .iter()
                .enumerate()
                .map(|(i, record)| {
                    let raw = record.field(*field).trim();
                    let value = if raw.is_empty() { "(blank)" } else { raw };
                    format!("{letter}{}={value}", first_row + i)
                })
                .collect();

            Some(format!("{column_name}: {}", cell_refs.join(", ")))
        })
        .collect();

    if ref_clauses.is_empty() {
        base_note.to_string()
    } else {
        format!("{base_note}  [{}]", ref_clauses.join("; "))
    }
}

/// The structured counterpart to `note_with_cell_refs` — the cell
/// references for one field across a cluster of `record_count` records
/// starting at `first_row`, without the flat-string bracket formatting.
/// Used to attach real cell references to the on-screen note bullets
/// the API returns (see `api::dedup_view`), computed from the exact same
/// column layout (`COLUMNS`) and row-numbering convention as the actual
/// exported file, so an on-screen reference always points at the same
/// cell the export would put it in. Returns an empty `Vec` if `field`
/// isn't one of the export's own columns (shouldn't happen for any
/// `FieldName` this crate defines, but this stays a lookup rather than
/// an assumption, matching `note_with_cell_refs`'s own `?`-based
/// lookup above).
pub(crate) fn field_cell_refs(field: FieldName, first_row: usize, record_count: usize) -> Vec<String> {
    let column_name = csv_column_name(field);
    let Some(column_index) = COLUMNS.iter().position(|c| *c == column_name) else {
        return Vec::new();
    };
    let letter = col_letter(column_index);

    (0..record_count).map(|i| format!("{letter}{}", first_row + i)).collect()
}

/// The single cell reference (e.g. `"T7"`) that `note_with_cell_refs`
/// would put first — used as the xlsx hyperlink target. A cell can
/// only carry one hyperlink, so when a note cites several fields (or a
/// field with several differing values), this picks one deterministic
/// destination rather than trying to link every citation
/// independently. In practice this lands the reader in the right
/// neighborhood regardless, since every cited cell always belongs to
/// the same small group of nearby rows.
pub(super) fn first_cell_ref(cite_fields: &[FieldName], first_row: usize) -> Option<String> {
    let field = *cite_fields.first()?;
    let column_name = csv_column_name(field);
    let column_index = COLUMNS.iter().position(|c| *c == column_name)?;
    Some(format!("{}{}", col_letter(column_index), first_row))
}

/// 0-based column index -> spreadsheet column letter(s) (0 -> A, 25 ->
/// Z, 26 -> AA, ...). Mirrors the reference script's own `col_letter`.
fn col_letter(index0: usize) -> String {
    let mut idx = index0 + 1;
    let mut letters = String::new();
    while idx > 0 {
        let rem = (idx - 1) % 26;
        letters.insert(0, (b'A' + rem as u8) as char);
        idx = (idx - 1) / 26;
    }
    letters
}

/// Maps this crate's internal `FieldName` to the export file's own
/// column name — they diverge for alternate-contact fields
/// (`AltContact*` internally vs. `AlternateContact*` in the output
/// header), so this can't just be `format!("{field:?}")` the way the
/// note-composer's plain-English detail can.
fn csv_column_name(field: FieldName) -> &'static str {
    match field {
        FieldName::PhoneNumber => "PhoneNumber",
        FieldName::PhoneNumberPrefix => "PhoneNumberPrefix",
        FieldName::Email => "Email",
        FieldName::AddressStreet1 => "AddressStreet1",
        FieldName::AddressStreet2 => "AddressStreet2",
        FieldName::AddressCity => "AddressCity",
        FieldName::AddressState => "AddressState",
        FieldName::AddressPostalCode => "AddressPostalCode",
        FieldName::AltContactFirstName => "AlternateContactFirstName",
        FieldName::AltContactLastName => "AlternateContactLastName",
        FieldName::AltContactEmail => "AlternateContactEmail",
        FieldName::AltContactPhoneNumber => "AlternateContactPhoneNumber",
        FieldName::AltContactPhoneNumberPrefix => "AlternateContactPhoneNumberPrefix",
        FieldName::AltContactAddressStreet1 => "AlternateContactAddressStreet1",
        FieldName::AltContactAddressStreet2 => "AlternateContactAddressStreet2",
        FieldName::AltContactAddressCity => "AlternateContactAddressCity",
        FieldName::AltContactAddressState => "AlternateContactAddressState",
        FieldName::AltContactAddressPostalCode => "AlternateContactAddressPostalCode",
        FieldName::CompanyName => "CompanyName",
        FieldName::FirstName => "FirstName",
        FieldName::LastName => "LastName",
    }
}

#[cfg(test)]
#[path = "cell_refs_tests.rs"]
mod tests;
