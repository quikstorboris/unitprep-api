//! What `main` does, in order: load `.env.local`, dispatch a subcommand if
//! one was given, otherwise install logging, build the shared state,
//! start the background tasks and serve. Each step lives in its own
//! module so `main` reads as the list of steps and nothing else.

mod cli;
mod config;
mod logging;
mod serve;
mod state;
mod tasks;

pub(crate) async fn run() {
    // Loaded first, before anything else reads an env var -- a missing
    // file is fine (a deployed environment injects real env vars
    // directly instead), but a parse failure is worth a visible warning
    // rather than silently ignoring whatever did parse.
    match dotenvy::from_filename(".env.local") {
        Ok(_) => {}
        Err(dotenvy::Error::Io(_)) => {}
        Err(err) => {
            eprintln!("Warning: failed to parse .env.local: {err}");
        }
    }

    if let cli::Dispatch::Handled = cli::dispatch().await {
        return;
    }

    logging::init();

    let (state, stores) = state::build().await;
    tasks::spawn(&state, &stores);
    serve::run(state).await;
}
