//! Every long-lived background task the server starts, in one place.
//! Called once from `main` with the finished state, before serving.

use unitprep_core::vendor_format::ContentType;

use crate::api::AppState;

use super::state::DurableStores;
use crate::{client_ops, clients};

pub(super) fn spawn(state: &AppState, stores: &DurableStores) {
    // Expire idle sessions and ceremonies.
    stores.start_cleanup_tasks();

    // The background sync that keeps clients.ps_person_index fresh --
    // only runs when PS is actually configured. See `clients::sync`'s own
    // module doc for the delta-sync mechanism and the RLS reasoning
    // behind SYSTEM_USER_ID.
    if let Some(client) = &state.process_street {
        clients::sync::start_background_sync_task(
            client.clone(),
            state.db.clone(),
            state.sync_progress.clone(),
        );
    }

    // Group Prep's and dedup's vendor-format registries -- an in-memory
    // snapshot per content type, loaded once at startup (best-effort; see
    // `initial_cache`'s own doc comment for why a failure there doesn't
    // panic startup) and kept fresh by these tasks, never queried per
    // request. See `client_ops::vendor_format`'s module doc comment for
    // the full reasoning.
    client_ops::vendor_format::start_refresh_task(
        state.unit_vendors.clone(),
        state.db.clone(),
        ContentType::Units,
    );
    client_ops::vendor_format::start_refresh_task(
        state.tenant_vendors.clone(),
        state.db.clone(),
        ContentType::Tenants,
    );
    client_ops::vendor_file_meta::start_refresh_task(
        state.tenant_file_meta.clone(),
        state.db.clone(),
        ContentType::Tenants,
    );
}
