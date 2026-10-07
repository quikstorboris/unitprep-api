//! Binding the listener and serving until a shutdown signal.

use std::net::SocketAddr;

use crate::api::{self, AppState};

use super::config;

pub(super) async fn run(state: AppState) {
    let app = api::router(state);

    let (host, port) = config::listen_host_and_port();
    let addr = format!("{host}:{port}");

    // A plain `.unwrap()` here used to panic with just "Address already
    // in use" and no next step -- the actually useful information (which
    // *other* process is holding the port) isn't something this process
    // can look up about itself, so the fix is pointing at the command
    // that finds it, not trying to embed a PID we don't have.
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!(
                "Failed to start: {addr} is already in use — another unitprep instance is likely still running.\nOn the host: find it with `ss -ltnp | grep :{port}` (or `lsof -i :{port}`) and stop it before starting a new one.\nRunning inside the api-dev Docker container? Host-level tools won't see it -- the other instance is almost always still alive inside this SAME container (e.g. a previous `cargo run` left running). Clear it with: `docker compose up -d --force-recreate api-dev`."
            );

            std::process::exit(1);
        }
        Err(err) => {
            panic!("Failed to bind to {addr}: {err}");
        }
    };

    tracing::info!(
        pid = std::process::id(),
        "UnitPrep API listening on http://{addr}"
    );

    // `with_connect_info` rather than plain `into_make_service` -- the
    // auth rate limiter (api::router) keys by peer IP via
    // `ConnectInfo<SocketAddr>`, which only ever gets populated this way.
    //
    // `with_graceful_shutdown` matters beyond the log line it lets us add
    // below: without it, Ctrl+C/SIGTERM kill the process immediately,
    // mid-request, rather than letting axum finish in-flight requests
    // first. Previously there was no signal handling at all -- the
    // process simply stopped, with nothing recorded either way.
    if let Err(err) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    {
        tracing::error!(error = %err, "server stopped with an error");
        std::process::exit(1);
    }

    tracing::info!("UnitPrep API stopped");
}

/// Waits for Ctrl+C or (on Unix) SIGTERM -- the signal systemd/Docker/Fly
/// send for a normal stop, as opposed to SIGKILL, which nothing can
/// intercept or log. Logs which one fired, so a deliberate stop is
/// distinguishable in the logs from the process just disappearing.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("received Ctrl+C, shutting down gracefully");
        }
        _ = terminate => {
            tracing::info!("received SIGTERM, shutting down gracefully");
        }
    }
}
