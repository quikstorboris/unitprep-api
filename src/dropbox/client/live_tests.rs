//! Real-network tests against the actual Dropbox account. `#[ignore]`d --
//! they need real `DROPBOX_*` values in `.env.local`.

use super::dto::Entry;
use super::*;

// Real-network test against the actual Dropbox account and QMS
// Onboarding folder -- no mocking, matching this codebase's existing
// #[ignore]d real-credential tests (see
// auth::authenticated_user's and auth::roles's DB-backed ones).
// Requires .env.local to hold real DROPBOX_* values. Run with:
//   cargo test --ignored dropbox
#[tokio::test]
#[ignore]
async fn lists_the_real_qms_onboarding_folder() {
    let _ = dotenvy::from_filename(".env.local");

    let config = DropboxConfig::from_env()
        .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
    let root_path = config.root_path.clone();
    let client = DropboxClient::new(config);

    let entries = client
        .list_folder(&root_path)
        .await
        .expect("list_folder should succeed against the real QMS Onboarding folder");

    assert!(
        entries.len() > 200,
        "expected roughly 282 customer subfolders, got {}",
        entries.len()
    );
    assert!(
        entries
            .iter()
            .any(|e| e.is_folder() && e.name == "Papa Ducks"),
        "expected to find the known 'Papa Ducks' subfolder"
    );
}

// Same real-network reasoning as the test above. Searches for a
// known facility ("Highway 20 Self Storage", under client "Prairie
// Enterprises LLC") by a facility-only term, verifying both that the
// folder-only filter actually drops the many file-name matches this
// query also hits (rent rolls, unit lists, templates) and that a
// generic query term still surfaces the facility folder itself
// despite not naming the client at all.
#[tokio::test]
#[ignore]
async fn search_folders_finds_a_facility_by_name_alone() {
    let _ = dotenvy::from_filename(".env.local");

    let config = DropboxConfig::from_env()
        .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
    let client = DropboxClient::new(config);

    let folders = client
        .search_folders("Highway 20")
        .await
        .expect("search_folders should succeed against the real QMS Onboarding folder");

    assert!(
        folders.iter().all(Entry::is_folder),
        "every returned entry should be a folder, not a file match"
    );
    assert!(
        folders
            .iter()
            .any(|e| e.name == "Highway 20 Self Storage"
                && e.path_display.contains("Prairie Enterprises LLC")),
        "expected to find the Highway 20 Self Storage facility folder without searching by client name"
    );
}

// Real-network, read-only: resolve_shared_link is the primary,
// reliable path -- Highway 20's own real dropbox_folder_url, whose
// name matches OO's facility name.
#[tokio::test]
#[ignore]
async fn resolves_highway_20s_real_shared_link_to_its_actual_path() {
    let _ = dotenvy::from_filename(".env.local");

    let config = DropboxConfig::from_env()
        .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
    let client = DropboxClient::new(config);

    let found = client
        .resolve_shared_link(
            "https://www.dropbox.com/scl/fo/iptn5zwsl0c4tr74r4jfi/AH4kjB7xiOg16DHFgVJ7J1M?rlkey=ahi804fhw7d141w3gj2e45ltx&st=cinc446y&dl=0",
        )
        .await
        .expect("resolving a real facility's own shared link must succeed")
        .expect("Highway 20's own real link must resolve to a real path");

    assert!(found.is_folder());
    assert!(found.path_display.to_lowercase().contains("highway 20"));
}

// Real-network, read-only: the case that actually matters --
// Sand-Sto's own real dropbox_folder_url resolves to its real
// folder ("Sand-Sto Storage") even though OO's own facility name
// ("Sand-Sto Climate Controlled Storage") doesn't match it at all.
// Confirms this mechanism never depends on the name matching, unlike
// find_facility_folder's own name-search fallback.
#[tokio::test]
#[ignore]
async fn resolves_sand_stos_real_shared_link_despite_the_oo_name_mismatch() {
    let _ = dotenvy::from_filename(".env.local");

    let config = DropboxConfig::from_env()
        .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
    let client = DropboxClient::new(config);

    let found = client
        .resolve_shared_link(
            "https://www.dropbox.com/scl/fo/8yhn4pue198c2gzcqwyxt/AI6hvEdb_Mepxjumow8fAig?rlkey=5sn69x7ouu3kduvf9my112lww&st=bvd1b2gc&dl=0",
        )
        .await
        .expect("resolving a real facility's own shared link must succeed")
        .expect("Sand-Sto's own real link must resolve to a real path");

    assert!(found.is_folder());
    assert_eq!(found.name, "Sand-Sto Storage");
}

