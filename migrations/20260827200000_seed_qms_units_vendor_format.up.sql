-- QMS is QuikStor's own new PMS, replacing QSX. Registering it here lets
-- Group Prep auto-recognize a QMS unit-list export instead of every
-- facility onboarding needing the manual-upload override
-- (/unit-file/upload) added for this exact case.
--
-- Header shape and the four mapped fields below are confirmed against a
-- real export (Prairie Enterprises LLC / Pyott Road Self Storage,
-- 2026-08-27) -- Number/UnitGroup/Category/StandardRate all have an
-- unambiguous, direct source column. `Active` is deliberately left
-- unmapped: QMS's UnitStatus column carries short codes (R0, L0, D3, E0,
-- O3, U0, A0, ...) whose active/vacant/damaged meaning needs confirming
-- with Boris before it's safe to encode -- Active is optional/
-- informational only (see unit-group::format::REQUIRED_TARGET_FIELDS),
-- so this is safe to add later without touching this row (either via a
-- follow-up migration, or per-import through the manual mapping UI).
INSERT INTO client_ops.vendor_format (name, content_type, signature_headers, field_mapping) VALUES
('QMS', 'units', ARRAY['UnitNumber','SizeCode','SizeDescription','UnitStatus'], '[
    {"target":"Number","source":"UnitNumber"},
    {"target":"UnitGroup","source":"SizeDescription"},
    {"target":"Category","source":"UnitType"},
    {"target":"StandardRate","source":"StandardRate"}
]'::jsonb);
