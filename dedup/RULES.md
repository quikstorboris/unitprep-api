# Duplicate Tenant Check — Rules Reference

This is the single place to see every detection/comparison rule this
crate (`unitprep-dedup`) currently implements, in plain English, with a
pointer to the module that actually implements it. Written for both
humans and AI assistants working on this codebase — when you add,
change, or remove a rule, **update this file in the same change**, not
as a follow-up. If this file and the code ever disagree, the code is
right and this file is stale — fix the file.

Every rule below is governed by one project-wide policy, stated once
here rather than repeated per rule: **exact match decides identity;
everything else is advisory only.** No rule in this crate ever merges
records or asserts two tenants are definitely the same person — it
only ever surfaces a candidate for a human to look at. See
`lib.rs`'s own crate-level doc comment for the same principle stated
for the crate as a whole.

## 1. Grouping — who counts as "one tenant"

**Rule**: records are grouped into one tenant by an exact match on the
`FirtLast` column, trimmed and lowercased. Nothing fuzzy here — two
misspelled variants of the same name are, by this rule alone, two
different tenants (see rule 3, typo-variant detection, for how that
gets caught separately).

**Implements**: `grouping.rs` (`group_key`, `group_records`,
`multi_unit_groups`).

## 2. Contact-mismatch detection — "flagged groups"

**Rule**: within one tenant (2+ units, same exact name key), compare
every known field (phone, email, address, alternate contact, company
name, name) across all their units. Blank-vs-filled counts as a
mismatch, not a match — an incomplete record is a real problem, not a
non-issue. Fields are grouped into categories, checked in a fixed
priority order (phone → email → address → alt contact → company →
name) to decide which category leads the note when more than one
differs — but *every* differing category gets described in the note
now (see rule 6), not just the lead one.

**Special case**: if the *only* difference is that every unit has a
non-blank email and all of them are mutually distinct (no format/`@`
validation — just present and different), the note says "these may be
separate tenants" instead of "please fix this" — a shared name with
genuinely different emails is a real thing (two different people, same
name) and shouldn't be presented as an error.

**Implements**: `comparison.rs` (`find_differing_categories`,
`contact_info_matches`), `types/fields.rs` (`FieldCategory`,
`FIELD_SPECS`, `CATEGORY_PRIORITY`), `report.rs` (`flag_groups`).

## 3. Typo/name-variant detection

**Rule**: compare every pair of *different-key* tenants' display names
(`"FirstName LastName"`) for similarity — `max(straight ratio,
token-sort ratio)`, where the token-sort ratio alphabetically sorts
each name's words before comparing (catches transposed first/last
names, e.g. "TED BEACH" vs "BEACH TED", that a straight ratio scores
low purely due to word order). Anything scoring **0.85 or above**
(`VARIANT_SURFACE_THRESHOLD`) is surfaced.

**Policy note**: unlike the original reference script (which
auto-merges anything ≥0.90 directly into its output), this crate
**never auto-merges, at any ratio** — every candidate above threshold
is surfaced identically, for human confirmation. Whether the two
tenants' other contact info already matches only changes the note's
*wording* (confirms vs. flags a discrepancy), never whether the pair
gets surfaced at all.

**Runs over every tenant**, including single-unit ones — two
single-unit tenants can be the same person under two misspelled keys
just as easily as two multi-unit ones.

**Implements**: `similarity.rs` (`name_similarity`,
`VARIANT_SURFACE_THRESHOLD`), `report.rs`
(`find_typo_variant_candidates`).

## 4. Related-tenant detection (added 2026-07-17)

**Rule**: flag two or more *different-key* tenants who share one
specific, non-blank identifying value, despite having no name
similarity at all. This catches a real pattern neither rule 1 nor rule
3 can ever find, since both hinge entirely on name — a business and
its owner, family members, a subdivided unit, none of which need to
share anything about their *name*.

Four signals, each independent:
- **Shared phone number** — the same phone number (primary or
  alternate-contact) appears on two different tenants.
- **Shared email address** — same, for email.
- **Shared alternate-contact identity** — two different primary
  tenants list the *same person by name* as their alternate contact
  (even if that person's own phone/email differs or is blank between
  the two listings).
- **Shared home address** — the same full street address (street +
  city + state + postal, not just city) appears on two different
  tenants, via either their primary or alternate-contact address.

**Guardrails, deliberately conservative**:
- A blank value never counts as "shared" — two tenants both having an
  empty phone field is not a match.
- A value connecting **more than 3 distinct tenants** is excluded
  entirely (`MAX_CLUSTER_SIZE`) — a value that popular is far more
  likely a shared office number or a generic mailing address than a
  real relationship between that many specific people.
- A blank street address is never treated as a real address to
  compare, even if city/state/postal are shared — otherwise two
  unrelated tenants merely in the same city would falsely "share an
  address."
- Reuses the exact same normalization already used everywhere else in
  this crate (`normalization.rs`) — no second, independently-drifting
  comparison logic.

