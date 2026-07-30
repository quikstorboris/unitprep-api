-- Makes two foreign keys declare what they actually do.
--
-- auth_audit_logs.actor_user_id and .target_user_id were ON DELETE SET
-- NULL, which reads as "deleting a user nulls the reference". That action
-- can never fire: nulling the column is an UPDATE, and the append-only
-- triggers on this table forbid UPDATE unconditionally -- for every role,
-- including the table owner. So deleting a user who has any audit history
-- fails with:
--
--   ERROR: auth_audit_logs is append-only: UPDATE not permitted
--
-- which points at the wrong thing entirely. The foreign key looked
-- satisfiable and the trigger looked like the problem, when in fact the
-- pair of them mean "a user with audit history cannot be hard-deleted".
--
-- RESTRICT states that directly, and fails with a foreign-key error naming
-- the referencing table.
--
-- The alternative -- loosening the trigger so FK-driven nulling is allowed
-- -- is deliberately rejected. It would weaken the append-only guarantee,
-- which is the more valuable property, in order to enable a hard delete
-- that this design does not want: users are soft-deleted via deleted_at,
-- and users_delete_blocked already denies DELETE to the application
-- outright (USING (false)). Preserving a deleted user's history so it can
-- be restored later is an explicit product goal, so the inability to
-- hard-delete is the correct behaviour, not a limitation to work around.
--
-- Nothing changes about what is or is not possible. Only the error a
-- reader gets, and what the schema claims when someone reads it.
--
-- Not changed: user_invites.created_by and auth_configuration.updated_by
-- are also ON DELETE SET NULL and are left alone. Neither table has an
-- append-only trigger, so their SET NULL works exactly as declared.

ALTER TABLE auth.auth_audit_logs
    DROP CONSTRAINT auth_audit_logs_actor_user_id_fkey,
    ADD CONSTRAINT auth_audit_logs_actor_user_id_fkey
        FOREIGN KEY (actor_user_id) REFERENCES auth.users(id) ON DELETE RESTRICT;

ALTER TABLE auth.auth_audit_logs
    DROP CONSTRAINT auth_audit_logs_target_user_id_fkey,
    ADD CONSTRAINT auth_audit_logs_target_user_id_fkey
        FOREIGN KEY (target_user_id) REFERENCES auth.users(id) ON DELETE RESTRICT;
