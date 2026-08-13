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

**Special case**: the note says "these may be separate tenants"
instead of "please fix this" only when Email **and** Address are the
*only two* differing categories, and each is non-blank and mutually
distinct across every unit. Both conditions are required — a shared
name with a genuinely different email *and* a genuinely different
home address looks like two different people who happen to share a
name, not one person with a typo. **Email differing alone is not
enough**: since a category only appears as "differing" when it
actually differs, "Email is the only entry" by itself would mean every
other field — including address — already *matches*, which is exactly
the same-person-typo'd-their-email case, not the separate-tenants one.
This was a real, shipped bug (fixed 2026-08-10, caught on real client
data): the old condition checked only "Email differs and all emails
distinct," which could only ever fire in the same-person case,
never the separate-tenants case it claimed to detect.

**Corroborating case**: conversely, when Email is the *only* differing
category and the address *and* phone are both present and identical
across every unit, the note explicitly names this as likely one person
with a mistyped email, rather than leaving the reader to notice the
matching address/phone unassisted.

**Implements**: `comparison.rs` (`find_differing_categories`,
`contact_info_matches`), `types/fields.rs` (`FieldCategory`,
`FIELD_SPECS`, `CATEGORY_PRIORITY`), `report.rs` (`flag_groups`),
`note_composer.rs` (`compose_group_note`), `phrasing.rs`
(`all_addresses_present_and_distinct`, `address_present_and_shared`,
`phone_present_and_shared`).

**Excluded fields**: `PhoneNumberPrefix`/`AlternateContactPhoneNumberPrefix`
are not compared at all (same treatment as `Gender`/`DateOfBirth` —
absent from `FieldName` entirely, not merely never differing). Legacy
QSX never exposed the prefix field to users, so any difference there
is migration noise, not a correctable discrepancy — matches the
reference skill's own current stance. The raw values still round-trip
in the CSV/XLSX export (`dedup_export_plan.rs`'s `COLUMNS`); only the
comparison-taxonomy layer excludes them.

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
- A placeholder value someone typed as a stand-in for "not applicable"
  instead of leaving the field blank (`n/a`, `na`, `none`, `tbd`,
  `unknown`, `n.a.`, `not applicable`, `null`, `nil`, `xxx`) never
  counts as shared either, checked against the raw trimmed+lowercased
  value before any field-kind-specific normalization runs. Added
  2026-08-10 after the literal string `"None"` in
  `AlternateContactLastName` connected four otherwise-unrelated tenants
  in real client data.
- A phone value with fewer than 10 digits after normalization never
  counts as a shared phone — a shorter value is a truncated fragment
  (an area-code stub, a partial paste), not a real number. Added
  2026-08-10 after a 3-digit `AlternateContactPhoneNumber` fragment
  (`"978"`) connected an unrelated tenant to the facility's own account
  in real client data. Scoped to this signal only — a short/garbage
  phone *difference between two units of the same tenant* is still a
  real mismatch under rule 2's blank-vs-filled-always-differs policy.
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

**Households, not one row per signal** (added 2026-08-10): every
(signal, value) cluster that passes the guardrails above is merged
into a *household* by transitive closure — two clusters that share
even one tenant belong to the same household, regardless of which
signal connected them. This is what turns e.g. a spousal pair matching
on phone, email, *and* alternate contact into one candidate naming
them once with three pieces of evidence, instead of three separate
rows each repeating the same two names; it's also what connects a
three-tenant chain (A shares a phone with B, B shares an email with C,
A and C share nothing directly) into one household instead of two
disjoint pairs a reader would have to notice are related themselves.
A household capped at more than `MAX_HOUSEHOLD_SIZE` (8) members is
excluded entirely — deliberately more generous than `MAX_CLUSTER_SIZE`
since each piece of evidence is already individually filtered, this
guards only against a pathological chain of individually-small,
individually-unremarkable clusters accreting into one implausibly
large "family."

The composed note reflects this: a household with exactly one piece of
evidence keeps the original single-signal wording; a household with
more than one groups evidence by which specific members it connects
first (so multiple signals shared by the *same* pair combine into one
clause), then joins distinct-subset clauses with "; " and a single
shared closing sentence — never repeating "A and B" once per signal.

**Explicitly rejected as a trigger**: bare unit-number adjacency (e.g.
81F/81G/81H). Real-world signal, observed at least once, but far too
weak *on its own* — it doesn't require any of the four signals above,
so it was deliberately not implemented as a standalone check. If a
future finding happens to also be in adjacent units, that's noted as
supporting context in a human summary, never as its own trigger.

**Implements**: `relatedness.rs` (`find_related_tenant_candidates`,
`RelatedTenantEvidence`), `note_composer.rs`
(`compose_relatedness_note`, `RelatednessEvidenceInput`), `report.rs`
(wired in alongside rule 3).

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
