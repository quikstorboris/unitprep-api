//! Builds the shared [`AppState`]: the database pool, the durable session
//! stores, the integration clients and the vendor-format snapshots. Pure
//! construction -- nothing here spawns a background task; `tasks` does
//! that from the finished state, so what runs in the background is
//! readable in one place.

use std::sync::Arc;

use unitprep_core::durable_session_store::DurableSessionStore;
use unitprep_core::vendor_format::ContentType;

use crate::api::AppState;
use crate::application::dedup_session_service::DedupSession;
use crate::application::tagger_session_service::TaggerSession;
use crate::application::unit_group_session::Session;
use crate::{auth, client_ops, clients, db, dropbox, integrations, process_street};

use super::config;

/// The concrete durable stores behind the trait objects `AppState` holds.
/// `AppState` erases their type, which hides `start_cleanup_task`, so the
/// builder hands the concrete handles back for `tasks::spawn` to start.
pub(super) struct DurableStores {
    unit_group: Arc<DurableSessionStore<Session>>,
    dedup: Arc<DurableSessionStore<DedupSession>>,
    tagger: Arc<DurableSessionStore<TaggerSession>>,
    registration: Arc<DurableSessionStore<auth::RegistrationCeremony>>,
    authentication: Arc<DurableSessionStore<auth::AuthenticationCeremony>>,
}

impl DurableStores {
    /// Starts each store's idle-expiry sweep.
    pub(super) fn start_cleanup_tasks(&self) {
        self.unit_group.start_cleanup_task();
        self.dedup.start_cleanup_task();
        self.tagger.start_cleanup_task();
        self.registration.start_cleanup_task();
        self.authentication.start_cleanup_task();
    }
}

pub(super) async fn build() -> (AppState, DurableStores) {
    let session_timeout = config::session_timeout();

    // See db.rs -- deliberately non-blocking (connect_lazy), since most
    // existing endpoints do not touch Postgres at all yet. Constructed
    // first because every session store below needs a pool handle to
    // persist through.
    let db_pool = db::connect().unwrap_or_else(|err| {
        panic!("Failed to configure the database pool: {err}");
    });

    // Durable (2026-09-24), not plain InMemorySessionStore -- same
    // reasoning as the WebAuthn ceremony stores below: a restart or
    // crash mid-upload used to silently strand a Group Prep session,
    // with no way to recover it short of starting over. Cloning
    // db_pool here is cheap -- sqlx::PgPool is an Arc-backed handle to
    // the same underlying pool `state.db` gets below, not a second
    // pool. The dedup and tagger stores share the timeout policy and the
    // durability reasoning -- no reason for the tools' sessions to expire
    // (or survive a restart) on different schedules today.
    let unit_group_sessions = Arc::new(DurableSessionStore::<Session>::with_timeout(
        db_pool.clone(),
        "unit_group_session",
        session_timeout,
    ));
    let dedup_sessions = Arc::new(DurableSessionStore::<DedupSession>::with_timeout(
        db_pool.clone(),
        "dedup_session",
        session_timeout,
    ));
    let tagger_sessions = Arc::new(DurableSessionStore::<TaggerSession>::with_timeout(
        db_pool.clone(),
        "tagger_session",
        session_timeout,
    ));

    // The five independent startup reads (both integration configs and the
    // three vendor-format snapshots) are issued TOGETHER here rather than one
    // after another: against a remote or just-woken Neon each is at least a
    // network round trip (and the config reads decrypt), so serially they
    // were the bulk of boot time.
    let (dropbox_from_db, process_street_from_db, unit_vendors, tenant_vendors, tenant_file_meta) = tokio::join!(
        dropbox::DropboxConfig::from_db(&db_pool),
        process_street::ProcessStreetConfig::from_db(&db_pool),
        client_ops::vendor_format::initial_cache(&db_pool, ContentType::Units),
        client_ops::vendor_format::initial_cache(&db_pool, ContentType::Tenants),
        client_ops::vendor_file_meta::initial_cache(&db_pool, ContentType::Tenants),
    );

    let dropbox_client = Arc::new(dropbox::DropboxClient::new(resolve_dropbox_config(
        dropbox_from_db,
    )));
    let process_street_client = resolve_process_street_client(process_street_from_db);

    // See `integrations::env_source`'s own doc comment.
    let env_source: Arc<dyn integrations::env_source::EnvSource> =
        Arc::new(integrations::env_source::ProcessEnvSource);

    // Constructed unconditionally (starts as a harmless Idle value) so
    // `api::clients_sync`'s manual "Sync Now" endpoint always has
    // something to read even when PS isn't configured -- it checks
    // `process_street` separately before acting on it.
    let sync_progress = Arc::new(parking_lot::RwLock::new(
        clients::sync::SyncProgress::default(),
    ));

    let (auth_backend, registration_ceremonies, authentication_ceremonies) = build_auth(&db_pool);

    let stores = DurableStores {
        unit_group: unit_group_sessions.clone(),
        dedup: dedup_sessions.clone(),
        tagger: tagger_sessions.clone(),
        registration: registration_ceremonies.clone(),
        authentication: authentication_ceremonies.clone(),
    };

    let state = AppState {
        unit_group_sessions,
        dedup_sessions,
        tagger_sessions,
        db: db_pool,
        auth_backend,
        registration_ceremonies,
        authentication_ceremonies,
        unit_vendors,
        tenant_vendors,
        tenant_file_meta,
        dropbox: dropbox_client,
        process_street: process_street_client,
        sync_progress,
        resync_preview_cache: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
        env_source,
    };

    (state, stores)
}

