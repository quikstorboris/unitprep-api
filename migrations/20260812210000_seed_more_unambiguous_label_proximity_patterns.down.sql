DELETE FROM client_ops.tag_pattern
 WHERE kind = 'label_proximity'
   AND pattern->>'label' IN (
       'DATE',
       'CONTACT PHONE',
       'ST',
       'ALTERNATE NAME'
   );
