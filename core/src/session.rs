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
}

impl SessionMetadata {
    pub fn new(id: String, owner_id: Option<Uuid>) -> Self {
        let now = SystemTime::now();

        Self {
            id,
            created_at: now,
            last_accessed: now,
            owner_id,
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
