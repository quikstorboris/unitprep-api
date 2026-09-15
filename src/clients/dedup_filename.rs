//! Facility-scoped filenames for the dedup tool's exports --
//! `{ABBREV}_v{N}_pull_check_{MM-DD-YYYY}.{ext}`, e.g.
//! `MSS_v1_pull_check_09-14-2026.xlsx`. Lives here (`clients`), not
//! `client_ops`, because it's naming logic for a `clients.facilities`
//! row -- see `api::dedup`'s own module doc for the `clients` vs.
//! `client_ops` split this follows.
//!
//! `{ABBREV}` is derived from the *facility's* own name (dedup runs
//! per-facility, not per-company -- see `derive_abbreviation`).
//! `{N}` is `clients.facilities.dedup_export_sequence`, a durable
//! per-facility counter that never resets: the Nth export ever produced
//! for that facility, forever, regardless of date (see the migration
//! that added the column). `{MM-DD-YYYY}` is the current date in UTC,
//! matching this app's existing `Utc::now()` convention (e.g.
//! `api::export`'s own download-filename timestamp) -- no local-time
//! handling.
//!
//! A dedup session carries no facility identity of its own (see
//! `application::dedup_session_service::DedupSession`'s own doc) --
//! `facility_id` is passed in explicitly by the caller at export time,
//! same as `DedupExportRequest::client_id` already is. When it's `None`
//! (a standalone run with no client/facility context), `standalone_file_name`
//! preserves today's static/timestamped fallback behavior instead.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::begin_rls_transaction;

/// Corporate suffixes stripped (case-insensitively, whole-word) before
/// abbreviating a facility name -- e.g. "Main Street Storage LLC" and
/// "Main Street Storage" both abbreviate to "MSS".
const CORPORATE_SUFFIXES: &[&str] = &[
    "llc",
    "l.l.c",
    "l.l.c.",
    "inc",
    "inc.",
    "corp",
    "corp.",
    "corporation",
    "ltd",
    "ltd.",
    "co",
    "co.",
];

