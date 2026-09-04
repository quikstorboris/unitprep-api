//! QSX-legacy detection and the permanent per-category sync exemption it
//! grants, for the split Fees/Taxes/Delinquency/Coverage/Specials tabs
//! (`api::clients_facility_policies_edit`).
//!
//! **The problem this solves**: QSX (QuikStor Express, the legacy
//! product) has no equivalent to several of Process Street's own Intake
//! fields, so a facility migrating off QSX genuinely has nothing for
//! Fees/Taxes/Delinquency/Coverage/Specials in PS -- not "hasn't
//! answered yet," but "never will," confirmed live against Sand-Sto
//! Climate Controlled Storage (`previous_pms = "QuikStor Express"`,
//! every one of these five categories empty). A manager needs to type
//! these in by hand for such a facility, and once they do, that
//! category must never be silently overwritten by a future policy-sync
//! pass (not yet built) the way it would be for a facility whose data
//! genuinely came from PS.
//!
//! **The exemption is permanent, by design (Boris, 2026-09-04)**: once a
//! category is flagged exempt, nothing ever un-sets it automatically --
//! not even if PS somehow later has real data for it. Considered and
//! deliberately rejected: "PS now has data, but this category is locked"
//! reconciliation surfacing. Boris's call: "almost no chance of this
//! happening," not worth the complexity.
//!
//! **Why detection only matters at the moment of a category's first
//! edit, not at ingest time**: the condition is "empty right now, and
//! this facility is QSX" -- evaluated once, when a manager saves a
//! manual edit to a category with zero existing rows. `clients::create`/
//! `clients::repository::insert_facility_policies_and_people` (ingest)
//! never sets these flags themselves; only
//! `api::clients_facility_policies_edit`'s write handlers do, via
//! `mark_exempt_if_qsx_and_was_empty` below.
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Substrings of `clients.facilities.previous_pms` (PS's own "What
/// Property Management Software is this facility currently using?"
/// free-text field) known to mean QSX -- confirmed so far against
/// exactly one real facility (Sand-Sto: "QuikStor Express"). Matched
/// case-insensitively, substring rather than exact-equality, since this
/// is free text a human typed into a PS form. Expand this list the same
/// way the dash-format parser bug was found -- against real client data,
/// not guessed in advance.
const QSX_PREVIOUS_PMS_MARKERS: &[&str] = &["quikstor express", "qsx"];

pub fn is_qsx_legacy(previous_pms: Option<&str>) -> bool {
    let Some(previous_pms) = previous_pms else { return false };
    let lower = previous_pms.to_lowercase();
    QSX_PREVIOUS_PMS_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// One of the five Facility Policies categories -- the column name this
/// maps to on `clients.facility_policies` is `{category}_manually_exempt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyCategory {
    Fees,
    Taxes,
    Delinquency,
    Coverage,
    Specials,
}

impl PolicyCategory {
    fn exempt_column(self) -> &'static str {
        match self {
            PolicyCategory::Fees => "fees_manually_exempt",
            PolicyCategory::Taxes => "taxes_manually_exempt",
            PolicyCategory::Delinquency => "delinquency_manually_exempt",
            PolicyCategory::Coverage => "coverage_manually_exempt",
            PolicyCategory::Specials => "specials_manually_exempt",
        }
    }
}

/// Call after successfully writing a manual edit to `category`, with
/// `was_empty` = whether that category had zero rows/no data
/// immediately before this write. Idempotent (`OR`s the new condition
/// into the existing flag) -- a category already exempt stays exempt
/// regardless of what `was_empty`/QSX evaluate to on a later call. Not
/// itself transactional beyond the one UPDATE -- callers run this inside
/// their own write transaction, same as every other repository function
/// in this codebase.
pub async fn mark_exempt_if_qsx_and_was_empty(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    category: PolicyCategory,
    was_empty: bool,
) -> Result<(), sqlx::Error> {
    if !was_empty {
        return Ok(());
    }

    let previous_pms: Option<(Option<String>,)> =
        sqlx::query_as("SELECT previous_pms FROM clients.facilities WHERE id = $1")
            .bind(facility_id)
            .fetch_optional(&mut **tx)
            .await?;

    let is_qsx = previous_pms.and_then(|(pms,)| pms).is_some_and(|pms| is_qsx_legacy(Some(&pms)));
    if !is_qsx {
        return Ok(());
    }

    let sql = format!(
        "UPDATE clients.facility_policies SET {col} = true WHERE facility_id = $1",
        col = category.exempt_column()
    );
    sqlx::query(&sql).bind(facility_id).execute(&mut **tx).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_one_real_confirmed_spelling() {
        assert!(is_qsx_legacy(Some("QuikStor Express")));
    }

    #[test]
    fn is_case_insensitive() {
        assert!(is_qsx_legacy(Some("quikstor express")));
        assert!(is_qsx_legacy(Some("QUIKSTOR EXPRESS")));
    }

    #[test]
    fn matches_the_bare_qsx_abbreviation() {
        assert!(is_qsx_legacy(Some("Legacy QSX system")));
    }

    #[test]
    fn does_not_match_an_unrelated_pms() {
        assert!(!is_qsx_legacy(Some("SiteLink")));
        assert!(!is_qsx_legacy(Some("storEDGE")));
    }

    #[test]
    fn does_not_match_none() {
        assert!(!is_qsx_legacy(None));
    }
}

