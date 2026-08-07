//! The client-ops domain: tooling an Onboarding Manager (or anyone else
//! holding a client-ops-adjacent role) uses day to day, as distinct from
//! `auth`, which is identity/authorization. Lives in its own `client_ops`
//! Postgres schema for the same reason -- see migration
//! `20260808120000_create_client_ops_schema_and_qms_tag`.
//!
//! `qms_tag` (the reference catalog itself) is plain data accessed
//! directly from `api::client_ops_qms_tags` via `sqlx`, the same way
//! `api::auth_roles` reads `auth.roles` -- no repository layer for a
//! table this small. `audit_log` is the one piece of behavior this module
//! owns: recording client-ops mutations, kept separate from
//! `auth::audit_log` on purpose (see that module's own doc comment).

pub mod audit_log;
