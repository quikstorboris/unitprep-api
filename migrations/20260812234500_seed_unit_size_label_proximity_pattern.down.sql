DELETE FROM client_ops.tag_pattern
 WHERE kind = 'label_proximity'
   AND tag_key = 'u.dim'
   AND pattern->>'label' = 'SIZE';
