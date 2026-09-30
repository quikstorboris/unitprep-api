#!/usr/bin/env bash
# Makes the Docker Phase 1 ephemeral test-db (docker-compose.yml's
# test-db service) actually ready to run real-DB #[ignore]'d tests
# against -- not just "up and healthy" (which docker-compose's own
# healthcheck already confirms), but migrated and role-configured.
#
# Needed because test-db is genuinely ephemeral by design (tmpfs, no
# named volume for its data dir -- see docker-compose.yml's own
# comment on why): every time it's recreated, it comes back
# completely empty, with no migrations and no app_service role. This
# script re-applies both, idempotently, so bringing up a fresh,
# ready-to-use test-db is one command instead of a fragile multi-step
# sequence that's easy to forget or get wrong (found the hard way,
# 2026-09-29, verifying Docker Phase 2 -- see the vault's CI-CD
# Framework doc and Session log for the full story).
#
# Order matters and is non-obvious -- see
# scripts/setup_app_service_role.sql's own header comment for why the
# role script has to run both before AND after migrations.
#
# Also sets a real (but throwaway, local-only) password on app_service
# -- the shared role-setup script deliberately never does this itself
# ("a role with no password set cannot authenticate at all, which is
# intentional" -- correct for real Neon branches, where a human sets
# the real password by hand). Without this, TEST_DATABASE_URL has no
# way to authenticate as app_service and ends up connecting as the
# postgres superuser instead, which silently bypasses every row-level-
# security policy -- tests were passing without actually proving RLS
# held at all.
#
# Host/port/credentials are all overridable (env vars below, each with
# the Docker Phase 1 default) rather than hardcoded, specifically so
# this same script also works unchanged against GitHub Actions' service
# containers (a different host/port), not just the local test-db --
# one script, two callers, instead of near-duplicate logic in two
# places.
set -euo pipefail

cd "$(dirname "$0")/.."

PGHOST="${TEST_DB_HOST:-127.0.0.1}"
PGPORT="${TEST_DB_PORT:-5433}"
PGSUPERUSER="${TEST_DB_SUPERUSER:-postgres}"
PGSUPERPASS="${TEST_DB_SUPERUSER_PASSWORD:-postgres}"
APP_SERVICE_PASSWORD="${TEST_DB_APP_SERVICE_PASSWORD:-app_service}"
DBNAME="${TEST_DB_NAME:-unitprep_test}"

TEST_DB_URL="postgres://${PGSUPERUSER}:${PGSUPERPASS}@${PGHOST}:${PGPORT}/${DBNAME}"

echo "==> Waiting for test-db to be reachable at ${PGHOST}:${PGPORT}..."
for _ in $(seq 1 30); do
    if PGPASSWORD="$PGSUPERPASS" psql -h "$PGHOST" -p "$PGPORT" -U "$PGSUPERUSER" -d "$DBNAME" -c 'SELECT 1' > /dev/null 2>&1; then
        break
    fi
    sleep 1
done

echo "==> 1/4 creating app_service role (if missing)"
# A "role neondb_owner does not exist" error here is expected and
# harmless on a local (non-Neon) Postgres -- see this script's own
# note above. Anything else printed is real; this step intentionally
# doesn't hide output or swallow its exit code.
PGPASSWORD="$PGSUPERPASS" psql -h "$PGHOST" -p "$PGPORT" -U "$PGSUPERUSER" -d "$DBNAME" \
    -f scripts/setup_app_service_role.sql

echo "==> 2/4 applying migrations"
DATABASE_URL="$TEST_DB_URL" sqlx migrate run

echo "==> 3/4 applying app_service grants (schemas now exist)"
PGPASSWORD="$PGSUPERPASS" psql -h "$PGHOST" -p "$PGPORT" -U "$PGSUPERUSER" -d "$DBNAME" \
    -f scripts/setup_app_service_role.sql

echo "==> 4/4 setting a local-only password on app_service"
# Throwaway -- not a real secret, same reasoning as test-db's own
# postgres/postgres credentials. Idempotent: safe to re-run against an
# already-configured role.
PGPASSWORD="$PGSUPERPASS" psql -h "$PGHOST" -p "$PGPORT" -U "$PGSUPERUSER" -d "$DBNAME" \
    -c "ALTER ROLE app_service PASSWORD '${APP_SERVICE_PASSWORD}';"

echo
echo "test-db ready. Run an #[ignore]'d test with, e.g.:"
echo "  docker compose exec api-dev cargo test -- --ignored <name>"
echo "(TEST_DATABASE_URL is already set in docker-compose.yml, connecting as app_service -- RLS applies, same as production.)"
