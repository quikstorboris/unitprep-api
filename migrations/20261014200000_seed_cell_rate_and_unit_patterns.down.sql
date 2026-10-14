DELETE FROM client_ops.tag_pattern
 WHERE kind = 'label_proximity'
   AND notes LIKE 'Longview Self Storage / T.C.%'
   AND (   (tag_key = 'e.phone' AND pattern->>'label' = 'Cell:')
        OR (tag_key = 'm.effrate' AND pattern->>'label' = 'Rental Rate Per Month')
        OR (tag_key = 'u.num' AND pattern->>'label' = 'LVSS'));
