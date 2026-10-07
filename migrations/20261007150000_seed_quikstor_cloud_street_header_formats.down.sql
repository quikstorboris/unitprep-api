DELETE FROM client_ops.vendor_format
WHERE content_type = 'tenants'
  AND name IN (
      'QuikStor Cloud Alternate Tenants (street address headers)',
      'QuikStor Cloud (street address headers)',
      'QuikStor Cloud Leases'
  );

UPDATE client_ops.vendor_format
SET guidance = 'Tenants.csv from the QuikStor Cloud pull: one row per lease with the tenant name, contact details and address, but no unit numbers, so findings name records by tenant ID (LegacyTenantId). Unit numbers need a leases export that links tenants to units; that file is not supported yet. Units.csv has no tenant link and is not a dedup file.'
WHERE content_type = 'tenants' AND name = 'QuikStor Cloud';
