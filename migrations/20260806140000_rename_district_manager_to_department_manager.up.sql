-- Renamed same-day as the role's own creation: "district manager" collides
-- with real business nomenclature already in use for a client-side
-- facility manager role (self-storage industry term), a completely
-- different concept from this internal UnitPrep staff role (approves QMS
-- credential changes, oversees onboarding managers). Renamed to
-- Department Manager to remove the collision entirely -- both the key and
-- the label, not just the display label, so no trace of the confusing
-- term survives anywhere a developer or admin might read it.
--
-- A plain UPDATE is safe here: auth.role_permissions and auth.user_roles
-- both reference auth.roles by id (UUID), not by key, so renaming the key
-- does not disturb either mapping.
UPDATE auth.roles
   SET key = 'department_manager',
       label = 'Department Manager'
 WHERE key = 'district_manager';
