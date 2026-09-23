//! Structural answer to the gap THREAT_MODEL.md names under "Known
//! gaps": *"No formal, automated check that every new privilege-gated
//! handler actually calls `AuthenticatedUser::require_permission`."*
//! Before roles/permissions became data-driven tables (2026-08-06), a
//! closed `Role` enum meant adding a role forced every `match
//! admin.role` site to grow an arm or fail to compile -- a real,
//! compiler-enforced backstop. That backstop is gone; this is its
//! structural replacement.
//!
//! `Router::route` is never re-exposed here. `GatedRouter::gated_route`
//! is the only way to register a route through it, and it requires a
//! `RouteAccess` classification for every HTTP method the route binds --
//! so a new route simply cannot be wired up in `router.rs` without
//! someone deciding, at that exact call site, what authorizes it. This
//! only forces the *declaration* to exist, not that it's true --
//! `router.rs`'s own `permission_gate_tests` module closes that second
//! half by calling the real handler behind every `Permission`-classified
//! entry with a caller who holds no permissions, and failing loudly if
//! it doesn't 403.

use axum::http::Method;
use axum::routing::MethodRouter;
use axum::Router;

/// How a single (path, HTTP method) pair authorizes its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteAccess {
    /// No session required at all -- health checks.
    Public,
    /// Unauthenticated by design, gated by its own ceremony/token logic
    /// instead of a session: passkey registration, login. Sign-out is
    /// `Public` above, not this -- it must succeed even with a stale or
    /// missing cookie, which is a different reason than a ceremony's.
    AuthCeremony,
    /// Any authenticated caller may reach this; no specific permission
    /// or role is checked, and Postgres RLS isn't doing meaningfully
    /// different work per caller either (every table is still
    /// deny-by-default RLS per THREAT_MODEL.md, but that's not the
    /// *intended* access boundary for this particular route the way it
    /// is for `RlsRead`/`RlsWrite` below).
    Authenticated,
    /// Read-only; any authenticated caller may call it, but Postgres RLS
    /// -- not this app layer -- is the documented, intended access
    /// boundary on which rows come back.
    RlsRead,
    /// A write whose documented, intended access boundary is Postgres
    /// RLS narrowing which rows the caller's role may touch, not an
    /// app-layer permission check.
    RlsWrite,
    /// Requires `AuthenticatedUser::require_permission` to pass for
    /// every key in `keys` (almost always one; `/auth/invites` needs
    /// two -- see `auth_invites::create_invite`'s own doc comment for
    /// why). `action` is the same string threaded into that call's
    /// audit-log metadata, kept here so the classification and the real
    /// check stay readably paired.
    Permission {
        keys: &'static [&'static str],
        action: &'static str,
    },
}

/// One (path, HTTP method) pair's recorded classification, as collected
/// by `GatedRouter::gated_route`. `router()` (the real server) discards
/// every manifest entry it builds via `into_parts().0` -- only
/// `router.rs`'s own `#[cfg(test)] mod permission_gate_tests` ever reads
/// these fields back, hence the blanket `allow` below rather than a real
/// production caller.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RouteEntry {
    pub path: &'static str,
    pub method: Method,
    pub access: RouteAccess,
}

/// Thin wrapper around `axum::Router<S>`. Deliberately does not re-expose
/// `Router::route` -- see the module doc comment above for why.
pub struct GatedRouter<S> {
    router: Router<S>,
    manifest: Vec<RouteEntry>,
}

impl<S> GatedRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            manifest: Vec::new(),
        }
    }

    /// Registers `path` and records `access` for each `(Method,
    /// RouteAccess)` pair supplied. Most routes bind one method to one
    /// classification; a handful bind more than one method to
    /// *different* classifications (e.g. `/clients/{company_id}`'s GET
    /// is `Authenticated`, its DELETE is `Permission`) -- which is why
    /// this takes a list rather than a single `RouteAccess` for the
    /// whole path. axum has no public way to introspect which methods a
    /// `MethodRouter` binds, so the caller states them explicitly here,
    /// the same way it already states them by calling `get(...)`/
    /// `post(...)`/etc. to build `method_router` in the first place.
    pub fn gated_route<const N: usize>(
        mut self,
        path: &'static str,
        method_router: MethodRouter<S>,
        access: [(Method, RouteAccess); N],
    ) -> Self {
        for (method, access) in access {
            self.manifest.push(RouteEntry {
                path,
                method,
                access,
            });
        }

        self.router = self.router.route(path, method_router);
        self
    }

    pub fn merge(mut self, other: Self) -> Self {
        self.router = self.router.merge(other.router);
        self.manifest.extend(other.manifest);
        self
    }

    /// Escape hatch for layers (rate limiting, body limits) -- these
    /// apply to requests generically, not to a specific route's
    /// authorization, so they don't touch the manifest. Takes a closure
    /// rather than re-declaring `axum::Router::layer`'s own trait bounds
    /// here, which are long and change with axum's own version.
    pub fn map_router(mut self, f: impl FnOnce(Router<S>) -> Router<S>) -> Self {
        self.router = f(self.router);
        self
    }

    pub fn with_state<S2>(self, state: S) -> GatedRouter<S2> {
        GatedRouter {
            router: self.router.with_state(state),
            manifest: self.manifest,
        }
    }

    /// Consumes this `GatedRouter`, returning the underlying
    /// `axum::Router` plus every route's recorded classification --
    /// `router()` discards the manifest for the real server; tests use
    /// it to drive the permission-gate check.
    pub fn into_parts(self) -> (Router<S>, Vec<RouteEntry>) {
        (self.router, self.manifest)
    }
}
