//! Where a facility's legal owners come from: its own Merchant Account run, and sister facilities of the same company.

use super::dto::LegalOwnerSource;
use crate::clients::legal_owner::OwnerIdentity;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub(super) struct FacilityIdentity {
    pub(super) ps_intake_run_id: Option<String>,
}

/// A facility's own Merchant Account owners with a name -- the only
/// ones that can say who owns anything.
pub(super) async fn merchant_account_owners(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    facility_id: Uuid,
) -> Result<Vec<OwnerIdentity>, sqlx::Error> {
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT display_name, email, phone
           FROM clients.facility_merchant_account_parties
          WHERE facility_id = $1 AND party_role = 'owner'",
    )
    .bind(facility_id)
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(display_name, email, phone)| OwnerIdentity {
            display_name,
            email,
            phone,
        })
        .collect())
}

pub(super) fn has_a_named_owner(owners: &[OwnerIdentity]) -> bool {
    owners.iter().any(|owner| {
        owner
            .display_name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty())
    })
}

/// Owners from a **sister facility's** Merchant Account form, for a
/// facility that has no owners of its own yet. Boris, 2026-10-02: every
/// facility will eventually need its own Merchant form, but until then
/// "we can relatively safely pick any that has information about owners
/// (i.e. merch form filled out)" -- the same company's facilities share
/// the same legal owners far more often than not (Affordable Storage's
/// Beau and Brad Ryan own all nine). Picks the sister with the most
/// named owners, then the most recently synced, then by name, so the
/// choice is stable between page loads.
pub(super) async fn sister_facility_owners(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    company_id: Uuid,
    facility_id: Uuid,
) -> Result<Option<(LegalOwnerSource, Vec<OwnerIdentity>)>, sqlx::Error> {
    let source: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT f.id, f.name
           FROM clients.facilities f
           JOIN clients.facility_merchant_account_parties p ON p.facility_id = f.id
          WHERE f.company_id = $1 AND f.id <> $2
            AND p.party_role = 'owner'
            AND btrim(coalesce(p.display_name, '')) <> ''
          GROUP BY f.id, f.name
          ORDER BY count(*) DESC, max(p.last_synced_at) DESC NULLS LAST, f.name
          LIMIT 1",
    )
    .bind(company_id)
    .bind(facility_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some((source_id, source_name)) = source else {
        return Ok(None);
    };

    let owners = merchant_account_owners(tx, source_id).await?;
    Ok(Some((
        LegalOwnerSource {
            facility_id: source_id,
            facility_name: source_name,
        },
        owners,
    )))
}
