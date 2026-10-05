use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{ConnectOptions, Connection};

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

    Ok(pool_options(PING_IDLE_THRESHOLD).connect_lazy_with(connect_options))
}

/// A pooled connection that has sat idle at least this long is pinged
/// before it is handed out; one used more recently is trusted.
const PING_IDLE_THRESHOLD: Duration = Duration::from_secs(30);

/// How long a caller waits for a pooled connection (including opening a new
/// one) before giving up with `PoolTimedOut`. sqlx's default is 30 s, which
/// turns pool exhaustion into a request that hangs for half a minute; this
/// still leaves comfortable room for a cold start of a suspended Neon
/// compute while failing a starved request in a bounded, visible way.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// The application pool's sizing and health-check policy, separate from
/// `connect()` so it can be asserted on and benchmarked.
///
/// **Ping only connections that have been idle a while.** sqlx's default
/// (`test_before_acquire(true)`) pings EVERY connection on EVERY acquire --
/// a full extra network round trip before the caller's own first statement,
/// on every transaction and every pooled query in the app. Against a
/// remote Postgres that is a visible slice of each request. A connection
/// that was in use moments ago is almost certainly healthy, so it skips the
/// ping; one idle past `ping_idle_threshold` (long enough for a pooler,
/// proxy or the database to have dropped it) is still pinged, and a dead
/// one is discarded and replaced rather than handed to a request. The
/// remaining risk is a connection that dies inside that window, which fails
/// the single statement that finds it -- a rare, bounded cost against a
/// round trip saved on every acquire.
pub(crate) fn pool_options(ping_idle_threshold: Duration) -> PgPoolOptions {
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
    PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .test_before_acquire(false)
        .before_acquire(move |conn, meta| {
            Box::pin(async move {
                if meta.idle_for >= ping_idle_threshold {
                    conn.ping().await?;
                }
                Ok(true)
            })
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_pings_only_idle_connections_and_bounds_the_wait_for_one() {
        let options = pool_options(PING_IDLE_THRESHOLD);

        assert_eq!(options.get_max_connections(), 20);
        assert_eq!(options.get_acquire_timeout(), POOL_ACQUIRE_TIMEOUT);
        assert!(
            !options.get_test_before_acquire(),
            "the unconditional ping-on-every-acquire must stay off; the idle-gated before_acquire replaces it"
        );
    }

    /// Benchmark, not a pass/fail test: what the idle-gated ping saves.
    /// Meaningful only through `dev-tools/latency_proxy.py`, which adds
    /// artificial network latency in front of the local `test-db`:
    ///
    /// ```text
    /// python3 dev-tools/latency_proxy.py --delay-ms 10 &      # 20 ms round trip
    /// TEST_DATABASE_URL=postgres://app_service:app_service@127.0.0.1:5434/unitprep_test \
    ///   cargo test --release --bin unitprep -- --ignored --nocapture pool_ping_policy
    /// ```
    ///
    /// Each iteration does what a handler does: acquire a pooled
    /// connection, open an RLS transaction (BEGIN + set_config), run one
    /// query, commit. Prints the mean per iteration for sqlx's default
    /// (ping on every acquire) and for `pool_options`.
    #[tokio::test]
    #[ignore = "benchmark; needs test-db behind dev-tools/latency_proxy.py"]
    async fn pool_ping_policy_latency_benchmark() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        assert!(!url.contains("neon.tech"), "never run against Neon");
        let connect_options: PgConnectOptions = url.parse().unwrap();
        let user = uuid::Uuid::new_v4();
        let roles = vec!["admin".to_string()];
        const ITERATIONS: u32 = 40;

        async fn mean_ms(pool: &PgPool, user: uuid::Uuid, roles: &[String]) -> f64 {
            // Warm the pool so connection setup is not measured.
            for _ in 0..3 {
                let mut tx = crate::auth::begin_rls_transaction(pool, user, roles)
                    .await
                    .unwrap();
                sqlx::query("SELECT 1").execute(&mut *tx).await.unwrap();
                tx.commit().await.unwrap();
            }
            let started = std::time::Instant::now();
            for _ in 0..ITERATIONS {
                let mut tx = crate::auth::begin_rls_transaction(pool, user, roles)
                    .await
                    .unwrap();
                sqlx::query("SELECT 1").execute(&mut *tx).await.unwrap();
                tx.commit().await.unwrap();
            }
            started.elapsed().as_secs_f64() * 1000.0 / f64::from(ITERATIONS)
        }

        let default_pool = PgPoolOptions::new()
            .max_connections(20)
            .connect_lazy_with(connect_options.clone());
        let tuned_pool = pool_options(PING_IDLE_THRESHOLD).connect_lazy_with(connect_options);

        let default_ms = mean_ms(&default_pool, user, &roles).await;
        let tuned_ms = mean_ms(&tuned_pool, user, &roles).await;

        println!(
            "per handler-style transaction: default pool {default_ms:.1} ms, tuned pool {tuned_ms:.1} ms ({:.1} ms saved)",
            default_ms - tuned_ms
        );
    }

    /// Needs the local `test-db` (see docker-compose.yml). Exercises both
    /// branches of the idle-gated ping against a real server: a threshold
    /// of zero pings on every acquire, a huge one never does, and the pool
    /// must hand out a working connection either way.
    #[tokio::test]
    #[ignore = "needs the local test-db"]
    async fn the_idle_gated_ping_hands_out_working_connections_on_both_branches() {
        let _ = dotenvy::from_filename(".env.local");
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        assert!(!url.contains("neon.tech"), "never run against Neon");
        let connect_options: PgConnectOptions = url.parse().unwrap();

        for threshold in [Duration::ZERO, Duration::from_secs(3600)] {
            let pool = pool_options(threshold).connect_lazy_with(connect_options.clone());
            for _ in 0..3 {
                let one: i32 = sqlx::query_scalar("SELECT 1")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                assert_eq!(one, 1);
            }
        }
    }
}
