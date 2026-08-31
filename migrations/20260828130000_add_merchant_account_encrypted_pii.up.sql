-- Field-level encryption for the New Merchant Account workflow's
-- genuinely sensitive data -- SSN/DOB/home address for the signer and
-- every listed owner, plus EIN, bank routing/account numbers, and the
-- QMS/processor system credentials the same PS form hands over.
-- Confirmed present with real values while building Phase 1 ingestion
-- for the Prairie Enterprises / Highway 20 golden fixture (2026-08-28):
-- 3 real owners with full SSN/DOB/address, a real bank account number,
-- and a real QUIKSTOR_Password in plaintext on the PS side.
--
-- This is the trigger the vault's 2026-08-07 PII/compliance review
-- named in advance and deliberately deferred building for until it was
-- "actually scoped" -- see src/clients/encryption.rs's module doc for
-- the full reasoning (mirrors auth::totp's ChaCha20-Poly1305 approach
-- under its own key, CLIENT_PII_ENCRYPTION_KEY).
--
-- Two schema changes:
-- 1. `encrypted_secrets` on facility_merchant_accounts -- one blob per
--    facility for EIN/bank routing+account/QMS system credentials.
-- 2. A new facility_merchant_account_parties table -- the signer and up
--    to 4 owners and 4 intermediary businesses each get their own row,
--    with a per-row `encrypted_pii` blob (SSN/DOB/home address) bound to
--    that specific party via AAD, not just the facility -- so a
--    ciphertext copied from one owner's row onto a sibling owner's row
--    within the same facility still fails to decrypt.
--
-- Both tables' SELECT policy is tightened to onboarding_manager/
-- department_manager only -- NOT the blanket "any authenticated caller"
-- every other clients-schema table uses. The underlying bytes are
-- encrypted either way, but there is no legitimate business reason for
-- `sales` (read-only access to client records generally) to read
-- merchant-account KYC ciphertext at all, and least-privilege here
-- costs nothing.

ALTER TABLE clients.facility_merchant_accounts
    ADD COLUMN encrypted_secrets BYTEA;

DROP POLICY facility_merchant_accounts_select_authenticated ON clients.facility_merchant_accounts;

CREATE POLICY facility_merchant_accounts_select_client_ops_roles ON clients.facility_merchant_accounts
    FOR SELECT
    USING (
        auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );

CREATE TABLE clients.facility_merchant_account_parties (
    id BIGSERIAL PRIMARY KEY,
    facility_id UUID NOT NULL REFERENCES clients.facilities(id) ON DELETE CASCADE,
    party_role TEXT NOT NULL CHECK (party_role IN ('signer', 'owner', 'intermediary_business')),
    -- 0 for signer (there is exactly one), 1-4 for owner/
    -- intermediary_business slots. Never NULL -- Postgres UNIQUE treats
    -- NULLs as distinct, which would silently defeat the constraint
    -- below for the signer row.
    party_index INTEGER NOT NULL,
    -- A person's full name (signer/owner) or a business's name
    -- (intermediary_business) -- not sensitive, kept plain.
    display_name TEXT,
    title TEXT,
    ownership_percent NUMERIC,
    email TEXT,
    phone TEXT,
    country_of_citizenship TEXT,
    country TEXT,
    -- Encrypted JSON: {ssn, dob, home_address_line1, home_city,
    -- home_state_or_province, home_postal_code, home_country}. Null for
    -- intermediary_business rows (businesses have no SSN/DOB/home
    -- address in this form) and for any party whose fields were blank
    -- in PS.
    encrypted_pii BYTEA,
    source TEXT NOT NULL CHECK (source IN ('process_street', 'manual')),
    ps_new_merchant_run_id TEXT,
    last_synced_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (facility_id, party_role, party_index)
);

CREATE INDEX facility_merchant_account_parties_facility_id_idx
    ON clients.facility_merchant_account_parties(facility_id);

CREATE TRIGGER facility_merchant_account_parties_set_updated_at
    BEFORE UPDATE ON clients.facility_merchant_account_parties
    FOR EACH ROW
    EXECUTE FUNCTION auth.set_updated_at();

ALTER TABLE clients.facility_merchant_account_parties ENABLE ROW LEVEL SECURITY;

CREATE POLICY facility_merchant_account_parties_select_client_ops_roles
    ON clients.facility_merchant_account_parties
    FOR SELECT
    USING (
        auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );

CREATE POLICY facility_merchant_account_parties_insert_client_ops_roles
    ON clients.facility_merchant_account_parties
    FOR INSERT
    WITH CHECK (
        auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );
CREATE POLICY facility_merchant_account_parties_update_client_ops_roles
    ON clients.facility_merchant_account_parties
    FOR UPDATE
    USING (
        auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    )
    WITH CHECK (
        auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );
CREATE POLICY facility_merchant_account_parties_delete_client_ops_roles
    ON clients.facility_merchant_account_parties
    FOR DELETE
    USING (
        auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );
