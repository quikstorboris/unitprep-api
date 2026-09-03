-- Phase 4 (Client record UI): the Company page's "Financial Information"
-- section, per the vault's own design note. These 5 fields turned out to
-- live on the Intake workflow, not New Merchant Account as the design
-- note assumed -- confirmed directly against Highway 20's real Intake
-- fixture (`Accepted_Payment_Methods:`, `Accounting_Basis:`,
-- `Payment_Scheme:`, `Are_they_currently_offering_insurance/protection_to_
-- their_tenants?`, `Who_is_their_insurance_provider?`). Company-level,
-- same "whichever facility's Intake answered first-time = Yes" convention
-- already used for corporate_email/phone/address/subdomain -- see this
-- table's own original migration comment.
ALTER TABLE clients.companies
    ADD COLUMN accepted_payment_methods TEXT,
    ADD COLUMN accounting_basis TEXT,
    ADD COLUMN payment_scheme TEXT,
    -- Kept as raw text, not boolean -- same Phase 1 convention as every
    -- other yes/no PS field in this schema (sales_tax_applies_raw etc.).
    ADD COLUMN offers_tenant_insurance_raw TEXT,
    ADD COLUMN insurance_provider TEXT;
