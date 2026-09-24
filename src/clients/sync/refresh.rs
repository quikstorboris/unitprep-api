use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::clients::intake_mapping::{MappedCompany, MappedFacility};

use super::progress::SyncError;

/// Computes a refreshed field value -- never overwrites a field listed
/// in `manually_edited_fields` (a human's deliberate correction), and
/// never blanks a good existing value just because this fresh pull came
/// back empty for it. Only a genuine, non-null change from Process
/// Street is ever applied. See `clients.companies`/`clients.facilities`
/// `manually_edited_fields` columns' own migration comment for why this
/// exists at all.
fn refreshed_field<T: Clone + PartialEq>(
    current: &Option<T>,
    fresh: &Option<T>,
    is_protected: bool,
) -> Option<T> {
    if is_protected {
        return current.clone();
    }
    match fresh {
        Some(_) => fresh.clone(),
        None => current.clone(),
    }
}

/// Applies a fresh Intake mapping onto a company's current fields,
/// respecting `protected_fields` (that company's own
/// `manually_edited_fields`) field by field -- mirrors
/// `clients::create::diff_company_fields`'s own field list exactly,
/// since the two are the write and read sides of the same protected set.
pub(crate) fn apply_company_refresh(
    current: &MappedCompany,
    fresh: &MappedCompany,
    protected_fields: &[String],
) -> MappedCompany {
    let is_protected = |field: &str| protected_fields.iter().any(|p| p == field);
    MappedCompany {
        legal_name: refreshed_field(
            &current.legal_name,
            &fresh.legal_name,
            is_protected("legal_name"),
        ),
        corporate_email: refreshed_field(
            &current.corporate_email,
            &fresh.corporate_email,
            is_protected("corporate_email"),
        ),
        corporate_phone: refreshed_field(
            &current.corporate_phone,
            &fresh.corporate_phone,
            is_protected("corporate_phone"),
        ),
        corporate_address_street: refreshed_field(
            &current.corporate_address_street,
            &fresh.corporate_address_street,
            is_protected("corporate_address_street"),
        ),
        corporate_address_city: refreshed_field(
            &current.corporate_address_city,
            &fresh.corporate_address_city,
            is_protected("corporate_address_city"),
        ),
        corporate_address_state: refreshed_field(
            &current.corporate_address_state,
            &fresh.corporate_address_state,
            is_protected("corporate_address_state"),
        ),
        corporate_address_zip: refreshed_field(
            &current.corporate_address_zip,
            &fresh.corporate_address_zip,
            is_protected("corporate_address_zip"),
        ),
        subdomain: refreshed_field(
            &current.subdomain,
            &fresh.subdomain,
            is_protected("subdomain"),
        ),
        accepted_payment_methods: refreshed_field(
            &current.accepted_payment_methods,
            &fresh.accepted_payment_methods,
            is_protected("accepted_payment_methods"),
        ),
        accounting_basis: refreshed_field(
            &current.accounting_basis,
            &fresh.accounting_basis,
            is_protected("accounting_basis"),
        ),
        payment_scheme: refreshed_field(
            &current.payment_scheme,
            &fresh.payment_scheme,
            is_protected("payment_scheme"),
        ),
        offers_tenant_insurance_raw: refreshed_field(
            &current.offers_tenant_insurance_raw,
            &fresh.offers_tenant_insurance_raw,
            is_protected("offers_tenant_insurance_raw"),
        ),
        insurance_provider: refreshed_field(
            &current.insurance_provider,
            &fresh.insurance_provider,
            is_protected("insurance_provider"),
        ),
        // `map_intake_fields` never sets `MappedCompany::website_url`
        // (no PS field of its own -- see that field's own doc comment),
        // so `fresh.website_url` is always `None` here and
        // `refreshed_field`'s own "a fresh null never blanks a good
        // value" rule means this always just preserves whatever the
        // confirmation-screen fallback already copied in. Included
        // anyway for the same reason `is_protected` still gates it: the
        // day this ever gains a real PS source, this line already does
        // the right thing.
        website_url: refreshed_field(
            &current.website_url,
            &fresh.website_url,
            is_protected("website_url"),
        ),
    }
}

