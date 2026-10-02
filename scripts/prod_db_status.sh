#!/usr/bin/env bash
# Read-only: shows the prod DB branch latest applied migration and which local migrations it is missing. Run by hand (never CI).
set -euo pipefail
cd ~/Development/unitprep-api
# Extract only the prod direct URL; never `source` .env.local.
URL=$(grep -m1 '^NEON_PROD_DATABASE_URL_DIRECT=' .env.local | cut -d= -f2- | sed -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//")
psql "$URL" -At -c "select max(version), count(*) from _sqlx_migrations where success"
echo "--- local migration versions after prod max:"
MAXV=$(psql "$URL" -At -c "select max(version) from _sqlx_migrations where success")
ls migrations/*.up.sql | sed 's#.*/##; s#_.*##' | awk -v m="$MAXV" '$1>m' | tr '\n' ' '
echo
psql "$URL" -At -c "select version from _sqlx_migrations where not success"