/// Proves `mark_exempt_if_qsx_and_was_empty`'s actual guarantee against
/// real Postgres, not just the pure `is_qsx_legacy` matcher above: a QSX
/// facility's empty category gets permanently flagged, a QSX facility's
/// already-populated category does not, and a non-QSX facility's empty
/// category does not either. Needs a real, reachable Postgres with every
/// migration applied -- `#[ignore]`d for the same reason
/// `clients::repository`'s own live tests are.
#[cfg(test)]
mod live_tests {
    use super::*;
    use uuid::Uuid;

    async fn insert_test_facility(
        tx: &mut Transaction<'_, Postgres>,
        previous_pms: Option<&str>,
    ) -> Uuid {
        let (company_id,): (Uuid,) =
            sqlx::query_as("INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id")
                .bind("Test Exemption Co")
                .fetch_one(&mut **tx)
                .await
                .unwrap();

        let (facility_id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO clients.facilities (company_id, name, previous_pms, source) \
             VALUES ($1, $2, $3, 'manual') RETURNING id",
        )
        .bind(company_id)
        .bind("Test Exemption Facility")
        .bind(previous_pms)
        .fetch_one(&mut **tx)
        .await
        .unwrap();

        sqlx::query("INSERT INTO clients.facility_policies (facility_id) VALUES ($1)")
            .bind(facility_id)
            .execute(&mut **tx)
            .await
            .unwrap();

        facility_id
    }

    async fn fees_exempt(tx: &mut Transaction<'_, Postgres>, facility_id: Uuid) -> bool {
        let (exempt,): (bool,) =
            sqlx::query_as("SELECT fees_manually_exempt FROM clients.facility_policies WHERE facility_id = $1")
                .bind(facility_id)
                .fetch_one(&mut **tx)
                .await
                .unwrap();
        exempt
    }

    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    async fn a_qsx_facilitys_empty_category_gets_permanently_exempt() {
        let _ = dotenvy::from_filename(".env.local");
        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let mut tx = crate::auth::begin_rls_transaction(&db, Uuid::new_v4(), &["onboarding_manager".to_string()])
            .await
            .unwrap();

        let facility_id = insert_test_facility(&mut tx, Some("QuikStor Express")).await;

        mark_exempt_if_qsx_and_was_empty(&mut tx, facility_id, PolicyCategory::Fees, true).await.unwrap();

        assert!(fees_exempt(&mut tx, facility_id).await, "an empty category on a QSX facility must become exempt");

        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    async fn a_qsx_facilitys_already_populated_category_never_becomes_exempt() {
        let _ = dotenvy::from_filename(".env.local");
        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let mut tx = crate::auth::begin_rls_transaction(&db, Uuid::new_v4(), &["onboarding_manager".to_string()])
            .await
            .unwrap();

        let facility_id = insert_test_facility(&mut tx, Some("QuikStor Express")).await;

        // was_empty = false -- this is what a write handler passes when
        // the category already had rows before this save (came from PS,
        // or a previous manual edit), regardless of QSX status.
        mark_exempt_if_qsx_and_was_empty(&mut tx, facility_id, PolicyCategory::Fees, false).await.unwrap();

        assert!(
            !fees_exempt(&mut tx, facility_id).await,
            "a category that wasn't empty before this write must never become exempt, QSX or not"
        );

        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    async fn a_non_qsx_facilitys_empty_category_never_becomes_exempt() {
        let _ = dotenvy::from_filename(".env.local");
        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let mut tx = crate::auth::begin_rls_transaction(&db, Uuid::new_v4(), &["onboarding_manager".to_string()])
            .await
            .unwrap();

        let facility_id = insert_test_facility(&mut tx, Some("SiteLink")).await;

        mark_exempt_if_qsx_and_was_empty(&mut tx, facility_id, PolicyCategory::Fees, true).await.unwrap();

        assert!(
            !fees_exempt(&mut tx, facility_id).await,
            "an empty category on a non-QSX facility must never become exempt"
        );

        tx.rollback().await.unwrap();
    }
}
