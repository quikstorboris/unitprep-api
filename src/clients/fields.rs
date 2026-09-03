//! Shared PS form-field lookup helpers used by both `intake_mapping`
//! and `merchant_account_mapping`.

// Phase 1 only -- no HTTP handler calls into `clients::*` yet. Remove
// once a real caller exists.
#![allow(dead_code)]

use crate::process_street::FormField;

/// Looks up a field by PS's own `key` and returns its value trimmed of
/// leading/trailing whitespace -- PS's own exports carry a fair amount
/// of incidental trailing whitespace (e.g. a real captured
/// `"Facility_City:"` value of `"Marengo "`), which is never meaningful
/// content. Trimming is normalization, not decomposition -- it doesn't
/// touch what's inside the value the way parsing an amount out of
/// `"$10.00 (One-Time)"` would.
pub fn value_for(fields: &[FormField], key: &str) -> Option<String> {
    let raw = fields
        .iter()
        .find(|f| f.key == key)
        .and_then(FormField::value_as_str)?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Looks up a `MultiChoice`-type field (PS's own checkbox-group fields,
/// e.g. `Accepted_Payment_Methods:`) and joins its selected values into
/// one comma-separated string -- same "keep PS's own raw text, never
/// decompose it" convention as everything else in this module, just for
/// a field whose own raw shape is a JSON array (`data.values`) rather
/// than a single string (`data.value`, what `value_as_str`/`value_for`
/// read). Confirmed against real Highway 20 data: this field type has
/// no singular `value` key at all, so `value_for` always returns `None`
/// for it -- this exists specifically to not silently drop that data.
pub fn values_for(fields: &[FormField], key: &str) -> Option<String> {
    let values = fields
        .iter()
        .find(|f| f.key == key)?
        .data
        .as_ref()?
        .get("values")?
        .as_array()?;

    let joined = values
        .iter()
        .filter_map(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(", ");

    (!joined.is_empty()).then_some(joined)
}

/// Tries each key in order, returning the first that has a value. Only
/// needed because PS's own New Merchant Account template inconsistently
/// spells "% Ownership in Business" as "...in Buisness" across some
/// owner slots but not others -- a real template typo, not something
/// this codebase controls.
pub fn value_for_any(fields: &[FormField], keys: &[String]) -> Option<String> {
    keys.iter().find_map(|k| value_for(fields, k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(key: &str, value: &str) -> FormField {
        serde_json::from_value(serde_json::json!({
            "id": "x", "taskId": "x", "key": key, "label": key, "fieldType": "Text",
            "data": {"value": value}
        }))
        .unwrap()
    }

    #[test]
    fn trims_whitespace_and_treats_blank_as_absent() {
        let fields = vec![field("City", "Marengo "), field("Blank", "   ")];
        assert_eq!(value_for(&fields, "City").as_deref(), Some("Marengo"));
        assert_eq!(value_for(&fields, "Blank"), None);
        assert_eq!(value_for(&fields, "Missing"), None);
    }

    fn multi_choice_field(key: &str, values: &[&str]) -> FormField {
        serde_json::from_value(serde_json::json!({
            "id": "x", "taskId": "x", "key": key, "label": key, "fieldType": "MultiChoice",
            "data": {"values": values}
        }))
        .unwrap()
    }

    #[test]
    fn values_for_joins_a_multi_choice_fields_selected_values() {
        let fields = vec![multi_choice_field("Accepted_Payment_Methods:", &["Cash", "Check", "Visa"])];
        assert_eq!(
            values_for(&fields, "Accepted_Payment_Methods:").as_deref(),
            Some("Cash, Check, Visa")
        );
    }

    #[test]
    fn values_for_returns_none_for_an_empty_or_missing_field() {
        let fields = vec![multi_choice_field("Empty", &[])];
        assert_eq!(values_for(&fields, "Empty"), None);
        assert_eq!(values_for(&fields, "Missing"), None);
    }

    #[test]
    fn value_for_any_tries_keys_in_order() {
        let fields = vec![field("Owner_2_-_%_Ownership_in_Business", "30.00")];
        let keys = vec![
            "Owner_2_-_%_Ownership_in_Buisness".to_string(),
            "Owner_2_-_%_Ownership_in_Business".to_string(),
        ];
        assert_eq!(value_for_any(&fields, &keys).as_deref(), Some("30.00"));
    }
}
