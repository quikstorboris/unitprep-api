-- Phase 2 recognizer's pattern library: distinct from qms_tag, which is
-- just the catalog of what a tag_key MEANS. This table is the corpus of
-- how a tag's value actually shows up in a real document -- the label
-- text near a blank, or a fill-in-the-blank sentence shape -- that the
-- recognizer matches against. One row per confirmed real phrasing found
-- in the document sweep, mirroring the growth philosophy already
-- established for qms_tag itself ("if we found at least one real
-- occurrence, it belongs in the DB"). See the vault's QMS Template Tags
-- notes for corpus provenance.
--
-- Two kinds for now:
--   label_proximity -- a literal label/phrase found near a blank
--                       ("Move-In Date", "Move In Date", ...)
--   sentence_pattern -- a fill-in-the-blank sentence template, used for
--                       prose-embedded blanks and composite fields (a
--                       date or amount split across multiple blanks)
--
-- Shape validation (phone/email/zip) is deliberately NOT a third kind
-- here -- it's generic, cross-cutting regex logic applied to whichever
-- tag a match already resolved to, not per-tag authored content. See
-- value_shape on qms_tag below instead.

CREATE TABLE client_ops.tag_pattern (
    id BIGSERIAL PRIMARY KEY,
    tag_key TEXT NOT NULL REFERENCES client_ops.qms_tag(tag_key),
    kind TEXT NOT NULL CHECK (kind IN ('label_proximity', 'sentence_pattern')),
    -- label_proximity: {"label": "...", "position": "before"|"after"|"inside", "max_gap_chars": 40}
    -- sentence_pattern: {"template": "the ___ day of ___, 20___", "captures": ["day","month","year"]}
    pattern JSONB NOT NULL,
    -- A match against this pattern always needs human confirmation
    -- (tier <= 2) regardless of how confident the match itself is,
    -- because applying it means rewriting surrounding prose rather than
    -- filling an isolated blank -- e.g. collapsing "the ___ day of
    -- ___, 20___" into "{{m.indate}}". A locked property of the pattern,
    -- not computed per-match.
    requires_rewrite BOOLEAN NOT NULL DEFAULT false,
    is_active BOOLEAN NOT NULL DEFAULT true,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE client_ops.tag_pattern ENABLE ROW LEVEL SECURITY;

-- Same posture as qms_tag's CURRENT policies (widened in
-- 20260808130000 from admin-only to all three client_ops-adjacent
-- roles) -- reference data, readable by any authenticated caller,
-- mutable by admin/onboarding_manager/department_manager. No admin UI
-- or HTTP endpoint yet (migration-seeded for now, same trajectory
-- qms_tag itself started on), so this reuses client_ops.manage_tags'
-- role set directly rather than adding a new permission with no
-- endpoint to gate.
CREATE POLICY tag_pattern_select_authenticated ON client_ops.tag_pattern
    FOR SELECT
    USING (NULLIF(current_setting('app.current_user_id', true), '') IS NOT NULL);

CREATE POLICY tag_pattern_insert_client_ops_roles ON client_ops.tag_pattern
    FOR INSERT
    WITH CHECK (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );
CREATE POLICY tag_pattern_update_client_ops_roles ON client_ops.tag_pattern
    FOR UPDATE
    USING (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    )
    WITH CHECK (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );
CREATE POLICY tag_pattern_delete_client_ops_roles ON client_ops.tag_pattern
    FOR DELETE
    USING (
        auth.current_user_has_role('admin')
        OR auth.current_user_has_role('onboarding_manager')
        OR auth.current_user_has_role('department_manager')
    );

-- Shape validation belongs on the tag itself, not the pattern library --
-- it's a property of what a tag's VALUE looks like once captured, used
-- by the recognizer to sanity-check a candidate regardless of which
-- pattern found it. NULL means no shape check applies.
ALTER TABLE client_ops.qms_tag
    ADD COLUMN value_shape TEXT
    CHECK (value_shape IN ('phone', 'email', 'zip'));

UPDATE client_ops.qms_tag SET value_shape = 'phone'
    WHERE tag_key IN ('e.phone', 'e.a.phone', 'e.m.cophone', 'f.phone', 'l.vi.lhpn', 'm.opi.lhpn');
UPDATE client_ops.qms_tag SET value_shape = 'email'
    WHERE tag_key IN ('e.email', 'e.a.email', 'f.email', 'c.email');
UPDATE client_ops.qms_tag SET value_shape = 'zip'
    WHERE tag_key IN ('e.post', 'e.zip', 'e.a.post', 'e.a.zip', 'f.post');
