//! Subcommand dispatch, before any server setup. `bootstrap-admin` is a
//! one-shot administrative command that must not start a listener, open
//! the application pool, or need a WebAuthn configuration -- see
//! src/bootstrap.rs for why it is a subcommand rather than an endpoint.
//!
//! Deliberately a plain argv check rather than an argument-parsing
//! dependency: there are two subcommands, and everything else is
//! "serve", which takes no arguments at all.

use crate::{bootstrap, reencrypt_sources};

/// What `main` should do after looking at argv.
pub(super) enum Dispatch {
    /// No subcommand: start the server.
    Serve,
    /// A subcommand ran (or help was printed); the process is done.
    Handled,
}

pub(super) async fn dispatch() -> Dispatch {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(first) = argv.first() else {
        return Dispatch::Serve;
    };

    match first.as_str() {
        "bootstrap-admin" => {
            run_bootstrap(&argv[1..]).await;
            Dispatch::Handled
        }
        "reencrypt-tool-run-sources" => {
            match reencrypt_sources::run().await {
                Ok(message) => println!("{message}"),
                Err(message) => {
                    eprintln!("error: {message}");
                    std::process::exit(1);
                }
            }
            Dispatch::Handled
        }
        "--help" | "-h" | "help" => {
            println!(
                "{}
{}",
                bootstrap::USAGE,
                reencrypt_sources::USAGE_LINE
            );
            Dispatch::Handled
        }
        other => {
            eprintln!("unknown subcommand {other:?}\n\n{}", bootstrap::USAGE);
            std::process::exit(2);
        }
    }
}

/// Runs the `bootstrap-admin` subcommand and exits with a status the shell
/// can branch on -- 2 for a bad invocation, 1 for a refusal or failure,
/// 0 on success. Kept out of `main` so the serve path stays one flow.
async fn run_bootstrap(argv: &[String]) {
    let args = match bootstrap::parse_args(argv) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}\n\n{}", bootstrap::USAGE);
            std::process::exit(2);
        }
    };

    match bootstrap::run(args).await {
        Ok(message) => println!("{message}"),
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    }
}
