UPDATE client_ops.vendor_format
SET file_role = 'supporting',
    field_mapping = '[]'::jsonb,
    guidance = 'One row per lease, linking each tenant (LegacyTenantId) to a unit (UnitNumber). It would supply the unit numbers Tenants.csv lacks. Recognized, but not used yet.'
WHERE content_type = 'tenants' AND name = 'QuikStor Cloud Leases';

UPDATE client_ops.vendor_format
SET guidance = 'Tenants.csv from the QuikStor Cloud pull: one row per lease with the tenant name, contact details and address, but no unit numbers, so findings name records by tenant ID (LegacyTenantId). Leases.csv, if present, links tenants to units; it is recognized but not used yet. Units.csv has no tenant link and is not a dedup file.'
WHERE content_type = 'tenants' AND name = 'QuikStor Cloud';

UPDATE client_ops.vendor_format
SET guidance = 'Tenants.csv from the QuikStor Cloud pull (the variant whose address columns are AddressStreet1, AddressCity, ...): one row per tenant record with the name, contact details and address, but no unit numbers, so findings name records by tenant ID (LegacyTenantId). Leases.csv links tenants to units; it is recognized but not used yet. Units.csv has no tenant link and is not a dedup file.'
WHERE content_type = 'tenants' AND name = 'QuikStor Cloud (street address headers)';
