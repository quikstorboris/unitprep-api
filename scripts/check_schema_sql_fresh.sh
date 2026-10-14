#!/usr/bin/env bash
# Cheap drift guard for SCHEMA.sql: its header records how many migrations
# it was generated from; if migrations/ has more (or fewer) up-files than
# that, a migration landed without ./scripts/regenerate_schema_sql.sh being
# re-run. Needs no Docker and no database, so it runs in preflight and in
# the fast CI job. It cannot see an edit to an existing migration -- that
# is a rewrite of history and should not happen once a migration is applied.
set -euo pipefail

cd "$(dirname "$0")/.."

actual="$(ls migrations/*.up.sql | wc -l | tr -d ' ')"
recorded="$(sed -nE 's/^-- directory -- the ([0-9]+) incremental migrations.*/\1/p' SCHEMA.sql | head -1)"

if [ -z "$recorded" ]; then
    echo "check_schema_sql_fresh: could not read the migration count from SCHEMA.sql's header." >&2
    exit 1
fi

if [ "$actual" != "$recorded" ]; then
    echo "FAILED: SCHEMA.sql was generated from $recorded migrations but migrations/ has $actual." >&2
    echo "Run ./scripts/bootstrap_test_db.sh then ./scripts/regenerate_schema_sql.sh and commit SCHEMA.sql." >&2
    exit 1
fi

echo "OK: SCHEMA.sql matches the $actual migrations."
