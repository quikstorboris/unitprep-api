DELETE FROM auth.role_permissions
WHERE permission_key = 'client_ops.manage_vendor_formats'
  AND role_id = (SELECT id FROM auth.roles WHERE key = 'developer');
