//! The full route table -- every path this API serves, paired with the
//! `RouteAccess` that authorizes it (see `route_access`'s module doc for
//! why `GatedRouter` makes that pairing unavoidable). Split out from
//! `router/mod.rs` (2026-09-24) once that file crossed 1900 lines: this
//! table and `permission_gate_tests`'s proof that it's actually enforced
//! were together most of that growth -- two genuinely separable concerns
//! (route declarations vs. response-shaping middleware) that had been
//! sharing one file since before either existed.

mod account;
mod clickup;
mod client_ops;
mod clients;
mod integrations;
mod rate_limited;
mod tools;

use axum::extract::DefaultBodyLimit;

use super::super::route_access::GatedRouter;
use super::super::AppState;

/// Builds the whole route tree via `GatedRouter` -- see `route_access`'s
/// module doc for why every route below carries an explicit `RouteAccess`
/// at its call site. Kept separate from `router()`/`with_response_layers`
/// so tests can call this directly (with a fixture `AppState`) purely to
/// read back the manifest, without needing to also build the response-
/// shaping layers below, none of which affect authorization.
pub(super) fn build(state: AppState) -> GatedRouter<()> {
    GatedRouter::new()
        .merge(account::health_routes())
        .merge(account::step_up_routes())
        .merge(account::user_routes())
        .merge(clickup::clickup_routes())
        .merge(account::auth_admin_routes())
        .merge(client_ops::client_ops_routes())
        .merge(clients::client_routes())
        .merge(clients::facility_routes())
        .merge(clients::client_sync_routes())
        .merge(integrations::integration_routes())
        .merge(account::audit_log_routes())
        .merge(account::logout_routes())
        .merge(rate_limited::rate_limited_routes(&state))
        .merge(tools::unit_group_routes())
        .merge(tools::dedup_routes())
        .merge(tools::tagger_routes())
        .merge(tools::tagger_check_route())
        .map_router(|r| r.layer(DefaultBodyLimit::max(100 * 1024 * 1024)))
        .with_state(state)
}
