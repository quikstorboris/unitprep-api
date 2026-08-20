//! The client-ops domain: tooling an Onboarding Manager (or anyone else
//! holding a client-ops-adjacent role) uses day to day, as distinct from
//! `auth`, which is identity/authorization. Lives in its own `client_ops`
//! Postgres schema for the same reason -- see migration
//! `20260808120000_create_client_ops_schema_and_qms_tag`.
//!
//! `qms_tag` (the reference catalog itself) is plain data accessed
//! directly from `api::client_ops_qms_tags` via `sqlx`, the same way
//! `api::auth_roles` reads `auth.roles` -- no repository layer for a
//! table this small. `audit_log` and `vendor_format` are the two pieces
//! of behavior this module owns: `audit_log` records client-ops
//! mutations, kept separate from `auth::audit_log` on purpose (see that
//! module's own doc comment); `vendor_format` loads the
//! `client_ops.vendor_format` registry (Group Prep's and dedup's shared,
//! DB-backed replacement for what used to be per-tool hardcoded vendor
//! consts) into `unitprep_core::vendor_format::VendorFormat` -- a real
//! repository module, unlike `qms_tag`, because two unrelated tools read
//! it rather than one.

pub mod audit_log;
pub mod vendor_format;
