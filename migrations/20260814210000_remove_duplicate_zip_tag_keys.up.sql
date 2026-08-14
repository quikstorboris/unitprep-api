-- 'e.zip' was accidentally re-added by the production-sweep migration
-- (20260811090000) as a duplicate of the already-existing 'e.post'
-- (seeded earlier by 20260808120000) -- both are the tenant's postal
-- code under the 'Tenant' category. The canonical tag is 'e.post';
-- 'e.zip' was never referenced by any client_ops.tag_pattern row (the
-- label_proximity seed only ever used 'e.post'), so removing it is a
-- pure catalog cleanup with no pattern-library fallout.
DELETE FROM client_ops.qms_tag WHERE tag_key = 'e.zip';
