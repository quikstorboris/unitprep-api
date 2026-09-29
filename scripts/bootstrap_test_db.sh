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
set -euo pipefail

cd "$(dirname "$0")/.."

TEST_DB_URL="postgres://postgres:postgres@127.0.0.1:5433/unitprep_test"

echo "==> Waiting for test-db to be reachable..."
for _ in $(seq 1 30); do
    if PGPASSWORD=postgres psql -h 127.0.0.1 -p 5433 -U postgres -d unitprep_test -c 'SELECT 1' > /dev/null 2>&1; then
        break
    fi
    sleep 1
done

echo "==> 1/3 creating app_service role (if missing)"
# A "role neondb_owner does not exist" error here is expected and
# harmless on a local (non-Neon) Postgres -- see this script's own
# note above. Anything else printed is real; this step intentionally
# doesn't hide output or swallow its exit code.
PGPASSWORD=postgres psql -h 127.0.0.1 -p 5433 -U postgres -d unitprep_test \
    -f scripts/setup_app_service_role.sql

echo "==> 2/3 applying migrations"
DATABASE_URL="$TEST_DB_URL" sqlx migrate run

echo "==> 3/3 applying app_service grants (schemas now exist)"
PGPASSWORD=postgres psql -h 127.0.0.1 -p 5433 -U postgres -d unitprep_test \
    -f scripts/setup_app_service_role.sql

echo
echo "test-db ready. Run an #[ignore]'d test with, e.g.:"
echo "  docker compose exec -e DATABASE_URL='postgres://postgres:postgres@test-db:5432/unitprep_test' api-dev cargo test -- --ignored <name>"
