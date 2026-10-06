//! The routes that sit behind a per-IP rate limit -- the anonymous
//! sign-in/registration ceremonies and the admin invite endpoints -- plus
//! the background task that keeps those limiters from leaking memory.
//! Split out of `build()` so the limiter configuration and its reasoning
//! live next to the routes they protect.

use std::sync::Arc;
use std::time::Duration;

use axum::{http::Method, routing::post};
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};

use crate::api::route_access::{GatedRouter, RouteAccess};
use crate::api::{auth_invites, auth_login, auth_register, AppState};

use super::super::rate_limit_exceeded_with_audit;

/// The auth-ceremony routes and the invite routes, each with its own
/// rate-limit bucket, merged into one router. Spawns the limiter cleanup
/// task, so call it once per router build.
pub(super) fn rate_limited_routes(state: &AppState) -> GatedRouter<AppState> {
    // Ten requests answered immediately, one more every three seconds
    // after that (~20/min sustained) -- generous enough that a real
    // person retrying a cancelled Windows Hello prompt or fumbling a
    // TOTP code a few times in a row never notices this exists, while
    // bounding how fast an anonymous caller can iterate through
    // addresses or guess codes against these endpoints. Keying is by the
    // TCP peer address (`tower_governor`'s default `PeerIpKeyExtractor`),
    // never a client-supplied header -- this deliberately does not
    // attempt to trust `X-Forwarded-For`, since no trusted-reverse-proxy
    // policy exists yet (see the `ip_address` NULL comments in
    // auth_register.rs / auth_login.rs for the same open question). Once
    // real client IPs need trusting for any reason, this and that NULL
    // should be revisited together, not separately -- they are the same
    // unresolved question in two places. Until then, behind a reverse
    // proxy that does not preserve the original TCP peer, this still
    // limits correctly, just coarsely: every client behind that proxy
    // shares one bucket rather than getting one each, which is strictly
    // more restrictive than intended, never less.
    let auth_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(3)
            .burst_size(10)
            .finish()
            .expect("auth rate-limit config: burst size and period are both non-zero constants"),
    );

    // A separate, more generous bucket for invite creation: authenticated
    // and admin-only already, so this is bounding accidental or scripted
    // hammering by a trusted caller, not probing by an anonymous one.
    let invite_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(20)
            .finish()
            .expect("invite rate-limit config: burst size and period are both non-zero constants"),
    );

    // The keyed limiter accumulates one entry per distinct peer IP it has
    // ever seen and nothing prunes that on its own -- `retain_recent()`
    // is `governor`'s own answer, and it has to be called from somewhere.
    // Mirrors `InMemorySessionStore::start_cleanup_task`: a background
    // tick that must keep running even if one iteration panics, since the
    // alternative is the rate limiter quietly becoming a slow memory leak
    // for the life of the process.
    {
        let auth_limiter = auth_rate_limit.limiter().clone();
        let invite_limiter = invite_rate_limit.limiter().clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));

            loop {
                interval.tick().await;

                // catch_unwind, not just calling these directly: the doc
                // comment above has always claimed this loop survives a
                // panicking tick, but nothing enforced that -- a real
                // panic inside retain_recent() would kill this spawned
                // task silently and permanently, quietly resuming the
                // exact memory leak this task exists to prevent, with no
                // log line anywhere saying so.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    auth_limiter.retain_recent();
                    invite_limiter.retain_recent();
                }));

                if let Err(panic) = result {
                    let message = panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("unknown panic payload");

                    tracing::error!(
                        panic = %message,
                        "rate-limit cleanup tick panicked; will retry on the next tick"
                    );
                }
            }
        });
    }

    // Split out as their own routers purely so the rate-limit layer
    // applies to exactly these paths and nothing else -- merged back into
    // the main router below while it is still `GatedRouter<AppState>`,
    // since `.merge` requires matching state types and `.with_state`
    // further down converts the main chain to `GatedRouter<()>`.
    let auth_routes = GatedRouter::new()
        .gated_route(
            "/auth/register/begin",
            post(auth_register::register_begin),
            [(Method::POST, RouteAccess::AuthCeremony)],
        )
        .gated_route(
            "/auth/register/finish",
            post(auth_register::register_finish),
            [(Method::POST, RouteAccess::AuthCeremony)],
        )
        .gated_route(
            "/auth/login/begin",
            post(auth_login::login_begin),
            [(Method::POST, RouteAccess::AuthCeremony)],
        )
        .gated_route(
            "/auth/login/finish",
            post(auth_login::login_finish),
            [(Method::POST, RouteAccess::AuthCeremony)],
        )
        .map_router(|r| {
            r.layer(
                GovernorLayer::new(auth_rate_limit)
                    .error_handler(rate_limit_exceeded_with_audit("auth", state.db.clone())),
            )
        });

    let invite_routes = GatedRouter::new()
        // Admin-only. Authorization is `require_permission` inside the
        // handler plus the admin-only RLS policies underneath it, not a
        // route-level guard -- there is no middleware layer that could be
        // reordered away from this path.
        .gated_route(
            "/auth/invites",
            post(auth_invites::create_invite),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["users.manage", "users.manage_roles"],
                    action: "create_invite",
                },
            )],
        )
        // Despite the name, this is an admin-only account-recovery action
        // (revokes and reissues every credential on the target account),
        // not a self-service "forgot my passkey" flow -- see
        // THREAT_MODEL.md's actor table: "isn't reachable unauth -- admin
        // only". Shares this bucket rather than the anonymous
        // `auth_routes` one above -- same trust level as invite creation.
        .gated_route(
            "/auth/invites/recover",
            post(auth_invites::recover_account),
            [(
                Method::POST,
                RouteAccess::Permission {
                    keys: &["users.manage"],
                    action: "recover_account",
                },
            )],
        )
        .map_router(|r| {
            r.layer(
                GovernorLayer::new(invite_rate_limit)
                    .error_handler(rate_limit_exceeded_with_audit("invite", state.db.clone())),
            )
        });

    auth_routes.merge(invite_routes)
}
