-- Map the vendor's own tenant identifier into dedup's canonical TenantId
-- field, so tenants are grouped by id (and one person under several ids
-- is reported) instead of by name alone.
--
--   SiteLink              TenantID        (the Directory / Rent Roll tenant record)
--   QuikStor Cloud        LegacyTenantId  (also still mapped to CustNumb / UnitNumber)
--
-- QSX and Easy Storage Solutions have no tenant id, only a per-unit
-- CustNumb, so they keep grouping by name. Adding a format that has one
-- (Winsen / Sentinel CustID, etc.) is just another entry like these.
UPDATE client_ops.vendor_format
SET field_mapping = field_mapping || '[{"target":"TenantId","source":"TenantID"}]'::jsonb
WHERE content_type = 'tenants'
  AND name IN ('SiteLink Directory', 'SiteLink Rent Roll');

UPDATE client_ops.vendor_format
SET field_mapping = field_mapping || '[{"target":"TenantId","source":"LegacyTenantId"}]'::jsonb
WHERE content_type = 'tenants'
  AND name = 'QuikStor Cloud';
