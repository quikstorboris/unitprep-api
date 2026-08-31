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
