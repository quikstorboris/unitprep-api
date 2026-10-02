-- SiteLink unit file for Group Prep (content_type = 'units'): the Custom
-- Unit Report, one row per unit with no tenant data.
--
-- Boris's decision (2026-10-02): a SiteLink unit group is unit TYPE x
-- UNIT SIZE (the grain SiteLink's Price List prices at), e.g.
-- "Self Storage 10x20". The report carries the two as separate columns
-- and has no group column, so the `derive_sitelink_unit_group` transform
-- appends `UnitGroup` and this mapping picks it up as an ordinary rename.
--
-- Mapped: unit number, the derived group, rate and dimensions.
-- Deliberately NOT mapped: Power/Climate/Inside/Alarm/... -- the report's
-- flags are 'X' or a blank-space string rather than booleans, and the
-- locality/climate checks would read a blank as a real "no". Real sample
-- (LG Squared, 308 units): 20 distinct type x size groups.
INSERT INTO client_ops.vendor_format
    (name, content_type, signature_headers, field_mapping, transform_key,
     pms, report_name, file_role, selection_priority, guidance)
VALUES
('SiteLink Custom Unit Report', 'units',
 ARRAY['UnitName','Type','UnitSize','Width','Length','StandardRate'], '[
    {"target":"Number","source":"UnitName"},
    {"target":"UnitGroup","source":"UnitGroup"},
    {"target":"StandardRate","source":"StandardRate"},
    {"target":"Width","source":"Width"},
    {"target":"Length","source":"Length"}
]'::jsonb, 'derive_sitelink_unit_group',
 'SiteLink', 'Custom Unit Report', 'primary', 0,
 'SiteLink Custom Unit Report: one row per unit. Unit groups are built as unit type plus unit size (for example "Self Storage 10x20").');
