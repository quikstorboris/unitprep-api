-- Reverses 20260730160000.
--
-- Dropping the lockout columns discards any in-flight lock, which is the
-- safe direction: it un-locks rather than locking, and the credential's
-- secret and confirmed_at are untouched.

DROP FUNCTION auth.record_totp_success(UUID);
DROP FUNCTION auth.record_totp_failure(UUID);
DROP FUNCTION auth.resolve_totp_candidate(citext);

ALTER TABLE auth.totp_credentials
    DROP COLUMN locked_until,
    DROP COLUMN failed_attempts;