// Real-network, read-only: proves `find_facility_folder` locates the
// real, writable path -- confirmed live 2026-09-04 to be the same
// physical folder `dropbox_folder_url`'s own shared link points at
// (same subfolders: Final Data, Preliminary Data, Tenants & Leases
// Migration, Units Migration, Validation), reached by name search
// under this app's own root as a fallback when `resolve_shared_link`
// isn't available.
#[tokio::test]
#[ignore]
async fn finds_a_real_facilitys_own_folder_by_exact_name() {
    let _ = dotenvy::from_filename(".env.local");

    let config = DropboxConfig::from_env()
        .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
    let client = DropboxClient::new(config);

    let found = client
        .find_facility_folder("Highway 20 Self Storage")
        .await
        .expect("searching for a real facility's folder must succeed")
        .expect("Highway 20 Self Storage's own folder must be found");

    assert!(found.is_folder());
    assert!(found
        .path_display
        .to_lowercase()
        .contains("prairie enterprises llc"));
}

// Real-network confirmation of the actual case found live 2026-09-04:
// OO's own facility name ("Sand-Sto Climate Controlled Storage")
// doesn't match its real Dropbox folder name ("Sand-Sto Storage") --
// and searching for OO's own name doesn't even reliably surface the
// real folder as a candidate at all (Dropbox's own search returned
// exactly one result, and it was an unrelated folder,
// `sand_sto_climate_control_storage_decrypt`). This must resolve to
// nothing, not a wrong guess -- see `pick_facility_folder`'s own doc
// comment for the full story of why a same-day fallback attempt here
// was reverted.
#[tokio::test]
#[ignore]
async fn resolves_to_nothing_for_a_facility_whose_dropbox_folder_name_differs_from_oos_name() {
    let _ = dotenvy::from_filename(".env.local");

    let config = DropboxConfig::from_env()
        .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
    let client = DropboxClient::new(config);

    let found = client
        .find_facility_folder("Sand-Sto Climate Controlled Storage")
        .await
        .expect("the search call itself must still succeed");

    assert!(
        found.is_none(),
        "must not guess at an unrelated folder when nothing matches exactly"
    );
}

// A name that matches files but no exact-named folder (every real
// Highway 20 CSV export mentions the facility name) must not
// false-positive on one of those files' own containing folder.
#[tokio::test]
#[ignore]
async fn returns_none_when_no_folder_matches_the_name_exactly() {
    let _ = dotenvy::from_filename(".env.local");

    let config = DropboxConfig::from_env()
        .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
    let client = DropboxClient::new(config);

    let found = client
        .find_facility_folder("Highway 20 Self Storage Unit Coverages")
        .await
        .expect("the search itself must still succeed even with no exact match");

    assert!(found.is_none());
}

// Real-network, and genuinely mutating: creates a folder in the real
// QMS Onboarding tree. Deliberately targets a path nested under the
// real Highway 20 folder used by the test above (not a throwaway
// top-level folder), named so it's unambiguous as a test artifact if
// ever seen by a human. Run manually and clean up in Dropbox after --
// not something to fire automatically.
#[tokio::test]
#[ignore]
async fn create_folder_if_missing_is_idempotent_against_the_real_account() {
    let _ = dotenvy::from_filename(".env.local");

    let config = DropboxConfig::from_env()
        .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
    let client = DropboxClient::new(config);
    let path = format!(
        "{}/_unitprep_dropbox_client_test_scratch",
        client.root_path()
    );

    client
        .create_folder_if_missing(&path)
        .await
        .expect("creating a genuinely new folder must succeed");
    client
        .create_folder_if_missing(&path)
        .await
        .expect("creating the same folder again must be treated as success, not an error");
}
