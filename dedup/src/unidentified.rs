//! Tenants the source system gave no customer id.
//!
//! When most rows carry a vendor tenant id but some do not (a Winsen unit
//! that is missing from the rent roll, say), those rows can't be grouped
//! reliably and are held out of the main check instead of being guessed
//! at. The user then decides: ignore them, or match them by name against
//! everyone. This module builds that separate section of the report.
//!
//! A file with no ids at all (QSX, say) is not affected: every row is
//! grouped by name, as always, and nothing is held out.

use serde::{Deserialize, Serialize};

use crate::comparison::find_differing_categories;
use crate::grouping::{group_key, group_records};
use crate::note_composer::NoteComposer;
use crate::relatedness::{find_related_tenant_candidates, RelatedTenantCandidate};
use crate::similarity::find_typo_variant_candidates;
use crate::types::{FlaggedGroup, TenantGroup, TenantRecord, TypoVariantCandidate};

/// What the user chose to do with the tenants that have no customer id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnidentifiedMode {
    /// Not decided yet: the tenants are listed and the user is asked.
    #[default]
    Pending,
    /// The user chose to leave them out of the matching.
    Ignored,
    /// The user chose to match them by name against every tenant.
    MatchedByName,
}

/// A customer id that already holds a tenant of the same name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentifiedMatch {
    pub tenant_id: String,
    pub display_name: String,
    pub units: Vec<String>,
}

/// One name among the tenants without a customer id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnidentifiedTenant {
    pub key: String,
    pub display_name: String,
    pub units: Vec<String>,
    /// Customer ids that hold someone with this exact name. Filled in only
    /// once the user chose to match by name.
    pub same_name_as: Vec<IdentifiedMatch>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnidentifiedTenants {
    pub mode: UnidentifiedMode,
    pub tenants: Vec<UnidentifiedTenant>,
    /// The rest are filled in only for `MatchedByName`: findings that
    /// involve at least one tenant without an id, found by comparing
    /// everyone by name.
    pub flagged_groups: Vec<FlaggedGroup>,
    pub typo_variant_candidates: Vec<TypoVariantCandidate>,
    pub related_tenant_candidates: Vec<RelatedTenantCandidate>,
}

/// Splits `records` into `(with an id, without one)`, or `None` when no
/// holding out is needed: no row has an id (the format has none) or every
/// row has one.
pub fn split_by_identification(
    records: &[TenantRecord],
) -> Option<(Vec<TenantRecord>, Vec<TenantRecord>)> {
    let has_id = |r: &TenantRecord| !r.tenant_id.trim().is_empty();
    let identified: Vec<TenantRecord> = records.iter().filter(|r| has_id(r)).cloned().collect();
    let unidentified: Vec<TenantRecord> = records.iter().filter(|r| !has_id(r)).cloned().collect();

    if identified.is_empty() || unidentified.is_empty() {
        return None;
    }
    Some((identified, unidentified))
}

fn units_of(records: &[TenantRecord]) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    for record in records {
        let unit = record.unit_number.trim();
        if !unit.is_empty() && !units.iter().any(|u| u == unit) {
            units.push(unit.to_string());
        }
    }
    units
}

