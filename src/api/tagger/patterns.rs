//! Loading the admin-defined label-proximity patterns the recognizer uses.

use serde::Deserialize;
use unitprep_template_tagger::{LabelPosition, LabelProximityPattern};

#[derive(Debug, Deserialize)]
pub(super) struct LabelProximityPatternJson {
    pub(super) label: String,
    pub(super) position: String,
    pub(super) max_gap_chars: usize,
    #[serde(default)]
    pub(super) requires_preceding_anchor: Option<PrecedingAnchorJson>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PrecedingAnchorJson {
    pub(super) text: String,
    pub(super) within_chars: usize,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct PatternRow {
    pub(super) tag_key: String,
    pub(super) pattern: serde_json::Value,
}

/// Loads every active `label_proximity` pattern from `client_ops.tag_pattern`.
/// A row whose `pattern` JSONB doesn't parse into the expected shape, or
/// names an unrecognized `position`, is logged and skipped rather than
/// failing the whole request -- one malformed pattern (most likely from
/// hand-authored data, since there's no admin UI for this table yet)
/// should not block every other tag from being recognized.
pub(super) async fn load_label_proximity_patterns(
    tx: &mut sqlx::PgConnection,
) -> Result<Vec<LabelProximityPattern>, sqlx::Error> {
    // sentence_pattern rows (the other `kind`) and qms_tag.value_shape aren't consumed here yet.
    let rows: Vec<PatternRow> = sqlx::query_as(
        "SELECT tag_key, pattern FROM client_ops.tag_pattern
          WHERE kind = 'label_proximity' AND is_active = true",
    )
    .fetch_all(tx)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let parsed: LabelProximityPatternJson = match serde_json::from_value(row.pattern) {
                Ok(parsed) => parsed,
                Err(err) => {
                    tracing::warn!(tag_key = %row.tag_key, error = %err, "Skipping malformed tag_pattern row");
                    return None;
                }
            };
            let position = match parsed.position.as_str() {
                "before" => LabelPosition::Before,
                "after" => LabelPosition::After,
                other => {
                    tracing::warn!(tag_key = %row.tag_key, position = %other, "Skipping tag_pattern row with unrecognized position");
                    return None;
                }
            };
            Some(LabelProximityPattern {
                tag_key: row.tag_key,
                label: parsed.label,
                position,
                max_gap_chars: parsed.max_gap_chars,
                requires_preceding_anchor: parsed.requires_preceding_anchor.map(|a| {
                    unitprep_template_tagger::PrecedingAnchor {
                        text: a.text,
                        within_chars: a.within_chars,
                    }
                }),
            })
        })
        .collect())
}