/// `apply_company_refresh`'s counterpart for `MappedFacility` --
/// `go_live_date` is always carried through from `current` untouched,
/// same "never touched by anything but PS's own original mapping" rule
/// `clients::create::apply_facility_overrides` already established.
pub(crate) fn apply_facility_refresh(
    current: &MappedFacility,
    fresh: &MappedFacility,
    protected_fields: &[String],
) -> MappedFacility {
    let is_protected = |field: &str| protected_fields.iter().any(|p| p == field);
    MappedFacility {
        name: refreshed_field(&current.name, &fresh.name, is_protected("name")),
        street_address: refreshed_field(
            &current.street_address,
            &fresh.street_address,
            is_protected("street_address"),
        ),
        city: refreshed_field(&current.city, &fresh.city, is_protected("city")),
        state: refreshed_field(&current.state, &fresh.state, is_protected("state")),
        zip: refreshed_field(&current.zip, &fresh.zip, is_protected("zip")),
        phone: refreshed_field(&current.phone, &fresh.phone, is_protected("phone")),
        email: refreshed_field(&current.email, &fresh.email, is_protected("email")),
        units_count: refreshed_field(
            &current.units_count,
            &fresh.units_count,
            is_protected("units_count"),
        ),
        primary_storage_offering: refreshed_field(
            &current.primary_storage_offering,
            &fresh.primary_storage_offering,
            is_protected("primary_storage_offering"),
        ),
        previous_pms: refreshed_field(
            &current.previous_pms,
            &fresh.previous_pms,
            is_protected("previous_pms"),
        ),
        access_control_system: refreshed_field(
            &current.access_control_system,
            &fresh.access_control_system,
            is_protected("access_control_system"),
        ),
        dropbox_folder_url: refreshed_field(
            &current.dropbox_folder_url,
            &fresh.dropbox_folder_url,
            is_protected("dropbox_folder_url"),
        ),
        subdomain: refreshed_field(
            &current.subdomain,
            &fresh.subdomain,
            is_protected("subdomain"),
        ),
        subdomain_exists_in_qms_raw: refreshed_field(
            &current.subdomain_exists_in_qms_raw,
            &fresh.subdomain_exists_in_qms_raw,
            is_protected("subdomain_exists_in_qms_raw"),
        ),
        system_email: refreshed_field(
            &current.system_email,
            &fresh.system_email,
            is_protected("system_email"),
        ),
        website_url: refreshed_field(
            &current.website_url,
            &fresh.website_url,
            is_protected("website_url"),
        ),
        go_live_date: current.go_live_date,
    }
}

/// Field names where two `MappedFacility` values differ -- `go_live_date`
/// is deliberately excluded, same reasoning as everywhere else in this
/// module: nothing but the original PS mapping ever sets it. Used by
/// `api::clients_resync` to tell "this field would change on refresh"
/// (`current` vs. the fresh pull) apart from "this field would change
/// AND it's protected" (an actual conflict needing the caller's choice)
/// -- `clients::create::diff_company_fields` is `MappedCompany`'s own
/// counterpart, directly reusable there since both its arguments are
/// already `MappedCompany`.
pub(crate) fn facility_fields_that_differ(
    a: &MappedFacility,
    b: &MappedFacility,
) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if a.name != b.name {
        changed.push("name");
    }
    if a.street_address != b.street_address {
        changed.push("street_address");
    }
    if a.city != b.city {
        changed.push("city");
    }
    if a.state != b.state {
        changed.push("state");
    }
    if a.zip != b.zip {
        changed.push("zip");
    }
    if a.phone != b.phone {
        changed.push("phone");
    }
    if a.email != b.email {
        changed.push("email");
    }
    if a.units_count != b.units_count {
        changed.push("units_count");
    }
    if a.primary_storage_offering != b.primary_storage_offering {
        changed.push("primary_storage_offering");
    }
    if a.previous_pms != b.previous_pms {
        changed.push("previous_pms");
    }
    if a.access_control_system != b.access_control_system {
        changed.push("access_control_system");
    }
    if a.dropbox_folder_url != b.dropbox_folder_url {
        changed.push("dropbox_folder_url");
    }
    if a.subdomain != b.subdomain {
        changed.push("subdomain");
    }
    if a.subdomain_exists_in_qms_raw != b.subdomain_exists_in_qms_raw {
        changed.push("subdomain_exists_in_qms_raw");
    }
    if a.system_email != b.system_email {
        changed.push("system_email");
    }
    if a.website_url != b.website_url {
        changed.push("website_url");
    }
    changed
}

