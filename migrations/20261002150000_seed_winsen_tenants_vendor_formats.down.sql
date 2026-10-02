DELETE FROM client_ops.vendor_format
WHERE content_type = 'tenants' AND pms = 'Winsen';

ALTER TABLE client_ops.vendor_format
    DROP CONSTRAINT IF EXISTS vendor_format_file_role_check;
ALTER TABLE client_ops.vendor_format
    ADD CONSTRAINT vendor_format_file_role_check
    CHECK (file_role IN ('primary', 'supporting'));
