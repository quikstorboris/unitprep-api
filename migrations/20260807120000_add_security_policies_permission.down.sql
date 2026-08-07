DELETE FROM auth.role_permissions WHERE permission_key = 'security_policies.manage';
DELETE FROM auth.permissions WHERE key = 'security_policies.manage';
