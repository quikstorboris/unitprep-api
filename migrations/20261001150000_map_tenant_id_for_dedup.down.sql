UPDATE client_ops.vendor_format
SET field_mapping = COALESCE(
    (SELECT jsonb_agg(entry)
       FROM jsonb_array_elements(field_mapping) AS entry
      WHERE entry ->> 'target' <> 'TenantId'),
    '[]'::jsonb)
WHERE content_type = 'tenants'
  AND name IN ('SiteLink Directory', 'SiteLink Rent Roll', 'QuikStor Cloud');
