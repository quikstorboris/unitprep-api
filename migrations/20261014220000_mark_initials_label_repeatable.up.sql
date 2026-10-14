-- "initials here" legitimately recurs under every acknowledged paragraph of
-- the Longview / T.C.'s lease, all for the same tag (e.init). Without this,
-- the recognizer treats an unanchored label that repeats as ambiguous (like
-- an unanchored "City:" that could be the occupant's, the alternate
-- contact's or the lienholder's) and only suggests it.
UPDATE client_ops.tag_pattern
   SET pattern = pattern || '{"repeats": true}'::jsonb
 WHERE kind = 'label_proximity'
   AND tag_key = 'e.init'
   AND pattern->>'label' = 'initials here';
