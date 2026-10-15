#!/usr/bin/env bash
# Measures line coverage of the whole workspace -- the default tests PLUS the
# DB-backed #[ignore]'d tests (the same selection scripts/run_ci_db_tests.sh
# runs) -- and prints it per area. Run by hand; not part of preflight (an
# instrumented build is slow). Coverage is a map of where the tests are thin,
# not a target: see the vault's "Test Audit 2026-10-14".
#
# Needs: cargo-llvm-cov (`cargo install cargo-llvm-cov --locked`), docker, and
# a local ephemeral test-db with every migration applied. Uses a throwaway
# database of its own (unitprep_cov), never the shared dev/prod Neon branches:
#
#   docker compose --profile test up -d test-db
#   ./scripts/coverage.sh
#
# Output: a per-area table, the files with the most uncovered lines, and the
# full JSON at $COVERAGE_JSON (default /tmp/unitprep_coverage.json).
set -euo pipefail

cd "$(dirname "$0")/.."

container="${TEST_DB_CONTAINER:-unitprep-api-test-db-1}"
db="${TEST_DB_NAME:-unitprep_cov}"
json="${COVERAGE_JSON:-/tmp/unitprep_coverage.json}"

if ! docker ps --format '{{.Names}}' | grep -qx "$container"; then
    echo "test-db container '$container' is not running." >&2
    echo "Start it with: docker compose --profile test up -d test-db" >&2
    exit 1
fi

echo "==> fresh database $db (the test-db is ephemeral and local; Neon is never touched)"
docker exec "$container" psql -U postgres -q -c "drop database if exists $db" -c "create database $db"
TEST_DB_NAME="$db" ./scripts/bootstrap_test_db.sh > /dev/null

export TEST_DATABASE_URL="postgres://app_service:app_service@127.0.0.1:5433/$db"

echo "==> instrumented run 1/2: default tests"
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --no-report

echo "==> instrumented run 2/2: DB-only ignored tests"
names=$(sed -n '/^DB_ONLY_IGNORED_TESTS=(/,/^)/p' scripts/run_ci_db_tests.sh | grep -E '^\s+[a-z_0-9]+$' | tr -d ' ')
# shellcheck disable=SC2086
cargo llvm-cov --workspace --no-report -- --ignored _db_ $names

echo "==> report"
cargo llvm-cov report --workspace --summary-only --json --output-path "$json"
python3 scripts/coverage_by_area.py "$json"
