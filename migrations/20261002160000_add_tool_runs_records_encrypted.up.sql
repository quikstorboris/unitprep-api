-- The normalized tenant records a Dedup run was computed from (name,
-- address, phone, email, customer id, unit: the dedup fields only, never
-- the source file's card or SSN columns), encrypted with
-- CLIENT_PII_ENCRYPTION_KEY and bound to the run's session_id. Lets
-- Onboarding Work re-run a past check with a different choice for the
-- tenants that had no customer id ("rematch") without needing every file
-- the run was built from: a joined run (Winsen) has several sources but
-- only one stored source file. NULL for runs recorded before this column
-- existed, which cannot be rematched.
ALTER TABLE client_ops.tool_runs
    ADD COLUMN records_encrypted BYTEA;
