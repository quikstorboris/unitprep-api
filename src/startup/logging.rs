//! Tracing subscriber and panic hook, installed before anything else
//! that might log or panic.

/// Defaults to `info` (aggregate summaries only) when RUST_LOG isn't
/// set. Deep per-request tracing is still available on demand via
/// `RUST_LOG=unitprep=debug` -- it's just no longer forced on by
/// default, which is what made every discovery/upload run emit
/// hundreds of per-file DEBUG lines regardless of what the operator
/// actually wanted to see.
/// "sqlx=warn" surfaces sqlx's own built-in slow-query events (see
/// db.rs's log_slow_statements call) without also turning on its
/// per-query DEBUG noise -- that instrumentation already runs on
/// every query today, this just stops filtering the slow ones out.
pub(super) fn init() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("unitprep=info,sqlx=warn")),
        )
        .init();

    // A panic (the db_pool/dropbox_client/auth_backend/cookie-security
    // startup ones, or anything later) prints only to raw stderr via
    // Rust's default panic hook, entirely bypassing the tracing
    // subscriber just configured above -- invisible to anything that
    // collects this process's logs by following its tracing output
    // rather than tailing stderr directly. Wrapping the default hook
    // (not replacing it) keeps the familiar stderr backtrace for local
    // dev while also emitting a structured tracing event through the
    // same pipe everything else this process logs through.
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        tracing::error!(panic = %panic_info, "panicked");
        default_panic_hook(panic_info);
    }));
}