/// Reads one field's current string value off a `MappedCompany` by name
/// -- used by `api::clients_resync` to describe a conflict generically
/// (field name + both candidate values) without a giant match at the
/// call site. `None` for a name this type doesn't have, which never
/// happens in practice since callers only ever pass names this same
/// module's own diff functions produced.
pub(crate) fn company_field_value(company: &MappedCompany, field: &str) -> Option<String> {
    match field {
        "legal_name" => company.legal_name.clone(),
        "corporate_email" => company.corporate_email.clone(),
        "corporate_phone" => company.corporate_phone.clone(),
        "corporate_address_street" => company.corporate_address_street.clone(),
        "corporate_address_city" => company.corporate_address_city.clone(),
        "corporate_address_state" => company.corporate_address_state.clone(),
        "corporate_address_zip" => company.corporate_address_zip.clone(),
        "subdomain" => company.subdomain.clone(),
        "accepted_payment_methods" => company.accepted_payment_methods.clone(),
        "accounting_basis" => company.accounting_basis.clone(),
        "payment_scheme" => company.payment_scheme.clone(),
        "offers_tenant_insurance_raw" => company.offers_tenant_insurance_raw.clone(),
        "insurance_provider" => company.insurance_provider.clone(),
        "website_url" => company.website_url.clone(),
        _ => None,
    }
}

/// `company_field_value`'s counterpart for `MappedFacility`.
pub(crate) fn facility_field_value(facility: &MappedFacility, field: &str) -> Option<String> {
    match field {
        "name" => facility.name.clone(),
        "street_address" => facility.street_address.clone(),
        "city" => facility.city.clone(),
        "state" => facility.state.clone(),
        "zip" => facility.zip.clone(),
        "phone" => facility.phone.clone(),
        "email" => facility.email.clone(),
        "units_count" => facility.units_count.map(|n| n.to_string()),
        "primary_storage_offering" => facility.primary_storage_offering.clone(),
        "previous_pms" => facility.previous_pms.clone(),
        "access_control_system" => facility.access_control_system.clone(),
        "dropbox_folder_url" => facility.dropbox_folder_url.clone(),
        "subdomain" => facility.subdomain.clone(),
        "subdomain_exists_in_qms_raw" => facility.subdomain_exists_in_qms_raw.clone(),
        "system_email" => facility.system_email.clone(),
        "website_url" => facility.website_url.clone(),
        _ => None,
    }
}

/// Refreshes the one `clients.companies` row (if any) whose
/// `ps_intake_run_id` matches this Intake run, applying `fresh` field by
/// field through `apply_company_refresh`. A no-op (returns `false`,
/// touches nothing) when no company matches this run at all, or when
/// every field that would change is protected/already up to date.
/// A `clients.companies` row's current refreshable fields --
/// `#[derive(FromRow)]` rather than a tuple for the same reason
/// `ExistingFacilityRow` below is: sqlx's tuple `FromRow` impls only go
/// up to a handful of elements, and the 2026-09-03 Financial Information
/// fields pushed this table past that.
#[derive(sqlx::FromRow)]
struct ExistingCompanyRow {
    id: Uuid,
    legal_name: String,
    corporate_email: Option<String>,
    corporate_phone: Option<String>,
    corporate_address_street: Option<String>,
    corporate_address_city: Option<String>,
    corporate_address_state: Option<String>,
    corporate_address_zip: Option<String>,
    subdomain: Option<String>,
    accepted_payment_methods: Option<String>,
    accounting_basis: Option<String>,
    payment_scheme: Option<String>,
    offers_tenant_insurance_raw: Option<String>,
    insurance_provider: Option<String>,
    website_url: Option<String>,
    manually_edited_fields: Vec<String>,
}

