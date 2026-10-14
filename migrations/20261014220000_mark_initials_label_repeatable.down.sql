UPDATE client_ops.tag_pattern
   SET pattern = pattern - 'repeats'
 WHERE kind = 'label_proximity'
   AND tag_key = 'e.init'
   AND pattern->>'label' = 'initials here';