/// See src/dropbox for the full scope/namespace caveats (Full Dropbox
/// access, app-level-only path enforcement, Team Space namespace).
///
/// DB-first, env-fallback (2026-09-09): `integrations.dropbox_configuration`
/// is the admin settings page's source of truth (see
/// `api::dropbox_settings`); `DROPBOX_*` env vars remain the fallback
/// for a deployment that hasn't configured it there yet, or hit before
/// that migration has run. A saved change on the settings page takes
/// effect on the next server start -- nothing re-reads this mid-run,
/// unlike Process Street's sync interval (see `clients::sync`).
fn resolve_dropbox_config<E: std::fmt::Display>(
    from_db: Result<Option<dropbox::DropboxConfig>, E>,
) -> dropbox::DropboxConfig {
    match from_db {
        Ok(Some(config)) => config,
        Ok(None) => dropbox::DropboxConfig::from_env().unwrap_or_else(|err| {
            panic!("Dropbox is not configured in the database or the environment: {err}");
        }),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "Could not read Dropbox configuration from the database, falling back to environment variables"
            );
            dropbox::DropboxConfig::from_env().unwrap_or_else(|env_err| {
                panic!("Failed to configure Dropbox: {env_err}");
            })
        }
    }
}

/// DB-first, env-fallback (2026-09-09) -- same reasoning as Dropbox
/// above. Unlike Dropbox/WebAuthn, a missing key here must still not
/// block startup -- this integration is still partial (Contract
/// Order on hold, no frontend yet), and every environment that
/// doesn't need it (most tests, a fresh dev checkout) shouldn't have
/// to configure a real key just to run the server. Endpoints that
/// need it return a clear error instead of silently no-op-ing; see
/// `api::clients_search`.
fn resolve_process_street_client<E: std::fmt::Display>(
    from_db: Result<Option<process_street::ProcessStreetConfig>, E>,
) -> Option<Arc<process_street::ProcessStreetClient>> {
    let config = match from_db {
        Ok(Some(config)) => Some(config),
        Ok(None) => process_street::ProcessStreetConfig::from_env().ok(),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "Could not read Process Street configuration from the database, falling back to environment variables"
            );
            process_street::ProcessStreetConfig::from_env().ok()
        }
    };

    match config {
        Some(config) => Some(Arc::new(process_street::ProcessStreetClient::new(config))),
        None => {
            tracing::warn!(
                "Process Street not configured -- PS-backed search/import endpoints will return an error until it's configured (Integrations > Process Street, or PROCESS_STREET_API_KEY)"
            );
            None
        }
    }
}

/// The WebAuthn backend and its two ceremony stores.
///
/// Fatal, not a warning, if a non-localhost origin would serve session
/// cookies without the Secure attribute: every session token would travel
/// in plaintext over the network. SESSION_COOKIE_SECURE=false is a
/// legitimate local-HTTP-dev escape hatch, so it must not be able to
/// reach a real deployment silently -- see `auth::validate_cookie_security`.
///
/// The ceremony stores are durable (2026-09-24), not plain
/// InMemorySessionStore -- a deploy landing in the ~5-minute window
/// between /begin and /finish used to silently strand the browser with an
/// inexplicable "ceremony expired" error, since the in-memory state
/// backing it vanished with the old process. DurableSessionStore
/// write-through-persists to `auth.durable_sessions` (see that migration
/// and unitprep_core::durable_session_store's own doc comments) while
/// keeping the exact same hot-path behavior for the overwhelmingly common
/// case where the process never restarts mid-ceremony. The two stores use
/// their own `kind` discriminator -- see auth.durable_sessions's own doc
/// comment for why (kind, id), not id alone, is that table's primary key.
#[allow(clippy::type_complexity)]
fn build_auth(
    db_pool: &sqlx::PgPool,
) -> (
    Arc<dyn auth::AuthBackend>,
    Arc<DurableSessionStore<auth::RegistrationCeremony>>,
    Arc<DurableSessionStore<auth::AuthenticationCeremony>>,
) {
    let webauthn = config::webauthn_settings();

    if let Err(message) = auth::validate_cookie_security(&webauthn.rp_origin) {
        panic!("{message}");
    }

    let auth_backend: Arc<dyn auth::AuthBackend> = Arc::new(
        auth::WebauthnRsBackend::new(&webauthn.rp_id, &webauthn.rp_origin).unwrap_or_else(|err| {
            panic!("Failed to configure the WebAuthn backend: {err}");
        }),
    );

    let registration_ceremonies = Arc::new(DurableSessionStore::with_timeout(
        db_pool.clone(),
        "webauthn_registration_ceremony",
        config::CEREMONY_TIMEOUT,
    ));
    let authentication_ceremonies = Arc::new(DurableSessionStore::with_timeout(
        db_pool.clone(),
        "webauthn_authentication_ceremony",
        config::CEREMONY_TIMEOUT,
    ));

    (
        auth_backend,
        registration_ceremonies,
        authentication_ceremonies,
    )
}