pub(super) async fn refresh_matching_company(
    tx: &mut Transaction<'_, Postgres>,
    run_id: &str,
    fresh: &MappedCompany,
) -> Result<bool, SyncError> {
    let existing: Option<ExistingCompanyRow> = sqlx::query_as(
        "SELECT id, legal_name, corporate_email, corporate_phone, corporate_address_street, \
         corporate_address_city, corporate_address_state, corporate_address_zip, subdomain, \
         accepted_payment_methods, accounting_basis, payment_scheme, offers_tenant_insurance_raw, \
         insurance_provider, website_url, manually_edited_fields \
         FROM clients.companies WHERE ps_intake_run_id = $1",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some(ExistingCompanyRow {
        id,
        legal_name,
        corporate_email,
        corporate_phone,
        corporate_address_street,
        corporate_address_city,
        corporate_address_state,
        corporate_address_zip,
        subdomain,
        accepted_payment_methods,
        accounting_basis,
        payment_scheme,
        offers_tenant_insurance_raw,
        insurance_provider,
        website_url,
        manually_edited_fields,
    }) = existing
    else {
        return Ok(false);
    };

    let current = MappedCompany {
        legal_name: Some(legal_name),
        corporate_email,
        corporate_phone,
        corporate_address_street,
        corporate_address_city,
        corporate_address_state,
        corporate_address_zip,
        subdomain,
        accepted_payment_methods,
        accounting_basis,
        payment_scheme,
        offers_tenant_insurance_raw,
        insurance_provider,
        website_url,
    };

    let refreshed = apply_company_refresh(&current, fresh, &manually_edited_fields);
    if refreshed == current {
        return Ok(false);
    }

    let refreshed_legal_name = refreshed
        .legal_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("(unnamed company)");

    sqlx::query(
        "UPDATE clients.companies SET legal_name = $1, corporate_email = $2, corporate_phone = $3, \
         corporate_address_street = $4, corporate_address_city = $5, corporate_address_state = $6, \
         corporate_address_zip = $7, subdomain = $8, accepted_payment_methods = $9, \
         accounting_basis = $10, payment_scheme = $11, offers_tenant_insurance_raw = $12, \
         insurance_provider = $13, website_url = $14, last_synced_at = now() WHERE id = $15",
    )
    .bind(refreshed_legal_name)
    .bind(&refreshed.corporate_email)
    .bind(&refreshed.corporate_phone)
    .bind(&refreshed.corporate_address_street)
    .bind(&refreshed.corporate_address_city)
    .bind(&refreshed.corporate_address_state)
    .bind(&refreshed.corporate_address_zip)
    .bind(&refreshed.subdomain)
    .bind(&refreshed.accepted_payment_methods)
    .bind(&refreshed.accounting_basis)
    .bind(&refreshed.payment_scheme)
    .bind(&refreshed.offers_tenant_insurance_raw)
    .bind(&refreshed.insurance_provider)
    .bind(&refreshed.website_url)
    .bind(id)
    .execute(&mut **tx)
    .await?;

    Ok(true)
}

/// A `clients.facilities` row's current refreshable fields --
/// `#[derive(FromRow)]` rather than a giant tuple purely because sqlx's
/// tuple `FromRow` impls only go up to a handful of elements, well short
/// of this table's real column count.
#[derive(sqlx::FromRow)]
struct ExistingFacilityRow {
    id: Uuid,
    name: String,
    street_address: Option<String>,
    city: Option<String>,
    state: Option<String>,
    zip: Option<String>,
    phone: Option<String>,
    email: Option<String>,
    units_count: Option<i32>,
    primary_storage_offering: Option<String>,
    previous_pms: Option<String>,
    access_control_system: Option<String>,
    go_live_date: Option<chrono::NaiveDate>,
    dropbox_folder_url: Option<String>,
    subdomain: Option<String>,
    subdomain_exists_in_qms_raw: Option<String>,
    system_email: Option<String>,
    website_url: Option<String>,
    manually_edited_fields: Vec<String>,
}

/// `refresh_matching_company`'s counterpart for `clients.facilities`.
pub(super) async fn refresh_matching_facility(
    tx: &mut Transaction<'_, Postgres>,
    run_id: &str,
    fresh: &MappedFacility,
) -> Result<bool, SyncError> {
    let existing: Option<ExistingFacilityRow> = sqlx::query_as(
        "SELECT id, name, street_address, city, state, zip, phone, email, units_count, \
         primary_storage_offering, previous_pms, access_control_system, go_live_date, \
         dropbox_folder_url, subdomain, subdomain_exists_in_qms_raw, system_email, website_url, \
         manually_edited_fields \
         FROM clients.facilities WHERE ps_intake_run_id = $1",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some(ExistingFacilityRow {
        id,
        name,
        street_address,
        city,
        state,
        zip,
        phone,
        email,
        units_count,
        primary_storage_offering,
        previous_pms,
        access_control_system,
        go_live_date,
        dropbox_folder_url,
        subdomain,
        subdomain_exists_in_qms_raw,
        system_email,
        website_url,
        manually_edited_fields,
    }) = existing
    else {
        return Ok(false);
    };

    let current = MappedFacility {
        name: Some(name),
        street_address,
        city,
        state,
        zip,
        phone,
        email,
        units_count,
        primary_storage_offering,
        previous_pms,
        access_control_system,
        go_live_date,
        dropbox_folder_url,
        subdomain,
        subdomain_exists_in_qms_raw,
        system_email,
        website_url,
    };

    let refreshed = apply_facility_refresh(&current, fresh, &manually_edited_fields);
    if refreshed == current {
        return Ok(false);
    }

    sqlx::query(
        "UPDATE clients.facilities SET name = $1, street_address = $2, city = $3, state = $4, \
         zip = $5, phone = $6, email = $7, units_count = $8, primary_storage_offering = $9, \
         previous_pms = $10, access_control_system = $11, dropbox_folder_url = $12, \
         subdomain = $13, subdomain_exists_in_qms_raw = $14, system_email = $15, website_url = $16, \
         last_synced_at = now() WHERE id = $17",
    )
    .bind(refreshed.name.as_deref().unwrap_or("(unnamed facility)"))
    .bind(&refreshed.street_address)
    .bind(&refreshed.city)
    .bind(&refreshed.state)
    .bind(&refreshed.zip)
    .bind(&refreshed.phone)
    .bind(&refreshed.email)
    .bind(refreshed.units_count)
    .bind(&refreshed.primary_storage_offering)
    .bind(&refreshed.previous_pms)
    .bind(&refreshed.access_control_system)
    .bind(&refreshed.dropbox_folder_url)
    .bind(&refreshed.subdomain)
    .bind(&refreshed.subdomain_exists_in_qms_raw)
    .bind(&refreshed.system_email)
    .bind(&refreshed.website_url)
    .bind(id)
    .execute(&mut **tx)
    .await?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshed_field_takes_a_new_non_null_value_when_not_protected() {
        let current = Some("old".to_string());
        let fresh = Some("new".to_string());

        assert_eq!(
            refreshed_field(&current, &fresh, false),
            Some("new".to_string())
        );
    }

    #[test]
    fn refreshed_field_never_overwrites_a_protected_field() {
        let current = Some("manually corrected".to_string());
        let fresh = Some("stale ps value".to_string());

        assert_eq!(
            refreshed_field(&current, &fresh, true),
            Some("manually corrected".to_string())
        );
    }

    #[test]
    fn refreshed_field_never_blanks_a_good_value_with_a_fresh_null() {
        // PS returning nothing for a field it previously had a value for
        // must not be read as "clear it" -- only a genuine new value
        // ever overwrites, protected or not.
        let current = Some("existing value".to_string());
        let fresh: Option<String> = None;

        assert_eq!(refreshed_field(&current, &fresh, false), current);
    }

    fn company(legal_name: &str) -> MappedCompany {
        MappedCompany {
            legal_name: Some(legal_name.to_string()),
            corporate_email: Some("office@example.com".to_string()),
            corporate_phone: Some("555-000-0000".to_string()),
            corporate_address_street: Some("1 Example St".to_string()),
            corporate_address_city: Some("Example City".to_string()),
            corporate_address_state: Some("IL".to_string()),
            corporate_address_zip: Some("60000".to_string()),
            subdomain: Some("example.qms-email.com".to_string()),
            accepted_payment_methods: Some("Credit Card, ACH".to_string()),
            accounting_basis: Some("Cash".to_string()),
            payment_scheme: Some("Advance".to_string()),
            offers_tenant_insurance_raw: Some("Yes".to_string()),
            insurance_provider: Some("Example Insurance Co".to_string()),
            website_url: Some("https://example.com".to_string()),
        }
    }

    #[test]
    fn apply_company_refresh_updates_unprotected_fields_that_changed() {
        let current = company("Old Legal Name LLC");
        let fresh = company("Prairie Enterprises LLC");

        let refreshed = apply_company_refresh(&current, &fresh, &[]);

        assert_eq!(
            refreshed.legal_name.as_deref(),
            Some("Prairie Enterprises LLC")
        );
    }

    #[test]
    fn apply_company_refresh_leaves_a_manually_edited_field_untouched() {
        let current = company("Manually Corrected LLC");
        let fresh = company("Stale PS Legal Name LLC");
        let protected = vec!["legal_name".to_string()];

        let refreshed = apply_company_refresh(&current, &fresh, &protected);

        assert_eq!(
            refreshed.legal_name.as_deref(),
            Some("Manually Corrected LLC")
        );
        // Every other field is still free to refresh normally.
        assert_eq!(refreshed.corporate_email, fresh.corporate_email);
    }

    #[test]
    fn apply_facility_refresh_never_touches_go_live_date() {
        let current = MappedFacility {
            go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            ..fully_mapped_facility()
        };
        let fresh = MappedFacility {
            go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 6, 1),
            ..fully_mapped_facility()
        };

        let refreshed = apply_facility_refresh(&current, &fresh, &[]);

        assert_eq!(refreshed.go_live_date, current.go_live_date);
    }

    #[test]
    fn apply_facility_refresh_leaves_a_manually_edited_field_untouched() {
        let current = MappedFacility {
            phone: Some("555-CORRECTED".to_string()),
            ..fully_mapped_facility()
        };
        let fresh = MappedFacility {
            phone: Some("555-STALE".to_string()),
            name: Some("Updated Facility Name".to_string()),
            ..fully_mapped_facility()
        };
        let protected = vec!["phone".to_string()];

        let refreshed = apply_facility_refresh(&current, &fresh, &protected);

        assert_eq!(refreshed.phone.as_deref(), Some("555-CORRECTED"));
        assert_eq!(refreshed.name.as_deref(), Some("Updated Facility Name"));
    }

    /// Every field name serde reports for `value` -- read from the type's
    /// own `Serialize` impl rather than hand-listed, so a field added to
    /// `MappedCompany`/`MappedFacility` shows up here automatically. This
    /// is what makes the parity tests below actually catch field-list
    /// drift instead of just re-asserting a second hand-maintained copy
    /// of the same list the code under test already has.
    fn field_names<T: serde::Serialize>(value: &T) -> Vec<String> {
        serde_json::to_value(value)
            .expect("test fixture must serialize")
            .as_object()
            .expect("must serialize to a JSON object")
            .keys()
            .cloned()
            .collect()
    }

    /// Closes the gap named in the vault's Dev Principles audit: nothing
    /// stops a new `MappedCompany` field from being forgotten in
    /// `company_field_value`'s match arms. Unlike `apply_company_refresh`
    /// (a full struct literal -- the compiler already refuses to build if
    /// a field is missing there), `company_field_value` is a `match
    /// field: &str` with a catch-all `_ => None`, so a forgotten arm
    /// compiles fine and just silently returns `None` forever. `company()`
    /// sets every field to `Some(..)`, so any field this function doesn't
    /// recognize shows up as a `None` here.
    #[test]
    fn company_field_value_has_an_arm_for_every_mappedcompany_field() {
        let company = company("Test LLC");

        for field in field_names(&company) {
            assert!(
                company_field_value(&company, &field).is_some(),
                "company_field_value has no arm for MappedCompany field `{field}` (or it \
                 doesn't return the field's real value) -- it was likely added to the struct \
                 but forgotten here",
            );
        }
    }

    /// `diff_company_fields` (`clients::create`) is `company_field_value`'s
    /// sibling for the same drift risk -- a per-field `if a != b` list
    /// with no catch-all to fail loudly, so a forgotten field silently
    /// never reports as "changed" instead of erroring.
    #[test]
    fn diff_company_fields_reports_every_mappedcompany_field_that_actually_changed() {
        let reviewed = company("Reviewed LLC");
        // A second value differing in *every* field -- constructed by
        // appending to every string field rather than hand-writing 14
        // distinct literals, so this stays correct if a field's content
        // changes without needing to be hand-updated in lockstep.
        let fresh = MappedCompany {
            legal_name: reviewed.legal_name.clone().map(|v| format!("{v} (fresh)")),
            corporate_email: reviewed
                .corporate_email
                .clone()
                .map(|v| format!("{v} (fresh)")),
            corporate_phone: reviewed
                .corporate_phone
                .clone()
                .map(|v| format!("{v} (fresh)")),
            corporate_address_street: reviewed
                .corporate_address_street
                .clone()
                .map(|v| format!("{v} (fresh)")),
            corporate_address_city: reviewed
                .corporate_address_city
                .clone()
                .map(|v| format!("{v} (fresh)")),
            corporate_address_state: reviewed
                .corporate_address_state
                .clone()
                .map(|v| format!("{v} (fresh)")),
            corporate_address_zip: reviewed
                .corporate_address_zip
                .clone()
                .map(|v| format!("{v} (fresh)")),
            subdomain: reviewed.subdomain.clone().map(|v| format!("{v} (fresh)")),
            accepted_payment_methods: reviewed
                .accepted_payment_methods
                .clone()
                .map(|v| format!("{v} (fresh)")),
            accounting_basis: reviewed
                .accounting_basis
                .clone()
                .map(|v| format!("{v} (fresh)")),
            payment_scheme: reviewed
                .payment_scheme
                .clone()
                .map(|v| format!("{v} (fresh)")),
            offers_tenant_insurance_raw: reviewed
                .offers_tenant_insurance_raw
                .clone()
                .map(|v| format!("{v} (fresh)")),
            insurance_provider: reviewed
                .insurance_provider
                .clone()
                .map(|v| format!("{v} (fresh)")),
            website_url: reviewed.website_url.clone().map(|v| format!("{v} (fresh)")),
        };

        let changed = crate::clients::create::diff_company_fields(&fresh, &reviewed);

        for field in field_names(&reviewed) {
            assert!(
                changed.contains(&field.as_str()),
                "diff_company_fields did not report `{field}` as changed even though this \
                 test set it to a different value on both sides -- it was likely added to \
                 MappedCompany but forgotten here",
            );
        }
    }

    /// `MappedFacility`'s counterpart to the two tests above.
    /// `go_live_date` is deliberately excluded, same reasoning as
    /// `facility_field_value`'s and `facility_fields_that_differ`'s own
    /// doc comments: nothing but the original PS mapping ever sets it.
    const FACILITY_FIELD_NOT_COVERED_BY_REFRESH: &str = "go_live_date";

    #[test]
    fn facility_field_value_has_an_arm_for_every_mappedfacility_field() {
        let facility = fully_mapped_facility();

        for field in field_names(&facility) {
            if field == FACILITY_FIELD_NOT_COVERED_BY_REFRESH {
                continue;
            }
            assert!(
                facility_field_value(&facility, &field).is_some(),
                "facility_field_value has no arm for MappedFacility field `{field}` (or it \
                 doesn't return the field's real value) -- it was likely added to the struct \
                 but forgotten here",
            );
        }
    }

    #[test]
    fn facility_fields_that_differ_reports_every_mappedfacility_field_that_actually_changed() {
        let a = fully_mapped_facility();
        let b = MappedFacility {
            name: a.name.clone().map(|v| format!("{v} (fresh)")),
            street_address: a.street_address.clone().map(|v| format!("{v} (fresh)")),
            city: a.city.clone().map(|v| format!("{v} (fresh)")),
            state: a.state.clone().map(|v| format!("{v} (fresh)")),
            zip: a.zip.clone().map(|v| format!("{v} (fresh)")),
            phone: a.phone.clone().map(|v| format!("{v} (fresh)")),
            email: a.email.clone().map(|v| format!("{v} (fresh)")),
            units_count: a.units_count.map(|v| v + 1),
            primary_storage_offering: a
                .primary_storage_offering
                .clone()
                .map(|v| format!("{v} (fresh)")),
            previous_pms: a.previous_pms.clone().map(|v| format!("{v} (fresh)")),
            access_control_system: a
                .access_control_system
                .clone()
                .map(|v| format!("{v} (fresh)")),
            go_live_date: a.go_live_date,
            dropbox_folder_url: a.dropbox_folder_url.clone().map(|v| format!("{v} (fresh)")),
            subdomain: a.subdomain.clone().map(|v| format!("{v} (fresh)")),
            subdomain_exists_in_qms_raw: a
                .subdomain_exists_in_qms_raw
                .clone()
                .map(|v| format!("{v} (fresh)")),
            system_email: a.system_email.clone().map(|v| format!("{v} (fresh)")),
            website_url: a.website_url.clone().map(|v| format!("{v} (fresh)")),
        };

        let changed = facility_fields_that_differ(&a, &b);

        for field in field_names(&a) {
            if field == FACILITY_FIELD_NOT_COVERED_BY_REFRESH {
                continue;
            }
            assert!(
                changed.contains(&field.as_str()),
                "facility_fields_that_differ did not report `{field}` as changed even though \
                 this test set it to a different value on both sides -- it was likely added \
                 to MappedFacility but forgotten here",
            );
        }
    }

    fn fully_mapped_facility() -> MappedFacility {
        MappedFacility {
            name: Some("Example Facility".to_string()),
            street_address: Some("1 Example St".to_string()),
            city: Some("Example City".to_string()),
            state: Some("IL".to_string()),
            zip: Some("60000".to_string()),
            phone: Some("555-000-0000".to_string()),
            email: Some("facility@example.com".to_string()),
            units_count: Some(100),
            primary_storage_offering: Some("Standard Self-Storage".to_string()),
            previous_pms: Some("3rd Party PMS".to_string()),
            access_control_system: Some("Keypad".to_string()),
            go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            dropbox_folder_url: Some("https://example.com/dropbox".to_string()),
            subdomain: Some("example".to_string()),
            subdomain_exists_in_qms_raw: Some("No".to_string()),
            system_email: Some("system@example.com".to_string()),
            website_url: Some("https://example.com".to_string()),
        }
    }
}
