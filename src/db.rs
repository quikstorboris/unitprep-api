use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::ConnectOptions;

/// Builds the application database connection pool from DATABASE_URL.
///
/// Uses connect_lazy_with rather than connect deliberately: this pool must
/// not block application startup on Postgres being reachable, since most
/// of UnitPrep's existing endpoints (upload/discover/validate/etc.) do
/// not touch the database at all yet, and the app_service credential may
/// not even be filled in yet during early setup. A bad or unreachable
/// URL only surfaces the first time something actually queries through
/// this pool (see the /health/db endpoint in api/mod.rs) rather than
/// crashing the whole binary.
///
/// DATABASE_URL must be the app_service role's connection string, never
/// the owner/direct one -- connecting as the table owner bypasses every
/// row-level security policy in the schema silently.
pub fn connect() -> Result<PgPool, sqlx::Error> {
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set -- see .env.local");

    // NO search_path is set on the connection, deliberately. Every
    // application query schema-qualifies its auth objects instead
    // (`auth.users`, `auth.resolve_session(...)`, ...).
    //
    // This used to set `options=[("search_path", "auth,public")]`, which
    // works on a direct connection and fails outright on Neon's pooled
    // endpoint: `search_path` travels in the Postgres startup packet, and
    // the pooler rejects unsupported startup parameters with
    // "unsupported startup parameter in options: search_path". Every
    // query failed, including /health/db.
    //
    // Moving it to a per-connection `SET search_path` via after_connect
    // would not fix it either. The pooler is transaction-mode PgBouncer,
    // so a session-level SET is not reliably tied to the client that
    // issued it -- it would appear to work under light load and start
    // leaking or vanishing under concurrency, which is worse than
    // failing.
    //
    // Schema-qualifying is the only form that is correct on both
    // endpoints and under pooling. The cost is that an unqualified name
    // added later fails at runtime rather than compile time -- see the
    // note in scripts/setup_app_service_role.sql on the search_path the
    // migration connection uses, which differs again.
    // sqlx already emits a `sqlx::query` tracing event for every query,
    // WARN-level for anything over this threshold (see main.rs's
    // "sqlx=warn" filter, which is what actually surfaces it) -- nothing
    // else in this app needs to instrument query latency by hand.
    // Default threshold is 1s, generous for a CRUD app this size;
    // tightened here so a genuinely slow query shows up promptly rather
    // than only once it's already severe.
    let connect_options: PgConnectOptions = database_url
        .parse::<PgConnectOptions>()?
        .log_slow_statements(log::LevelFilter::Warn, Duration::from_millis(200));

    // 20, not 5 (2026-09-03): `clients_detail`'s Company/Facility Policies
    // endpoints deliberately open several short-lived RLS transactions
    // concurrently (`tokio::join!`, one connection each) to cut real
    // network round trips to Neon rather than serialize them -- 5 was too
    // small a pool for that, so with up to 7 concurrent transactions from
    // one request, 2 of them queued for a free connection and the fix
    // barely helped (measured: facility_policies stayed ~630ms, no better
    // than the pre-fix serial version). This is Neon's own pooled
    // (`-pooler`) endpoint, itself a PgBouncer in front of Postgres, so
    // the app holding 20 connections against it is unremarkable.
    Ok(PgPoolOptions::new()
        .max_connections(20)
        .connect_lazy_with(connect_options))
}
