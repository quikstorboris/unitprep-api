-- Restore the two single-row seeds this migration replaced, then drop the
-- file-metadata columns. The columns go BEFORE the re-insert: `pms` is
-- NOT NULL, and the restored rows are the original shape, without it.
CREATE TEMP TABLE _qc ON COMMIT DROP AS
    SELECT * FROM client_ops.vendor_format
    WHERE content_type = 'tenants' AND name = 'QuikStor Cloud';
CREATE TEMP TABLE _sl ON COMMIT DROP AS
    SELECT * FROM client_ops.vendor_format
    WHERE content_type = 'tenants' AND name = 'SiteLink Rent Roll';

DELETE FROM client_ops.vendor_format
WHERE content_type = 'tenants'
  AND name IN ('QuikStor Cloud Alternate Tenants', 'QuikStor Cloud',
               'SiteLink Directory', 'SiteLink Rent Roll');

ALTER TABLE client_ops.vendor_format
    DROP COLUMN guidance,
    DROP COLUMN selection_priority,
    DROP COLUMN file_role,
    DROP COLUMN report_name,
    DROP COLUMN pms;

INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key)
SELECT name, content_type, signature_headers, field_mapping, transform_key FROM _qc;

INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key)
SELECT 'SiteLink', content_type, signature_headers, field_mapping, transform_key FROM _sl;
