-- Three independent changes, bundled because they all came out of the
-- same design discussion 2026-08-04: making auth.auth_configuration's
-- step_up_actions column actually load-bearing, dropping a column that
-- turned out to be vestigial, and closing a real gap in TOTP
-- re-enrollment.

-- mandatory_passkey_enrollment has no code path that ever makes passkey
-- enrollment optional -- it's baked into the registration flow itself,
-- not a toggle. Never read anywhere; removed rather than left as an
-- inert column implying a control that doesn't exist.
ALTER TABLE auth.auth_configuration DROP COLUMN mandatory_passkey_enrollment;

-- step_up_actions is being wired up for the first time (see
-- auth::step_up_policy) to gate "add a passkey to an account that
-- already has one" -- previously hardcoded as unconditionally required.
-- Seeded here, not left at the column's own default ('[]'), specifically
-- so turning this into a config-driven check does not silently disable
-- protection that already existed unconditionally.
UPDATE auth.auth_configuration SET step_up_actions = '["add_passkey"]'::jsonb WHERE id = 1;

-- auth_configuration was admin-only-readable (auth_configuration_admin_only,
-- FOR ALL) since it was created, which was fine while nothing read it.
-- Now that an ordinary user's own request needs to check step_up_actions
-- to know whether *their* action is gated, admin-only-select would make
-- that check impossible for a non-admin caller. Adds a second, narrower
-- permissive policy for SELECT only -- Postgres ORs multiple permissive
-- policies for the same command together, so this widens who can read
-- without touching who can write (INSERT/UPDATE/DELETE stay covered only
-- by the original admin-only FOR ALL policy).
CREATE POLICY auth_configuration_select_any_authenticated ON auth.auth_configuration
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

-- TOTP re-enrollment gap: `store_unconfirmed_secret` previously wrote the
-- new candidate straight into secret_encrypted and cleared confirmed_at
-- immediately at enroll/begin -- meaning the *existing* confirmed secret
-- stopped working the moment re-enrollment started, not when it finished.
-- Anyone who abandoned a re-enrollment partway (closed the tab, lost
-- power, anything) was left with no working step-up factor at all until
-- they came back and finished it.
--
-- pending_secret_encrypted holds the *candidate* secret instead.
-- enroll/begin only ever writes here now; the existing confirmed
-- secret_encrypted/confirmed_at are untouched throughout the entire
-- re-enrollment window. enroll/confirm promotes pending_secret_encrypted
-- into secret_encrypted (and clears this column) only on a verified
-- code -- so the old factor keeps working right up until the new one is
-- actually proven, not just until someone started replacing it.
ALTER TABLE auth.totp_credentials ADD COLUMN pending_secret_encrypted BYTEA;

COMMENT ON COLUMN auth.totp_credentials.pending_secret_encrypted IS
    'Candidate secret written by /auth/totp/enroll/begin, promoted to secret_encrypted only once /auth/totp/enroll/confirm verifies a code against it. Keeps the existing confirmed secret (if any) working for the entire re-enrollment window rather than only until begin is called.';

-- secret_encrypted was NOT NULL because every row used to get one written
-- immediately at enroll/begin. Now a brand-new enrolment writes only
-- pending_secret_encrypted and leaves secret_encrypted unset until the
-- first confirm succeeds -- so a row can legitimately exist with no live
-- secret yet. Safe to relax: the only reader of secret_encrypted for
-- step-up purposes (load_own_confirmed_secret) already filters
-- `confirmed_at IS NOT NULL`, and confirmed_at is only ever set in the
-- same statement that populates secret_encrypted, so a NULL
-- secret_encrypted can never be reachable as a "confirmed" credential.
ALTER TABLE auth.totp_credentials ALTER COLUMN secret_encrypted DROP NOT NULL;
