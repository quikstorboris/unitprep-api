-- QuikStor Cloud, second header variant (Davidson Road Self Storage's
-- "1st Prelim Data" pull, 2026-10-07). Same product and file set as the
-- Freeland pull the first QuikStor Cloud rows were built from, but its
-- Tenants.csv / AlternateTenants.csv name the address columns
-- AddressStreet1 / AddressStreet2 / AddressCity / AddressState /
-- AddressPostalCode (and add a Gender column) instead of AddressLine /
-- AddressLineOptional / City / State / PostalCode. The first rows require
-- AddressLine in their signature, so none of this pull's files were
-- recognized and dedup refused to run.
--
-- The original rows are left exactly as they are (Freeland-style exports
-- still match them); these are additional rows for the variant. Detection
-- takes the FIRST matching row by id and AlternateTenants.csv carries every
-- Tenants.csv header plus its own, so the alternate-contacts row is
-- inserted BEFORE the tenants row. Nothing else matches these signatures
-- (no other format has LegacyTenantId + AddressStreet1, or the lease ids).
--
-- The same derive transform applies (it reads FirstName / LastName /
-- CompanyName / the three phone slots / Email, all unchanged). Because the
-- source address headers already are dedup's canonical names, those
-- mapping entries are identity pairs.

INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
VALUES
('QuikStor Cloud Alternate Tenants (street address headers)', 'tenants',
 ARRAY['LegacyTenantId','AccountType','FirstName','LastName','AddressStreet1','CellPhoneNumber','LegacyAlternateTenantId'],
 '[]'::jsonb, NULL,
 'QuikStor Cloud', 'AlternateTenants.csv', 'supporting', 0,
 'Alternate contacts for the tenants in Tenants.csv (keyed by LegacyTenantId). Recognized, but not checked on its own yet.');

INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
VALUES
('QuikStor Cloud (street address headers)', 'tenants',
 ARRAY['LegacyTenantId','AccountType','FirstName','LastName','AddressStreet1','CellPhoneNumber'],
 '[
    {"target":"CustNumb","source":"LegacyTenantId"},
    {"target":"UnitNumber","source":"LegacyTenantId"},
    {"target":"TenantId","source":"LegacyTenantId"},
    {"target":"FirtLast","source":"FirtLast"},
    {"target":"FirstName","source":"FirstName"},
    {"target":"LastName","source":"LastName"},
    {"target":"CompanyName","source":"CompanyName"},
    {"target":"PhoneNumber","source":"PhoneNumber"},
    {"target":"PhoneNumberPrefix","source":"PhoneNumberPrefix"},
    {"target":"Email","source":"Email"},
    {"target":"AddressStreet1","source":"AddressStreet1"},
    {"target":"AddressStreet2","source":"AddressStreet2"},
    {"target":"AddressCity","source":"AddressCity"},
    {"target":"AddressState","source":"AddressState"},
    {"target":"AddressPostalCode","source":"AddressPostalCode"}
 ]'::jsonb,
 'derive_quikstor_cloud_tenant_fields',
 'QuikStor Cloud', 'Tenants.csv', 'primary', 0,
 'Tenants.csv from the QuikStor Cloud pull (the variant whose address columns are AddressStreet1, AddressCity, ...): one row per tenant record with the name, contact details and address, but no unit numbers, so findings name records by tenant ID (LegacyTenantId). Leases.csv links tenants to units; it is recognized but not used yet. Units.csv has no tenant link and is not a dedup file.');

-- Leases.csv: the tenant-to-unit link Freeland's pull lacked (one row per
-- lease: LegacyTenantId + UnitNumber). Registered as a supporting file so
-- it is recognized (and shown in the requirements panel) rather than
-- listed as unrecognized; using it needs a join step that does not exist
-- for this system yet.
INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
VALUES
('QuikStor Cloud Leases', 'tenants',
 ARRAY['LegacyLeaseId','LegacyTenantId','LegacyUnitId','UnitNumber'],
 '[]'::jsonb, NULL,
 'QuikStor Cloud', 'Leases.csv', 'supporting', 0,
 'One row per lease, linking each tenant (LegacyTenantId) to a unit (UnitNumber). It would supply the unit numbers Tenants.csv lacks. Recognized, but not used yet.');

-- The first QuikStor Cloud row's guidance said the leases file "is not
-- supported yet"; it is now recognized (not yet used).
UPDATE client_ops.vendor_format
SET guidance = 'Tenants.csv from the QuikStor Cloud pull: one row per lease with the tenant name, contact details and address, but no unit numbers, so findings name records by tenant ID (LegacyTenantId). Leases.csv, if present, links tenants to units; it is recognized but not used yet. Units.csv has no tenant link and is not a dedup file.'
WHERE content_type = 'tenants' AND name = 'QuikStor Cloud';
