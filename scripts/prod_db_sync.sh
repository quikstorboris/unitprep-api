#!/usr/bin/env bash
# Applies the pending unitprep-api migrations to the PROD Neon branch,
# re-applies the app_service grants, and verifies. Run by Boris, interactively.
set -euo pipefail
cd ~/Development/unitprep-api
# Extract only the prod direct URL; never `source` .env.local.
URL=$(grep -m1 '^NEON_PROD_DATABASE_URL_DIRECT=' .env.local | cut -d= -f2- | sed -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//")

echo "=== BEFORE: prod row counts (sanity)"
psql "$URL" -At -c "select 'users', count(*) from auth.users union all select 'qms_tag', count(*) from client_ops.qms_tag union all select 'vendor_format', count(*) from client_ops.vendor_format"
echo
echo "=== Pending migrations (sqlx):"
sqlx migrate info --database-url "$URL" | grep -i pending | wc -l
echo
read -r -p "Apply the pending migrations to PROD now? Type 'apply prod' to continue: " ANSWER
[ "$ANSWER" = "apply prod" ] || { echo "Aborted."; exit 1; }

echo "=== Applying (each migration runs in its own transaction; a failure stops here with earlier ones kept)"
sqlx migrate run --database-url "$URL"

echo "=== Re-applying app_service grants"
psql "$URL" -f scripts/setup_app_service_role.sql

echo "=== AFTER: verification"
psql "$URL" -At -c "select max(version), count(*) from _sqlx_migrations where success"
psql "$URL" -At -c "select 'failed migrations', count(*) from _sqlx_migrations where not success"
psql "$URL" -At -c "select 'users', count(*) from auth.users union all select 'qms_tag', count(*) from client_ops.qms_tag union all select 'vendor_format', count(*) from client_ops.vendor_format"
psql "$URL" -At -c "select 'app_service can read vendor_format', has_table_privilege('app_service','client_ops.vendor_format','SELECT')"
psql "$URL" -At -c "select 'app_service can read tool_runs', has_table_privilege('app_service','client_ops.tool_runs','SELECT')"
psql "$URL" -At -c "select 'app_service can read integrations.dropbox_configuration', has_table_privilege('app_service','integrations.dropbox_configuration','SELECT')"
psql "$URL" -At -c "select 'app_service can read clients.companies', has_table_privilege('app_service','clients.companies','SELECT')"
echo "Done."