pub fn analyze(
    identified: &[TenantRecord],
    unidentified: &[TenantRecord],
    mode: UnidentifiedMode,
    composer: &dyn NoteComposer,
) -> UnidentifiedTenants {
    let unid_groups = group_records(unidentified.to_vec());

    let mut tenants: Vec<UnidentifiedTenant> = unid_groups
        .iter()
        .map(|group| UnidentifiedTenant {
            key: group.key.clone(),
            display_name: group.records[0].display_name(),
            units: units_of(&group.records),
            same_name_as: Vec::new(),
        })
        .collect();

    if mode != UnidentifiedMode::MatchedByName {
        return UnidentifiedTenants {
            mode,
            tenants,
            ..UnidentifiedTenants::default()
        };
    }

    // Every tenant compared by name, the id-less ones included.
    let mut all: Vec<TenantRecord> = identified.to_vec();
    all.extend_from_slice(unidentified);
    let all_groups: Vec<TenantGroup> = group_records(all);

    let unid_keys: Vec<&str> = unid_groups.iter().map(|g| g.key.as_str()).collect();
    let involves_unidentified = |key: &str| unid_keys.contains(&key);

    for tenant in &mut tenants {
        let mut matches: Vec<IdentifiedMatch> = Vec::new();
        for record in identified {
            if group_key(&record.first_last) != tenant.key {
                continue;
            }
            match matches.iter_mut().find(|m| m.tenant_id == record.tenant_id) {
                Some(existing) => {
                    let unit = record.unit_number.trim();
                    if !unit.is_empty() && !existing.units.iter().any(|u| u == unit) {
                        existing.units.push(unit.to_string());
                    }
                }
                None => matches.push(IdentifiedMatch {
                    tenant_id: record.tenant_id.clone(),
                    display_name: record.display_name(),
                    units: units_of(std::slice::from_ref(record)),
                }),
            }
        }
        tenant.same_name_as = matches;
    }

    let flagged_groups: Vec<FlaggedGroup> = all_groups
        .iter()
        .filter(|g| involves_unidentified(&g.key) && g.records.len() >= 2)
        .filter_map(|group| {
            let differing = find_differing_categories(&group.records);
            if differing.is_empty() {
                return None;
            }
            let note = composer.compose_group_note(group, &differing);
            Some(FlaggedGroup {
                group: group.clone(),
                mismatches: differing,
                note,
            })
        })
        .collect();

    let typo_variant_candidates = find_typo_variant_candidates(&all_groups, composer)
        .into_iter()
        .filter(|c| involves_unidentified(&c.key_a) || involves_unidentified(&c.key_b))
        .collect();

    let related_tenant_candidates = find_related_tenant_candidates(&all_groups, composer)
        .into_iter()
        .filter(|c| c.group_keys.iter().any(|k| involves_unidentified(k)))
        .collect();

    UnidentifiedTenants {
        mode,
        tenants,
        flagged_groups,
        typo_variant_candidates,
        related_tenant_candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note_composer::TemplateNoteComposer;

    fn rec(id: &str, name: &str, unit: &str, email: &str) -> TenantRecord {
        TenantRecord {
            tenant_id: id.to_string(),
            first_last: name.to_string(),
            unit_number: unit.to_string(),
            email: email.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn a_file_with_no_ids_at_all_holds_nothing_out() {
        let records = vec![rec("", "Ann Lee", "1", ""), rec("", "Bo Ray", "2", "")];
        assert!(split_by_identification(&records).is_none());
    }

    #[test]
    fn a_file_where_every_row_has_an_id_holds_nothing_out() {
        let records = vec![rec("5", "Ann Lee", "1", ""), rec("6", "Bo Ray", "2", "")];
        assert!(split_by_identification(&records).is_none());
    }

    #[test]
    fn rows_without_an_id_are_split_from_those_with_one() {
        let records = vec![
            rec("5", "Ann Lee", "1", ""),
            rec("", "Bo Ray", "2", ""),
            rec("  ", "Cy Poe", "3", ""),
        ];
        let (with, without) = split_by_identification(&records).unwrap();
        assert_eq!(with.len(), 1);
        assert_eq!(without.len(), 2);
    }

    #[test]
    fn before_the_user_chooses_the_tenants_are_only_listed() {
        let identified = vec![rec("5", "Ann Lee", "1", "")];
        let unidentified = vec![rec("", "Ann Lee", "9", ""), rec("", "Bo Ray", "8", "")];
        let out = analyze(
            &identified,
            &unidentified,
            UnidentifiedMode::Pending,
            &TemplateNoteComposer,
        );
        assert_eq!(out.tenants.len(), 2);
        assert!(out.tenants.iter().all(|t| t.same_name_as.is_empty()));
        assert!(out.flagged_groups.is_empty());
    }

    #[test]
    fn matching_by_name_finds_the_customer_id_that_already_holds_that_name() {
        let identified = vec![
            rec("5", "Ann Lee", "1", "ann@x.com"),
            rec("5", "Ann Lee", "2", "ann@x.com"),
        ];
        let unidentified = vec![rec("", "ann  lee", "9", "other@x.com")];
        let out = analyze(
            &identified,
            &unidentified,
            UnidentifiedMode::MatchedByName,
            &TemplateNoteComposer,
        );

        let tenant = &out.tenants[0];
        assert_eq!(tenant.same_name_as.len(), 1);
        assert_eq!(tenant.same_name_as[0].tenant_id, "5");
        assert_eq!(tenant.same_name_as[0].units, ["1", "2"]);

        assert_eq!(out.flagged_groups.len(), 1, "the email differs");
    }

    #[test]
    fn matching_by_name_compares_with_every_tenant_including_other_unidentified_ones() {
        let identified = vec![rec("5", "Zed Zimmer", "1", "")];
        let unidentified = vec![
            rec("", "John Smith", "7", "a@x.com"),
            rec("", "John Smith", "8", "b@x.com"),
        ];
        let out = analyze(
            &identified,
            &unidentified,
            UnidentifiedMode::MatchedByName,
            &TemplateNoteComposer,
        );
        assert_eq!(out.tenants.len(), 1, "one name over two units");
        assert_eq!(out.tenants[0].units, ["7", "8"]);
        assert_eq!(out.flagged_groups.len(), 1);
    }

    #[test]
    fn a_finding_between_two_identified_tenants_is_not_part_of_this_section() {
        let identified = vec![
            rec("5", "John Smith", "1", "a@x.com"),
            rec("6", "Jon Smith", "2", "a@x.com"),
        ];
        let unidentified = vec![rec("", "Maria Garcia", "9", "")];
        let out = analyze(
            &identified,
            &unidentified,
            UnidentifiedMode::MatchedByName,
            &TemplateNoteComposer,
        );
        assert!(out.typo_variant_candidates.is_empty());
        assert!(out.related_tenant_candidates.is_empty());
    }
}
