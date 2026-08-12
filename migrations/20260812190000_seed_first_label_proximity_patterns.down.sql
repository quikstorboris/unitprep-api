DELETE FROM client_ops.tag_pattern
 WHERE kind = 'label_proximity'
   AND pattern->>'label' IN (
       'Customer''s Initials',
       'OCCUPANT NAME',
       'SECOND PARTY',
       'UNIT #',
       'CITY',
       'ZIP CODE',
       'SECURITY DEPOSIT'
   );
