mod ai;
mod api;
mod application;
mod auth;
mod blocking;
mod bootstrap;
mod clickup;
mod client_ops;
mod clients;
mod db;
mod dropbox;
mod infrastructure;
mod integrations;
mod process_street;
mod reencrypt_sources;
mod startup;

#[tokio::main]
async fn main() {
    startup::run().await;
}
