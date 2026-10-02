-- Whether client_ops.tool_runs.source_bytes holds ChaCha20-Poly1305
-- ciphertext (new rows, keyed by CLIENT_PII_ENCRYPTION_KEY, bound to the
-- run's session_id) or the plaintext upload that rows written before this
-- migration still carry. The stored source is the raw upload and can hold
-- card ciphertext, tokens and SSNs, so it is encrypted at the application
-- layer like the other client PII. Existing rows keep false until
-- re-encrypted.
ALTER TABLE client_ops.tool_runs
    ADD COLUMN source_encrypted BOOLEAN NOT NULL DEFAULT false;
