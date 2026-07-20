mod ai;
mod api;
mod application;
mod infrastructure;

use std::sync::Arc;

use unitprep_core::in_memory_session_store::InMemorySessionStore;

use crate::api::AppState;
use crate::application::dedup_session_service::DedupSession;
use crate::application::unit_group_session::Session;

#[tokio::main]
async fn main() {
    // Defaults to `info` (aggregate summaries only) when RUST_LOG isn't
    // set. Deep per-request tracing is still available on demand via
    // `RUST_LOG=unitprep=debug` — it's just no longer forced on by
    // default, which is what made every discovery/upload run emit
    // hundreds of per-file DEBUG lines regardless of what the operator
    // actually wanted to see.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new(
                        "unitprep=info",
                    )
                }),
        )
        .init();

    // Overridable per deployment without a code change — defaults to
    // the same 10 minutes as before if unset or unparseable.
    let session_timeout_secs = std::env::var(
        "SESSION_TIMEOUT_SECS",
    )
    .ok()
    .and_then(|v| v.parse::<u64>().ok())
    .unwrap_or(60 * 10);

    let session_store =
        Arc::new(
            InMemorySessionStore::<Session>::with_timeout(
                std::time::Duration::from_secs(
                    session_timeout_secs,
                ),
            ),
        );

    session_store
        .start_cleanup_task();

    // Same timeout policy as unit_group_sessions — no reason for the
    // two tools' sessions to expire on different schedules today.
    let dedup_session_store =
        Arc::new(
            InMemorySessionStore::<DedupSession>::with_timeout(
                std::time::Duration::from_secs(
                    session_timeout_secs,
                ),
            ),
        );

    dedup_session_store
        .start_cleanup_task();

    let state = AppState {
        unit_group_sessions: session_store,
        dedup_sessions: dedup_session_store,
    };

    let app =
        api::router(state);

    // Defaults to 0.0.0.0 (all interfaces), not 127.0.0.1 — a container
    // runtime's proxy (Fly.io, Docker, etc.) connects over the container's
    // network interface, not loopback, so binding to 127.0.0.1 would make
    // the app unreachable from outside the container despite running fine
    // locally. HOST/PORT are the de-facto standard env vars most hosting
    // platforms inject; both are overridable for local conflicts.
    let host = std::env::var("HOST")
        .unwrap_or_else(|_| {
            "0.0.0.0".to_string()
        });

    let port = std::env::var("PORT")
        .unwrap_or_else(|_| {
            "8080".to_string()
        });

    let addr = format!("{host}:{port}");

    // A plain `.unwrap()` here used to panic with just "Address already
    // in use" and no next step — the actually useful information (which
    // *other* process is holding the port) isn't something this process
    // can look up about itself, so the fix is pointing at the command
    // that finds it, not trying to embed a PID we don't have.
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(err)
            if err.kind()
                == std::io::ErrorKind::AddrInUse =>
        {
            eprintln!(
                "Failed to start: {addr} is already in use — another unitprep instance is likely still running.\nFind it with `ss -ltnp | grep :{port}` (or `lsof -i :{port}`) and stop it before starting a new one."
            );

            std::process::exit(1);
        }
        Err(err) => {
            panic!(
                "Failed to bind to {addr}: {err}"
            );
        }
    };

    tracing::info!(
        pid = std::process::id(),
        "UnitPrep API listening on http://{addr}"
    );

    axum::serve(
        listener,
        app,
    )
    .await
    .unwrap();
}