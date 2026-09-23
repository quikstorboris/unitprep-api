-- Lets the Onboarding Work tab's new "delete a mistaken run" action
-- actually delete a row. `20260910120000_create_client_ops_tool_runs`
-- deliberately shipped with no DELETE policy at all (an append-only
-- posture, same reasoning as client_ops.audit_log) -- but a Dedup check
-- run against the wrong facility's data is a real operational mistake
-- that needs a way to be cleared, not lived with forever.
--
-- Gated the same way every other client-ops destructive action is --
-- see `20260909170000_add_developer_role`'s own top comment for why
-- `auth.current_user_is_client_ops_role()` (not a fresh
-- `onboarding_manager OR department_manager` chain) is the right
-- function to reuse here, and for the Rust-layer
-- `require_permission("client_ops.perform", ...)` check that backs it
-- up as the UX layer -- this policy is the real enforcement.
CREATE POLICY tool_runs_delete_client_ops_roles ON client_ops.tool_runs
    FOR DELETE USING (auth.current_user_is_client_ops_role());
