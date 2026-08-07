UPDATE auth.roles
   SET key = 'district_manager',
       label = 'District Manager'
 WHERE key = 'department_manager';