/// Splits `facility_name` on whitespace, drops common corporate suffixes
/// (case-insensitively), takes the first alphabetic character of each
/// remaining word, uppercases it, and joins with no separator --
/// "Main Street Storage" -> "MSS". Falls back to the first 3 alphabetic
/// characters of the *original*, unfiltered name, uppercased, for the
/// edge case where every word gets stripped (e.g. a facility literally
/// named "LLC Inc").
pub fn derive_abbreviation(facility_name: &str) -> String {
    let abbreviation: String = facility_name
        .split_whitespace()
        .filter(|word| {
            !CORPORATE_SUFFIXES
                .iter()
                .any(|suffix| word.eq_ignore_ascii_case(suffix))
        })
        .filter_map(|word| word.chars().find(|c| c.is_alphabetic()))
        .map(|c| c.to_ascii_uppercase())
        .collect();

    if !abbreviation.is_empty() {
        return abbreviation;
    }

    facility_name
        .chars()
        .filter(|c| c.is_alphabetic())
        .take(3)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Formats the final export filename for a real facility --
/// `derive_abbreviation(facility_name)` + `_v{sequence}_pull_check_` +
/// `now`'s UTC date + `.{ext}`. `now` is threaded in explicitly (rather
/// than calling `Utc::now()` here) so one export action -- which can
/// produce up to three filenames sharing one version number (the ZIP
/// itself plus its two inner files, see `api::dedup::generate_zip_bytes`)
/// -- always stamps the same date on all of them, even right at a
/// day boundary.
pub fn format_export_filename(
    facility_name: &str,
    sequence: i32,
    ext: &str,
    now: DateTime<Utc>,
) -> String {
    let abbreviation = derive_abbreviation(facility_name);
    let date = now.format("%m-%d-%Y");
    format!("{abbreviation}_v{sequence}_pull_check_{date}.{ext}")
}

/// The pre-existing, facility-less naming: `duplicate_tenant_check.{ext}`
/// for a browser download (`timestamped: false`, today's unchanged
/// behavior), or `duplicate_tenant_check_{YYYY-MM-DD_HHMM}.{ext}` for a
/// Dropbox save (`timestamped: true`) -- this used to be computed
/// client-side by `useDedupSaveToDropbox.ts`'s own `withTimestamp`
/// helper (see that file's doc comment on why: `DropboxClient::upload`
/// is overwrite-only, so two saves of the same session to the same
/// folder would otherwise silently clobber each other); now computed
/// here since the frontend no longer builds any dedup export filename
/// itself, real-facility or not.
pub fn standalone_file_name(ext: &str, now: DateTime<Utc>, timestamped: bool) -> String {
    if timestamped {
        let stamp = now.format("%Y-%m-%d_%H%M");
        format!("duplicate_tenant_check_{stamp}.{ext}")
    } else {
        format!("duplicate_tenant_check.{ext}")
    }
}

/// Atomically increments `clients.facilities.dedup_export_sequence` for
/// `facility_id` and returns the facility's own `name` plus the new
/// sequence value -- one `UPDATE ... RETURNING` so two concurrent
/// exports for the same facility can never be handed the same version
/// number. Opens and commits its own RLS transaction, same convention
/// as this handler's sibling calls into `client_ops::tool_runs` --
/// unlike those, a failure here is surfaced to the caller (`?`-able
/// `Result`, not swallowed-and-logged) since, unlike attaching output
/// metadata after the fact, a wrong or missing version number would
/// mean silently mis-naming the export the user is about to receive.
pub async fn increment_export_sequence(
    db: &PgPool,
    actor_user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<(String, i32), sqlx::Error> {
    let mut tx = begin_rls_transaction(db, actor_user_id, role_keys).await?;

    let (name, sequence): (String, i32) = sqlx::query_as(
        "UPDATE clients.facilities
            SET dedup_export_sequence = dedup_export_sequence + 1
          WHERE id = $1
        RETURNING name, dedup_export_sequence",
    )
    .bind(facility_id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok((name, sequence))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviates_a_plain_multi_word_name() {
        assert_eq!(derive_abbreviation("Main Street Storage"), "MSS");
    }

    #[test]
    fn strips_a_trailing_llc() {
        assert_eq!(derive_abbreviation("Main Street Storage LLC"), "MSS");
    }

    #[test]
    fn strips_a_trailing_inc_case_insensitively() {
        assert_eq!(derive_abbreviation("Prairie Enterprises inc."), "PE");
    }

    #[test]
    fn strips_corp_and_leaves_the_real_words() {
        assert_eq!(derive_abbreviation("Highway 20 Self Storage Corp"), "HSS");
    }

    #[test]
    fn a_single_word_name_abbreviates_to_one_letter() {
        assert_eq!(derive_abbreviation("Dubuqueland"), "D");
    }

    #[test]
    fn every_word_stripped_falls_back_to_first_three_letters_of_the_original() {
        // Every word here is itself a corporate suffix -- nothing
        // survives the filter, so this must fall back to the first 3
        // alphabetic characters of the original, unfiltered string.
        assert_eq!(derive_abbreviation("LLC Inc"), "LLC");
    }

    #[test]
    fn formats_the_final_filename_with_version_and_date() {
        let now = DateTime::parse_from_rfc3339("2026-09-14T18:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            format_export_filename("Main Street Storage", 1, "xlsx", now),
            "MSS_v1_pull_check_09-14-2026.xlsx"
        );
    }

    #[test]
    fn standalone_file_name_without_timestamp_is_the_static_default() {
        let now = DateTime::parse_from_rfc3339("2026-09-14T18:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            standalone_file_name("csv", now, false),
            "duplicate_tenant_check.csv"
        );
    }

    #[test]
    fn standalone_file_name_with_timestamp_matches_the_old_client_side_format() {
        let now = DateTime::parse_from_rfc3339("2026-09-14T18:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            standalone_file_name("csv", now, true),
            "duplicate_tenant_check_2026-09-14_1830.csv"
        );
    }
}
