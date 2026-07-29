-- Closes a privilege-escalation gap: app_service held TABLE-level UPDATE
-- on auth.users, which implicitly covers every column, including `role`.
--
-- The users_update_own_or_admin policy is row-scoped, not column-scoped:
-- it correctly permits a caller to update their OWN row, but nothing
-- stopped that update from being `SET role = 'admin'`. RLS cannot express
-- "this row, but not this column" -- column-level privileges can, and
-- they are enforced in addition to RLS, so this is the right layer for
-- the constraint rather than relying on application discipline alone.
--
-- Not currently exploitable (every existing user is already role=admin,
-- and no application code UPDATEs auth.users at all -- verified by grep
-- 2026-07-29), which is precisely why it is cheap to close now instead of
-- after the other roles exist and something depends on the loose grant.

REVOKE UPDATE ON auth.users FROM app_service;

-- Only what a self-service "edit my profile" endpoint would legitimately
-- need.
--
-- Everything else is an administrative act and must go through a
-- SECURITY DEFINER function that checks the caller is an admin -- the
-- same pattern create_session/resolve_session/consume_invite already
-- use. Specifically withheld:
--
--   role                          the escalation vector this migration exists for
--   status                        a user must not be able to reactivate themselves
--   deleted_at, deletion_reason   nor undo their own soft-delete
--   company                       organizational placement, not self-service
--   email                         the login identifier, and there is no
--                                 verification flow for changing it
--   id, created_at, updated_at    never application-writable
--
-- Note that `updated_at` needs no grant despite the users_set_updated_at
-- BEFORE UPDATE trigger writing to it: column-level UPDATE privilege is
-- checked against the columns named in the statement's SET list, not
-- against columns a trigger modifies on NEW. Verified empirically on the
-- dev branch as app_service, not assumed -- the trigger still bumps
-- updated_at on a first_name-only update.
GRANT UPDATE (first_name, last_name, job_title) ON auth.users TO app_service;
