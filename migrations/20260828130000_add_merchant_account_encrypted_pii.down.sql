DROP TABLE clients.facility_merchant_account_parties;

DROP POLICY facility_merchant_accounts_select_client_ops_roles ON clients.facility_merchant_accounts;

CREATE POLICY facility_merchant_accounts_select_authenticated ON clients.facility_merchant_accounts
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

ALTER TABLE clients.facility_merchant_accounts
    DROP COLUMN encrypted_secrets;
