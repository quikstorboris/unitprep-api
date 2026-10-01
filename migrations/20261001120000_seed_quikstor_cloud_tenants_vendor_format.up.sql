-- QuikStor Cloud's tenant export ("Tenants.csv" from a preliminary data
-- pull, confirmed against Freeland Warehousing & Storage, 2026-10-01).
-- Registered for dedup (content_type = 'tenants') so it is recognized by
-- its headers like QSX and Easy Storage Solutions are.
--
-- Shape differs from QSX: one row per tenant record (a tenant with
-- several leases repeats), keyed by LegacyTenantId, with no unit column
-- and no single name or phone column. So `FirtLast` (the grouping key),
-- `PhoneNumber` and `PhoneNumberPrefix` are derived by the named
-- transform `derive_quikstor_cloud_tenant_fields`
-- (core::vendor_format::transforms), which runs BEFORE the rename step
-- and writes those canonical headers itself -- the three identity pairs
-- below just pick them up, same arrangement as Easy Storage Solutions'
-- address columns. The transform also blanks the export's `#NoEmail`
-- Email sentinel so it never reads as a shared address.
--
-- This file carries no unit column or tenant-to-unit link, but dedup
-- names records by unit in every note ("units 12 and 14"), so
-- LegacyTenantId is mapped to BOTH CustNumb and UnitNumber: notes then
-- identify each record by its tenant ID instead of printing blanks. The
-- notes still say "unit"; read it as "tenant record" for this vendor.
--
-- Deliberately unmapped (dropped, not blank): every AlternateContact*
-- field -- those live in the separate AlternateTenants.csv, keyed by
-- LegacyTenantId, which dedup does not join.
--
-- Signature is the headers that identify this export family without
-- leaning on any column QSX or Easy Storage Solutions also use.
-- AlternateTenants.csv carries a superset of these headers and so also
-- matches; recognition is by presence only.
INSERT INTO client_ops.vendor_format (name, content_type, signature_headers, field_mapping, transform_key) VALUES
('QuikStor Cloud', 'tenants', ARRAY['LegacyTenantId','AccountType','FirstName','LastName','AddressLine','CellPhoneNumber'], '[
    {"target":"CustNumb","source":"LegacyTenantId"},
    {"target":"UnitNumber","source":"LegacyTenantId"},
    {"target":"FirtLast","source":"FirtLast"},
    {"target":"FirstName","source":"FirstName"},
    {"target":"LastName","source":"LastName"},
    {"target":"CompanyName","source":"CompanyName"},
    {"target":"PhoneNumber","source":"PhoneNumber"},
    {"target":"PhoneNumberPrefix","source":"PhoneNumberPrefix"},
    {"target":"Email","source":"Email"},
    {"target":"AddressStreet1","source":"AddressLine"},
    {"target":"AddressStreet2","source":"AddressLineOptional"},
    {"target":"AddressCity","source":"City"},
    {"target":"AddressState","source":"State"},
    {"target":"AddressPostalCode","source":"PostalCode"}
]'::jsonb, 'derive_quikstor_cloud_tenant_fields');
