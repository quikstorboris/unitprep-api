use std::time::SystemTime;

use uuid::Uuid;

/// The part of a session every UnitPrep tool needs, and the only part the
/// shared storage engine (`SessionStore`) ever looks at: an id, two
/// timestamps, and who created it. Tool-specific state (stages, parsed
/// documents, analysis results, etc.) lives entirely outside this struct,
/// in each tool's own session type.
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: SystemTime,
    pub last_accessed: SystemTime,

    /// The authenticated caller that created this session, if any. Always
    /// `None` today -- no session-creating endpoint has an authenticated
    /// caller to attribute it to yet -- but threaded through now so that
    /// wiring auth in later is "pass `Some(user.id)` instead of `None`" at
    /// the two HTTP call sites, not a data-model change made under time
    /// pressure. See `SessionStoreExt::with_owned_session` for where this
    /// gets enforced.
    pub owner_id: Option<Uuid>,

    /// Set once, under this session's own write lock, by `/session/cancel`
    /// just before it removes the session's map entry. Exists so that gap
    /// between "cancel decided to remove this session" and "the map entry
    /// is actually gone" has real synchronization behind it: a concurrent
    /// handler racing for the same session's write lock (e.g. via
    /// `with_session_mut`) either completes its mutation before this flag
    /// is set (lock acquired first -- its write is preserved right up
    /// until cancellation, same as if cancel had simply happened a moment
    /// later) or observes `cancelled == true` after acquiring the lock and
    /// is turned away exactly like a nonexistent session (lock acquired
    /// after) -- never a silent write to an object already detached from
    /// the map. See `SessionStoreExt`'s default methods, which gate on
    /// this the same way they gate on an `owner_id` mismatch.
    pub cancelled: bool,
}

impl SessionMetadata {
    pub fn new(id: String, owner_id: Option<Uuid>) -> Self {
        let now = SystemTime::now();

        Self {
            id,
            created_at: now,
            last_accessed: now,
            owner_id,
            cancelled: false,
        }
    }
}

/// Anything a tool wants managed by the shared `SessionStore` engine must
/// implement this — it's the entire contract between a tool's own session
/// type and the storage engine. The engine never needs anything else
/// about a session, so this is deliberately the only requirement.
pub trait HasSessionMetadata {
    fn metadata(&self) -> &SessionMetadata;
    fn metadata_mut(&mut self) -> &mut SessionMetadata;
}