**Explicitly rejected as a trigger**: bare unit-number adjacency (e.g.
81F/81G/81H). Real-world signal, observed at least once, but far too
weak *on its own* — it doesn't require any of the four signals above,
so it was deliberately not implemented as a standalone check. If a
future finding happens to also be in adjacent units, that's noted as
supporting context in a human summary, never as its own trigger.

**Implements**: `relatedness.rs` (`find_related_tenant_candidates`),
`report.rs` (wired in alongside rule 3).

## Normalization rules (used by rules 2 and 4)

- **Plain fields** (email, names): lowercase + trim.
- **Phone fields**: reduced to digits only (all other characters
  stripped), so `"(831) 555-1234"`, `"831-555-1234"`, and `"8315551234"`
  all compare equal regardless of formatting.
- **Address fields**: periods stripped *first* (so `"P.O. Box"` and
  `"PO Box"` both collapse to `"po box"` — stripping other punctuation
  before periods was a real, fixed bug), then remaining punctuation
  replaced with spaces, then each word run through a street-suffix/
  direction abbreviation table (`"Avenue"` → `"ave"`, `"North"` →
  `"n"`, etc.) so equivalent-but-differently-written addresses compare
  equal.

**Implements**: `normalization.rs`.

## Export (CSV/XLSX) — presentation, not detection

Both exported formats have the same three sections, in this order,
each blank-row separated: flagged groups (rule 2), typo/name variants
(rule 3), related tenants (rule 4). Flagged-group and typo-variant
notes also get spreadsheet-style cell references appended
(`"AlternateContactPhoneNumber: T7=..., T8=..."`) computed from the
source document's own column layout — related-tenant notes don't get
this, since their evidence ("this value matched somewhere among this
tenant's fields") doesn't point at one well-defined cell the way a
field mismatch does.

CSV and XLSX are two independent *writers* over one shared, format-
agnostic **export plan** — this exists specifically so the two formats
can never silently drift apart the way two independent row/column
implementations would. The plan (row ordering, which row carries the
note, cell references, cluster boundaries for XLSX's background-color
coding) is computed once and handed to both writers unchanged.

**Implements** (all in the binary, not this crate — export format is
deliberately an API-layer concern, not domain logic):
- `src/infrastructure/dedup_export_plan.rs` — the shared plan (`PlannedRow`,
  `build_export_plan`) and its `cell_refs` submodule (col-letter math,
  `first_cell_ref` for XLSX's hyperlink target).
- `src/infrastructure/dedup_csv_export.rs` — CSV writer over the plan.
- `src/infrastructure/dedup_xlsx_export.rs` — XLSX writer over the same
  plan; adds per-cluster background color and a clickable hyperlink on
  each note to its first cited cell.

## Explicitly considered and NOT implemented

Recorded here so a future session doesn't re-litigate these from
scratch — each was a real idea, each has a concrete reason it's not
(yet) a rule:

- **Company-name cross-reference** — checking whether one tenant's
  `CompanyName` contains a different tenant's personal name (hinting at
  an owner+business relationship). Real pattern, but higher
  false-positive risk with common surnames and meaningfully more
  complex fuzzy matching than the four signals above. Deferred, not
  rejected — a candidate for a future, carefully-scoped pass.
- **Cross-pull diffing** ("did this tenant's data change since the
  last time we checked this facility") — needs real persistence, which
  doesn't exist yet. Tabled until a database conversation happens.
- **Facility-internal name markers** (e.g. trailing asterisks like
  `"SMITH****"` used as an operational flag, not a typo) — the
  typo-variant logic (rule 3) already correctly merges these with the
  unmarked name when contact info matches, but this crate has no rule
  that *interprets* the marker — and shouldn't guess at what it means.
  A caution for whoever presents results to a client, not a detection
  rule.

## Adding a new rule

1. Pick the right module: a genuinely new *kind* of signal gets its
   own module (see `relatedness.rs` next to `similarity.rs` and
   `comparison.rs` — one module per kind of rule, not one shared
   "rules" file or config). A refinement of an *existing* signal
   (a new field, a new normalization case) extends the existing
   module instead.
2. Note text goes through the `NoteComposer` trait
   (`note_composer.rs`) — add a new trait method if the new rule needs
   a genuinely different note shape, don't bypass the trait with a
   one-off formatting function.
3. Wire it into `DedupReport` (`report.rs`) and, if it should appear
   in the export, into `dedup_export_plan.rs` — the shared plan both
   `dedup_csv_export.rs` and `dedup_xlsx_export.rs` consume (the binary
   side). Wiring it into just one writer directly would silently leave
   it out of the other format.
4. Update this file, in the same change, not after.
5. Thresholds/caps are Rust constants declared next to the logic that
   uses them (see `VARIANT_SURFACE_THRESHOLD` in `similarity.rs`,
   `MAX_CLUSTER_SIZE` in `relatedness.rs`) — not a config file. This is
   deliberate: a typo'd field name or a bad threshold value is a
   compile error this way, not a silent runtime misconfiguration.
