DELETE FROM auth.role_permissions WHERE permission_key = 'client_ops.manage_vendor_formats';
DELETE FROM auth.permissions WHERE key = 'client_ops.manage_vendor_formats';
DROP TABLE client_ops.vendor_format;
