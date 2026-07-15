use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;

use crate::session::HasSessionMetadata;

#[derive(Debug, Clone, Serialize)]
pub struct SessionMetrics {
    pub active_sessions: usize,
    pub created_sessions: u64,
    pub deleted_sessions: u64,
    pub expired_sessions: u64,
}

/// SessionStore is the single abstraction through which all session access flows.
///
/// NO code outside of this module should interact with session storage directly.
///
/// Generic over the session type `S` — the store only ever needs `S` to
/// expose its metadata (see `HasSessionMetadata`); it has no idea what
/// tool-specific data `S` carries otherwise. Each tool instantiates its own
/// store with its own concrete session type; two tools' stores never share
/// data even if both happen to use `InMemorySessionStore`.
///
/// FUTURE: RedisSessionStore will implement this trait exactly as
/// InMemorySessionStore does. Swapping storage backends will require zero
/// changes to any business logic.
///
/// ## Locking invariants (binding on every implementation)
///
/// Session locks are `parking_lot::RwLock`, not `std::sync::RwLock` — a
/// panic while holding one releases it instead of poisoning it, so a bug in
/// one request can never permanently wedge session access for every other
/// request. That removes lock poisoning as a failure mode, but the
/// ordering rule below still matters for deadlock avoidance, independent of
/// poisoning:
///
/// Implementations that use a lock per session plus a lock over the
/// collection of sessions (as `InMemorySessionStore` does) MUST follow this
/// order everywhere both are held at once:
///
/// 1. Acquire the outer collection lock first.
/// 2. Acquire an individual session's lock only while still holding (or
///    already having released) the outer lock in that same order — never
///    acquire the outer lock while already holding a session lock.
/// 3. Never hold a session lock across an `.await` point. Session
///    operations must stay synchronous and short-lived; if a caller needs
///    to do async work using session data, it must read/clone what it
///    needs, drop the lock, then await.
///
/// Violating either rule risks a real deadlock once more than one endpoint
/// touches session state concurrently (e.g. two handlers locking two
/// sessions in different orders), not just lock contention.
pub trait SessionStore<S: HasSessionMetadata>: Send + Sync {
    fn save(&self, session: S);

    fn get_handle(
        &self,
        id: &str,
    ) -> Option<Arc<RwLock<S>>>;

    fn delete(&self, id: &str);

    fn metrics(&self) -> SessionMetrics;
}

pub trait SessionStoreExt<S: HasSessionMetadata>:
    SessionStore<S>
{
    fn with_session<R>(
        &self,
        id: &str,
        operation: impl FnOnce(&S) -> R,
    ) -> Option<R> {
        let handle =
            self.get_handle(id)?;

        let session = handle.read();

        Some(operation(&session))
    }

    fn with_session_mut<R>(
        &self,
        id: &str,
        operation: impl FnOnce(&mut S) -> R,
    ) -> Option<R> {
        let handle =
            self.get_handle(id)?;

        let mut session = handle.write();

        Some(operation(
            &mut session,
        ))
    }
}

impl<S, T> SessionStoreExt<S> for T
where
    S: HasSessionMetadata,
    T: SessionStore<S> + ?Sized,
{
}
