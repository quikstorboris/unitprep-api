DELETE FROM auth.role_permissions WHERE permission_key = 'integrations.manage';
DELETE FROM auth.permissions WHERE key = 'integrations.manage';
