use std::sync::Arc;

use unitprep_core::session_store::SessionStore;

use crate::application::dedup_session_service::DedupSession;
use crate::application::tagger_session_service::TaggerSession;
use crate::application::unit_group_session::Session;
use crate::client_ops::vendor_format::VendorFormatCache;
use crate::clients::sync::SyncProgressHandle;
use crate::dropbox::DropboxClient;
use crate::process_street::ProcessStreetClient;

#[derive(Clone)]
pub struct AppState {
    // Named for the tool it serves, not just "the store" — UnitPrep is
    // moving toward multiple tools each with their own session type and
    // their own store instance (see unitprep-core's generic
    // SessionStore<S>); this field will get company (e.g.
    // `dedup_sessions`) rather than being renamed later under pressure.
    pub unit_group_sessions: Arc<dyn SessionStore<Session>>,

    // Additive, per the comment above — a second tool's store, not a
    // rename of the first.
    pub dedup_sessions: Arc<dyn SessionStore<DedupSession>>,

    // Third tool's store, same pattern as the two above — the QMS
    // Template Tagging Assistant.
    pub tagger_sessions: Arc<dyn SessionStore<TaggerSession>>,

    // The app_service-authenticated connection pool -- see db.rs for
    // why it is built lazily rather than blocking startup on Postgres
    // being reachable.
    pub db: sqlx::PgPool,

    // See auth/mod.rs for the AuthBackend trait -- Arc<dyn ...>, same
    // pattern as the session stores above, so a future backend swap is
    // a new impl, not a rewrite of every call site.
    pub auth_backend: Arc<dyn crate::auth::AuthBackend>,

    // Ephemeral WebAuthn registration-ceremony state (see
    // auth::RegistrationCeremony) -- same generic SessionStore engine as
    // unit_group_sessions/dedup_sessions above, just a much shorter
    // timeout, since a ceremony is one request/response round trip, not
    // a standing session.
    pub registration_ceremonies: Arc<dyn SessionStore<crate::auth::RegistrationCeremony>>,

    // Login's counterpart to registration_ceremonies. A separate store,
    // not a shared one: the two hold different webauthn-rs state types and
    // can be in flight simultaneously (see the ceremony-cookie names in
    // auth/ceremony_cookie.rs for the same reasoning).
    pub authentication_ceremonies: Arc<dyn SessionStore<crate::auth::AuthenticationCeremony>>,

    // Group Prep's recognized unit-file vendor registry (`client_ops.
    // vendor_format` where content_type = 'units') -- an in-memory
    // snapshot refreshed on a timer by
    // `client_ops::vendor_format::start_refresh_task`, not queried per
    // request. See that module's doc comment for why: discovery's own
    // handlers are called directly by a large existing test suite
    // against a pool that never connects, and those tests must stay
    // DB-free.
    pub unit_vendors: VendorFormatCache,

    // Dedup's own vendor registry (content_type = 'tenants'), same
    // caching reasoning as `unit_vendors` above -- a distinct field
    // rather than one cache keyed by content type, matching how
    // `unit_group_sessions`/`dedup_sessions` above are already separate
    // fields per tool rather than one map.
    pub tenant_vendors: VendorFormatCache,

    // Dropbox access for the QMS Onboarding folder (see src/dropbox for
    // the full scope/namespace caveats). A concrete client, not
    // Arc<dyn Trait> like auth_backend above -- there is one real
    // implementation and no swap point today.
    pub dropbox: Arc<DropboxClient>,

    // `None` when PROCESS_STREET_API_KEY isn't configured -- unlike
    // dropbox/auth above, this integration is still partial (Contract
    // Order on hold, no frontend yet) and must not block startup the
    // way a missing WebAuthn/Dropbox config does. Handlers that need it
    // return a clear error rather than panicking or silently no-op-ing.
    pub process_street: Option<Arc<ProcessStreetClient>>,

    // Shared between the nightly background sync and the manual "Sync
    // Now" endpoint (`api::clients_sync`) -- see `SyncProgressHandle`'s
    // own doc comment for why one shared handle is the mutual-exclusion
    // guard, not just a progress readout. Constructed unconditionally
    // (unlike `process_street` above) since it starts as a harmless
    // `Idle` value even when PS isn't configured -- only the endpoints
    // that read/act on it need to also check `process_street`.
    pub sync_progress: SyncProgressHandle,
}
