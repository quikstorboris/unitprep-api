//! Tracing subscriber and panic hook, installed before anything else
//! that might log or panic.

use std::sync::OnceLock;

use sentry::integrations::tracing as sentry_tracing;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Days of rotated JSON log files kept when `LOG_DIR` is set.
const LOG_FILE_RETENTION_DAYS: usize = 7;

/// The file appender's background writer flushes when this guard drops,
/// so it must live as long as the process -- parked here rather than
/// returned so `init()`'s signature (and its one call site) stay put.
static FILE_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

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
///
/// Two independent outputs, both under that one filter:
/// - stdout: human-readable text by default (the dev terminal);
///   `LOG_FORMAT=json` switches it to JSON for a platform that
///   collects stdout (journald, a cloud log service).
/// - `LOG_DIR`, when set: a second JSON copy in daily-rotated files
///   (`unitprep-api.log.YYYY-MM-DD`), keeping the last
///   `LOG_FILE_RETENTION_DAYS`. A file rather than stdout because the
///   dev server is started with `docker compose exec ... cargo run`,
///   whose output Docker's own log capture never sees.
pub(super) fn init() {
    let filter = || {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("unitprep=info,sqlx=warn"))
    };

    let json_stdout = std::env::var("LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));
    let stdout_layer = if json_stdout {
        tracing_subscriber::fmt::layer().json().boxed()
    } else {
        tracing_subscriber::fmt::layer().boxed()
    };

    let file_layer = std::env::var("LOG_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .and_then(|dir| {
            let appender = tracing_appender::rolling::Builder::new()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix("unitprep-api")
                .filename_suffix("log")
                .max_log_files(LOG_FILE_RETENTION_DAYS)
                .build(&dir);
            match appender {
                Ok(appender) => {
                    let (writer, guard) = tracing_appender::non_blocking(appender);
                    let _ = FILE_GUARD.set(guard);
                    Some(
                        tracing_subscriber::fmt::layer()
                            .json()
                            .with_ansi(false)
                            .with_writer(writer),
                    )
                }
                Err(e) => {
                    // Subscriber isn't installed yet, so plain stderr.
                    eprintln!("LOG_DIR={dir} unusable ({e}); file logging disabled");
                    None
                }
            }
        });

    tracing_subscriber::registry()
        .with(stdout_layer.with_filter(filter()))
        .with(file_layer.map(|layer| layer.with_filter(filter())))
        // ERROR becomes a Sentry event, WARN/INFO a breadcrumb; a no-op
        // when no Sentry client is bound (SENTRY_DSN unset).
        .with(sentry_tracing::layer().with_filter(filter()))
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
