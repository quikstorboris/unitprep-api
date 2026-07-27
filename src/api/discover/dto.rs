//! Request/response shapes for `/discover` (and, indirectly, every other
//! endpoint that shares `compute_discovery`'s response — see `mod.rs`).

use serde::{Deserialize, Serialize};

use unitprep_unit_group::{FieldMappingEntry, UnitFileCandidate};

#[derive(Debug, Deserialize)]
pub struct DiscoverRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct DiscoverResponse {
    pub unit_files_found: usize,
    pub group_files_found: usize,
    pub group_file_names: Vec<String>,
    pub selected_group_file_name:
        Option<String>,
    /// Whether the currently selected group file actually looks like a
    /// real master group file (the same `name`/`description`/
    /// `assignedto`/`status`/`lastupdated` header check discovery itself
    /// uses to classify one automatically) — meaningful mainly for a
    /// manually-uploaded override (see `/group-file/upload`), which
    /// bypasses that classification on purpose. `None` until a group
    /// file is selected at all.
    pub group_file_format_valid:
        Option<bool>,
    /// Explicit "yes, this is the right file" confirmation — see
    /// `/group-file/confirm`. Selecting a file (auto-detected or
    /// manual) is not enough on its own; `ready` requires this too.
    pub group_file_confirmed: bool,
    pub ready: bool,
    /// Distinct UnitGroup values found across the discovered unit
    /// files, sorted for stable display. Recomputed on every call
    /// rather than stored on `DiscoveryResult` — nothing downstream in
    /// the pipeline consumes it, it exists purely so the UI can show
    /// the user what it found before they commit to validate/export
    /// (most useful exactly when there's no master file to cross-check
    /// against yet). Empty until the selected unit file's format has
    /// been resolved (see `requires_format_resolution`) — a file whose
    /// vendor headers haven't been mapped to canonical columns yet has
    /// no `UnitGroup` column for this to read.
    pub discovered_group_names:
        Vec<String>,
    /// The subset of `discovered_group_names` that don't look like a real
    /// storage-unit group name (no parseable width/length dimension, or a
    /// degenerate 0x0) — a review hint, shown separately so it's easy to
    /// notice, never used to change matching/analysis behavior.
    pub uncommon_group_names:
        Vec<String>,

    /// Every discovered file matching a known vendor's header signature
    /// (QSX, DoorSwap, ...) — the checkbox list the frontend lets the
    /// user confirm a subset (or all) of.
    pub unit_file_candidates: Vec<UnitFileCandidate>,
    /// The confirmed set to actually process — same as `unit_file_names`,
    /// exposed under this name too since it's the one the unit-file
    /// selection/confirmation UI cares about.
    pub selected_unit_file_names: Vec<String>,
    /// More than one candidate and nothing confirmed yet — the frontend
    /// should show the checkbox picker (see `/unit-file/select`) before
    /// anything else.
    pub requires_unit_file_selection: bool,
    /// At least one confirmed file's vendor format hasn't been confirmed
    /// or manually mapped yet — the frontend should show the confirm/map
    /// screen (see `/unit-file/resolve-format`) for `current_unit_file_name`.
    pub requires_format_resolution: bool,
    /// Which confirmed file the confirm/map screen is currently working
    /// on — the first (by name) still missing a resolution. `None` once
    /// every confirmed file is resolved.
    pub current_unit_file_name: Option<String>,
    /// Every confirmed file still awaiting resolution, in the order
    /// they'll be resolved (`current_unit_file_name` is always this
    /// list's first entry) — lets the UI show progress without
    /// re-deriving it.
    pub pending_unit_file_names: Vec<String>,
    /// Confirmed files whose headers don't match the majority shape among
    /// the rest of the confirmed set — a safeguard against the
    /// supposedly-impossible case of the checkbox selection spanning more
    /// than one vendor/shape at once (see "Confirm {vendor}" bulk
    /// resolution below). Empty when everything's consistent, which
    /// should be every real case; non-empty blocks bulk confirmation
    /// until the user returns to Unit Files Selection and fixes it.
    pub mismatched_header_files: Vec<String>,
    pub detected_vendor_name: Option<String>,
    /// The vendor confirmed for the confirmed unit files, once every one
    /// of them is resolved (`detected_vendor_name` goes back to `None`
    /// at that point, since there's no longer a "current" pending file
    /// for it to describe) — derived by re-running vendor detection
    /// against any one of the selected documents, purely for display.
    /// `None` if formats aren't all resolved yet, or none of them
    /// matched a known vendor (all manually mapped).
    pub confirmed_vendor_name: Option<String>,
    /// `current_unit_file_name`'s own headers — only populated while
    /// `requires_format_resolution` is true, for building the manual
    /// mapping UI's per-target dropdowns.
    pub source_headers: Vec<String>,
    /// The detected vendor's preset mapping, to pre-fill the manual
    /// mapping UI (still fully overridable).
    pub suggested_mapping: Vec<FieldMappingEntry>,
    /// Static, session-independent: the full set of target fields the
    /// manual mapping UI's left column should list, and which of those
    /// are required. Same on every response — included here so the
    /// frontend never has to hard-code its own copy.
    pub canonical_target_fields: Vec<String>,
    pub required_target_fields: Vec<String>,
}
