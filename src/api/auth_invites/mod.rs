//! Invitation creation, admin-only (Phase 2 task 7). The other half of the
//! invitation flow: task 6 made a token redeemable, this makes one
//! issuable by an administrator rather than only by the
//! `unitprep bootstrap-admin` CLI.
//!
//! ## Why this needs no SECURITY DEFINER function
//!
//! Both writes are permitted to `app_service` directly, under RLS policies
//! that check the identity GUCs:
//!
//! - `users_insert_admin_only` — `WITH CHECK (auth.current_user_has_role('admin'))`
//! - `user_invites_admin_only` — `FOR ALL` under the same condition
//!
//! So running inside `begin_rls_transaction(.., &admin.role_keys)` means the
//! **database** enforces admin-ness independently of this handler's own
//! check. Both exist on purpose: the handler's check produces a clean 403,
//! and the policy is what holds if a future refactor forgets it.
//!
//! `bootstrap-admin` needs the owner connection instead, for a reason that
//! does not apply here: at bootstrap time no administrator exists yet, so
//! there is no identity to put in the GUC and nothing for the policy to
//! approve. Creating the *first* user is genuinely a different problem from
//! creating the second.
//!
//! `user_invites.created_by` is left to its column default, which reads
//! `app.current_user_id` — so inside this transaction it records the issuing
//! admin by itself. That default was written for exactly this call site;
//! binding it explicitly would be duplicating the schema's own answer.
//!
//! ## Role validity now costs a database round trip
//!
//! A role key can no longer be checked against a closed Rust enum before
//! opening a transaction -- roles are real data (`auth.roles`), open-ended
//! rather than a fixed pair, so the only source of truth for "is this a
//! real role" is the table itself. `issue_invite` resolves it via
//! `resolve_role_id` right after opening the RLS transaction, and rolls
//! back cleanly on an unknown key rather than attempting the write.
//!
//! ## No email is sent
//!
//! There is no ESP integration yet, so the response returns the raw token
//! **once** and delivering it is the admin's problem. That is a deliberate
//! staging point, not an oversight: notify-on-enrolment was chosen over a
//! blocking approval step, and both wait on an ESP existing.

mod invite;
mod recovery;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub use invite::CreateInviteRequest;
pub use invite::{create_invite, CreateInviteResponse};
pub use recovery::recover_account;
#[cfg(test)]
pub use recovery::RecoverAccountRequest;
