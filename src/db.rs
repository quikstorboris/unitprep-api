use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

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
    let database_url = std::env::var("DATABASE_URL").expect(
        "DATABASE_URL must be set -- see .env.local",
    );

    // Every auth table/type/function lives in the `auth` schema now,
    // not `public` -- every unqualified name in application queries
    // (users, resolve_session, etc.) resolves through this. `public`
    // stays on the path behind it for shared extension types (citext).
    let connect_options: PgConnectOptions = database_url.parse()?;
    let connect_options = connect_options.options([("search_path", "auth,public")]);

    Ok(PgPoolOptions::new()
        .max_connections(5)
        .connect_lazy_with(connect_options))
}
