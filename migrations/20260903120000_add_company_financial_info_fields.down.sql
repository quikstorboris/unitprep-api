ALTER TABLE clients.companies
    DROP COLUMN accepted_payment_methods,
    DROP COLUMN accounting_basis,
    DROP COLUMN payment_scheme,
    DROP COLUMN offers_tenant_insurance_raw,
    DROP COLUMN insurance_provider;
