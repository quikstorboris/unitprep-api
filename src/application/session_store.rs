use std::sync::{Arc, RwLock};

use serde::Serialize;

use crate::domain::session::Session;

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
/// FUTURE: RedisSessionStore will implement this trait exactly as
/// InMemorySessionStore does. Swapping storage backends will require zero
/// changes to any business logic.
pub trait SessionStore: Send + Sync {
    fn save(&self, session: Session);

    fn get_handle(
        &self,
        id: &str,
    ) -> Option<Arc<RwLock<Session>>>;

    fn delete(&self, id: &str);

    fn metrics(&self) -> SessionMetrics;
}

pub trait SessionStoreExt: SessionStore {
    fn with_session<R>(
        &self,
        id: &str,
        operation: impl FnOnce(&Session) -> R,
    ) -> Option<R> {
        let handle =
            self.get_handle(id)?;

        let session =
            handle
                .read()
                .expect(
                    "Session read lock poisoned",
                );

        Some(operation(&session))
    }

    fn with_session_mut<R>(
        &self,
        id: &str,
        operation: impl FnOnce(&mut Session) -> R,
    ) -> Option<R> {
        let handle =
            self.get_handle(id)?;

        let mut session =
            handle
                .write()
                .expect(
                    "Session write lock poisoned",
                );

        Some(operation(
            &mut session,
        ))
    }
}

impl<T> SessionStoreExt for T
where
    T: SessionStore + ?Sized,
{
}
