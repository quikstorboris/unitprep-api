-- Fills in two things the Clients page filters (and the company page) need
-- for clients created before they were wired up, from the Process Street
-- Intake answers already stored in each row's raw_ps_snapshot. Clients
-- created from now on get both at creation (clients::staff_resolution and
-- clients::intake_mapping::previous_pms).
--
-- 1. Implementation Manager: the Intake "Who is the Conductor" step's
--    Conductor_Email_Address:, looked up in clients.staff_identity_alias
--    first (the sanctioned redirect for a former employee) and then
--    among OO users by email. No match leaves it unassigned. Never
--    overwrites a manager already set.
-- 2. Previous PMS: a facility whose answer is the umbrella "3rd Party PMS"
--    takes the specific product from "Which 3rd Party Software are they
--    using?" (or, for "Other", the typed name) -- "3rd Party PMS" alone
--    cannot be filtered on. A value a person edited by hand
--    (manually_edited_fields) is left alone.

UPDATE clients.companies c
   SET implementation_manager_user_id = m.user_id
  FROM (
        SELECT c2.id AS company_id,
               COALESCE(
                   (SELECT a.resolved_user_id
                      FROM clients.staff_identity_alias a
                     WHERE lower(a.raw_identifier) = lower(conductor.email)),
                   (SELECT u.id
                      FROM auth.users u
                     WHERE u.deleted_at IS NULL
                       AND lower(u.email::text) = lower(conductor.email))
               ) AS user_id
          FROM clients.companies c2
         CROSS JOIN LATERAL (
               SELECT btrim(f -> 'data' ->> 'value') AS email
                 FROM jsonb_array_elements(
                          CASE WHEN jsonb_typeof(c2.raw_ps_snapshot) = 'array'
                               THEN c2.raw_ps_snapshot ELSE '[]'::jsonb END) f
                WHERE f ->> 'key' = 'Conductor_Email_Address:'
                  AND btrim(f -> 'data' ->> 'value') <> ''
                LIMIT 1
         ) conductor
       ) m
 WHERE c.id = m.company_id
   AND m.user_id IS NOT NULL
   AND c.implementation_manager_user_id IS NULL;

UPDATE clients.facilities fac
   SET previous_pms = s.specific
  FROM (
        SELECT f2.id,
               COALESCE(
                   NULLIF(btrim((SELECT x -> 'data' ->> 'value'
                                   FROM jsonb_array_elements(f2.raw_ps_snapshot) x
                                  WHERE x ->> 'key' = 'Which_3rd_Party_Software_are_they_using?'
                                  LIMIT 1)), ''),
                   NULL
               ) AS picked,
               NULLIF(btrim((SELECT x -> 'data' ->> 'value'
                               FROM jsonb_array_elements(f2.raw_ps_snapshot) x
                              WHERE x ->> 'key' = 'What_is_the_name_of_the_3rd_Party_Software_they_are_using?'
                              LIMIT 1)), '') AS typed
          FROM clients.facilities f2
         WHERE jsonb_typeof(f2.raw_ps_snapshot) = 'array'
       ) p
  CROSS JOIN LATERAL (
        SELECT CASE WHEN p.picked IS NOT NULL AND lower(p.picked) <> 'other' THEN p.picked
                    ELSE p.typed END AS specific
       ) s
 WHERE fac.id = p.id
   AND s.specific IS NOT NULL
   AND fac.previous_pms = '3rd Party PMS'
   AND NOT ('previous_pms' = ANY (fac.manually_edited_fields));
