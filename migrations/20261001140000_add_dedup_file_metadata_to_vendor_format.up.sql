-- File-level metadata for the dedup folder scan. A registry row is one
-- file FORMAT (a report a PMS can export); these columns let the app
-- say which PMS a format belongs to, what the report is called, whether
-- it can run on its own, which of several alternatives to pre-select, and
-- what to tell the user about it ("Files required for deduplication").
--
--   pms                 the PMS / vendor the format belongs to (panel grouping)
--   report_name         the report or file as the user knows it
--   file_role           'primary'    a self-contained tenant file
--                       'supporting' recognized, but cannot be checked on
--                                    its own until a join exists
--   selection_priority  higher wins when a folder holds several primaries
--                       of one PMS (e.g. SiteLink Directory over Rent Roll)
--   guidance            plain text shown in the panel
ALTER TABLE client_ops.vendor_format
    ADD COLUMN pms TEXT,
    ADD COLUMN report_name TEXT,
    ADD COLUMN file_role TEXT NOT NULL DEFAULT 'primary'
        CHECK (file_role IN ('primary', 'supporting')),
    ADD COLUMN selection_priority INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN guidance TEXT;

UPDATE client_ops.vendor_format SET pms = name;
ALTER TABLE client_ops.vendor_format ALTER COLUMN pms SET NOT NULL;

UPDATE client_ops.vendor_format
SET report_name = 'End Users export',
    guidance = 'The QSX / QMS End Users export (for example ARSS_QMS_End_Users_Template.csv): one row per tenant per unit, with CustNumb, UnitNumber and FirtLast columns. This one file is all that is needed.'
WHERE content_type = 'tenants' AND name = 'QSX';

UPDATE client_ops.vendor_format
SET report_name = 'Full Tenant Data',
    guidance = 'The Easy Storage Solutions "Full Tenant Data" export: one row per unit with Name, Address, Phone, Email and an Alternate Contact block. This one file is all that is needed.'
WHERE content_type = 'tenants' AND name = 'Easy Storage Solutions';

-- Detection takes the FIRST matching row by id, so a format whose
-- signature is a superset of another's must come before it. The two
-- applied seed rows below are therefore re-created (copied, then
-- re-inserted) in the right order instead of edited in place.
CREATE TEMP TABLE _qc ON COMMIT DROP AS
    SELECT * FROM client_ops.vendor_format
    WHERE content_type = 'tenants' AND name = 'QuikStor Cloud';
CREATE TEMP TABLE _sl ON COMMIT DROP AS
    SELECT * FROM client_ops.vendor_format
    WHERE content_type = 'tenants' AND name = 'SiteLink';

DELETE FROM client_ops.vendor_format
WHERE content_type = 'tenants' AND name IN ('QuikStor Cloud', 'SiteLink');

-- QuikStor Cloud: AlternateTenants.csv carries every Tenants.csv header
-- plus its own, so it is registered first as a supporting file and is
-- never mistaken for the tenant file.
INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
SELECT 'QuikStor Cloud Alternate Tenants', 'tenants',
       signature_headers || ARRAY['LegacyAlternateTenantId'], '[]'::jsonb, NULL,
       'QuikStor Cloud', 'AlternateTenants.csv', 'supporting', 0,
       'Alternate contacts for the tenants in Tenants.csv (keyed by LegacyTenantId). Recognized, but not checked on its own yet.'
FROM _qc;

INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
SELECT name, content_type, signature_headers, field_mapping, transform_key,
       'QuikStor Cloud', 'Tenants.csv', 'primary', 0,
       'Tenants.csv from the QuikStor Cloud pull: one row per lease with the tenant name, contact details and address, but no unit numbers, so findings name records by tenant ID (LegacyTenantId). Unit numbers need a leases export that links tenants to units; that file is not supported yet. Units.csv has no tenant link and is not a dedup file.'
FROM _qc;

-- SiteLink: the Directory report is the Rent Roll's headers plus
-- TenantName, so it is registered first and wins on a Directory file.
INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
SELECT 'SiteLink Directory', content_type, signature_headers || ARRAY['TenantName'],
       field_mapping, transform_key,
       'SiteLink', 'Directory', 'primary', 20,
       'The SiteLink Directory report: one row per active lease (tenant, unit and ledger together). Preferred, because it lists only active tenants. Select this OR the Rent Roll, not both. The other SiteLink reports lack address and email and are not used; several hold card and access-code data, so select only this file.'
FROM _sl;

INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
SELECT 'SiteLink Rent Roll', content_type, signature_headers,
       field_mapping, transform_key,
       'SiteLink', 'Rent Roll', 'primary', 10,
       'The SiteLink Rent Roll report: one row per unit, vacant units included (they are skipped automatically). It lists the same tenants as the Directory; use it only when the Directory is not available.'
FROM _sl;
