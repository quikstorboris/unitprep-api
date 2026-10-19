#!/usr/bin/env bash
# Runs exactly the #[ignore]'d tests that need *a* real Postgres, not
# *the* real Neon data or any real external API -- the category the
# CI/CD framework's isolation control #4 says is safe to move to an
# ephemeral service-container Postgres. Never the tests needing real
# Process Street/Dropbox access -- those stay manual, deliberately
# triggered by a human, never wired into any automated job (see the
# vault's CI-CD Framework doc).
#
# An explicit allowlist, not a name-pattern --skip: cargo test's
# built-in harness only accepts one filter substring per invocation, so
# there's no single flag that means "all ignored tests except these" --
# and a pattern guess would be fragile anyway (one of the safe tests
# below is literally named ...dropbox... despite needing no real
# Dropbox access at all, which would have silently defeated a
# "--skip dropbox" heuristic). An explicit list is also the correct
# safe-by-default shape per isolation control #5: a newly-added
# #[ignore]'d test does NOT get picked up here automatically -- adding
# it to CI is a deliberate, visible, one-line addition to this file,
# not something that happens by omission.
#
# Since 2026-10-09 there is ALSO a convention-based step at the end: every
# ignored test whose name contains `_db_` runs. By convention a `_db_` test
# needs only TEST_DATABASE_URL (the ephemeral Postgres) plus loopback mock
# servers -- never a real Process Street/Dropbox/ClickUp account. That
# trades the "a new test joins CI only by a deliberate one-line addition"
# rule for "a new `_db_` test joins automatically", on purpose: the ~100
# `_db_` tests are the only automated check on sessions, RLS, registration,
# audit rows and the ClickUp permission paths, and a hand-kept list of them
# would rot. A test that needs a real external account must NOT have `_db_`
# in its name (the live ones are named after what they hit, e.g.
# `resolves_..._real_shared_link`).
#
# Requires TEST_DATABASE_URL already set and pointing at a real,
# throwaway Postgres (see db::connect_test() -- this script doesn't
# set it, just runs tests that read it).
set -euo pipefail

cd "$(dirname "$0")/.."

# One test-name filter substring per line -- each verified unique
# enough within this workspace that a plain substring match (no
# --exact needed) can't accidentally pull in an unrelated test.
DB_ONLY_IGNORED_TESTS=(
    remaining_active_admins_excluding_serializes_concurrent_callers
    a_registration_ceremony_survives_a_simulated_process_restart_durability
    query_sessions_own_sql_is_valid_against_the_real_schema
    a_tagger_session_survives_a_simulated_process_restart_durability
    a_dedup_session_survives_a_simulated_process_restart_durability
    a_group_prep_session_survives_a_simulated_process_restart_durability
    attach_output_bytes_runs_cleanly_against_the_real_schema
    attach_output_dropbox_runs_cleanly_against_the_real_schema
    a_qsx_facilitys_empty_category_gets_permanently_exempt
    a_qsx_facilitys_already_populated_category_never_becomes_exempt
    a_non_qsx_facilitys_empty_category_never_becomes_exempt
    highway20_golden_fixture_ingests_and_round_trips_through_real_postgres
    upsert_person_and_link_to_facility_refreshes_phone_on_a_second_call_with_the_same_name
    upsert_person_and_link_to_facility_keeps_distinct_names_separate_on_a_shared_email
    heal_person_in_place_corrects_a_known_persons_own_name_and_phone
    policy_delinquency_entries_trigger_check_matches_the_apps_own_validation
    edit_person_and_facility_link_flips_source_to_manual_only_when_protected
    the_seeded_registry_classifies_real_export_headers
)

# libtest accepts several filters after `--` and runs a test matching ANY
# of them, so the allowlist and the `_db_` convention share ONE cargo
# invocation (one build/link pass and one harness start per test binary)
# instead of eighteen-plus separate ones.
echo "==> Running ${#DB_ONLY_IGNORED_TESTS[@]} allowlisted DB-only #[ignore]'d tests plus every ignored test named *_db_ (convention: local test-db only)"
fail=0
# `--skip known_gap`: tests that assert the INTENDED behaviour of a documented,
# not-yet-fixed gap stay #[ignore]d with the reason in the attribute; the
# module name of a *_db_tests file would otherwise pull them into this sweep.
if ! cargo test --workspace -- --ignored _db_ "${DB_ONLY_IGNORED_TESTS[@]}" --skip known_gap; then
    fail=1
fi

echo
if [ "$fail" -ne 0 ]; then
    echo "One or more DB-only ignored tests FAILED -- see above."
    exit 1
fi
echo "All DB-only ignored tests passed."
