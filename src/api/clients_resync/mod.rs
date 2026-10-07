//! Scoped manual "Re-sync" for one already-imported client -- lets a
//! manager re-pull that company's own source run plus every one of its
//! facilities' own runs from Process Street right now, without waiting
//! for the next scheduled interval (see `clients::sync`'s own module
//! doc on why that interval can now be much shorter than the old
//! once-daily default, but still isn't "immediately").
//!
//! Two-phase, matching the hybrid design Boris asked for (2026-09-02):
//! a field that's been manually corrected in OO (`manually_edited_fields`,
//! same mechanism the scheduled sync silently respects) never gets
//! silently overwritten here either -- but unlike the scheduled sync,
//! a human is actually watching this one, so a real conflict (the field
//! is protected AND Process Street's current value genuinely differs)
//! is surfaced for an explicit per-field choice instead of always just
//! skipping it.
//!
//! `preview_resync` reports what would happen without writing anything;
//! `apply_resync` takes the caller's own resolutions for whichever
//! conflicts they want to overwrite from Process Street (unlisted or
//! `use_fresh: false` conflicts keep the manually-set value, same as the
//! scheduled sync's own default) and writes.
//!
//! **`apply_resync` reuses `preview_resync`'s own fetch, not a second
//! one** (2026-09-23, against Boris's own observation that confirming a
//! preview -- even choosing to keep every field as-is -- still took as
//! long as the preview itself). Both phases were independently calling
//! `load_comparisons`, which does the expensive part twice: every
//! linked run's fields *and* tasks, fetched live from Process Street,
//! for the company plus every one of its facilities. `preview_resync`
//! now stashes its own `load_comparisons` result in
//! `AppState::resync_preview_cache`, keyed by `company_id`; `apply_resync`
//! drains that entry (single-use -- a second apply without a fresh
//! preview falls through to fetching live again, same as a cache miss)
//! when it's still within `PREVIEW_CACHE_TTL`, and only calls
//! `load_comparisons` itself when there's nothing usable there. A
//! missing or stale entry is always a safe fallback to today's
//! behavior, never a correctness risk -- this is purely cutting a
//! redundant round trip to Process Street (and the PS API-rate-limit
//! cost that comes with it) out of the common path.

mod apply;
mod compare;
mod fetch;
mod preview;
mod rows;
mod write;

pub use apply::apply_resync;
#[cfg(test)]
pub use apply::ApplyResyncRequest;
pub use compare::ResyncPreviewCache;
pub use preview::preview_resync;

pub(super) const PERMISSION: &str = "client_ops.perform";

#[cfg(test)]
mod tests;
