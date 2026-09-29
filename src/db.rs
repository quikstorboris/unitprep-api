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

/// Builds a database connection pool for `#[ignore]`'d real-DB tests
/// only -- never used by the running application, which always uses
/// [`connect`] above.
///
/// Deliberately reads a distinct env var (`TEST_DATABASE_URL`), never
/// `DATABASE_URL`, and never falls back to it -- see the vault's CI-CD
/// Framework doc's isolation controls. A test calling this can't
/// silently pick up a real Neon connection string left over from a
/// bind-mounted `.env.local` or a forgotten env override: it hard-fails
/// if `TEST_DATABASE_URL` is unset, and aborts loudly if the resolved
/// host looks like a Neon endpoint at all, as a second, independent
/// backstop against the same mistake (defense in depth, not just
/// discipline -- the same posture this project already applies to
/// auth).
///
/// Point this at the local ephemeral `test-db` Docker service (see
/// docker-compose.yml), connecting as `app_service`, never the
/// superuser -- connecting as the table owner bypasses every
/// row-level-security policy silently, which would make these tests
/// pass without actually proving RLS holds.
#[cfg(test)]
pub fn connect_test() -> PgPool {
    let database_url = std::env::var("TEST_DATABASE_URL").expect(
        "TEST_DATABASE_URL must be set -- point it at the local ephemeral test-db \
         Docker service (see docker-compose.yml), never at Neon. This is deliberately \
         a different variable from DATABASE_URL so a test can never silently connect \
         to a real database.",
    );

    let connect_options: PgConnectOptions = database_url
        .parse()
        .expect("TEST_DATABASE_URL must be a well-formed Postgres connection string");

    let host = connect_options.get_host();
    assert!(
        !host.ends_with(".neon.tech"),
        "TEST_DATABASE_URL resolves to a Neon host ({host}) -- this must never point \
         at Neon, dev or prod. Point it at the local ephemeral test-db instead."
    );

    PgPoolOptions::new()
        .max_connections(5)
        .connect_lazy_with(connect_options)
}
