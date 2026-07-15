use std::time::SystemTime;

/// The part of a session every UnitPrep tool needs, and the only part the
/// shared storage engine (`SessionStore`) ever looks at: an id and two
/// timestamps. Tool-specific state (stages, parsed documents, analysis
/// results, etc.) lives entirely outside this struct, in each tool's own
/// session type.
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: SystemTime,
    pub last_accessed: SystemTime,
}

impl SessionMetadata {
    pub fn new(id: String) -> Self {
        let now = SystemTime::now();

        Self {
            id,
            created_at: now,
            last_accessed: now,
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
