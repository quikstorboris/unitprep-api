#!/usr/bin/env bash
# Starts the WHOLE local dev stack: test-db, api-dev and the alloy log shipper.
#
# Why this exists: alloy lives in its own compose profile ("logs"), so the
# obvious `docker compose --profile dev up -d` starts everything EXCEPT the
# shipper, and Grafana silently stops getting logs while ./logs keeps filling
# (2026-10-22/23: a day with no logs in Grafana). This runs the full command.
#
#   scripts/dev_up.sh            start (or leave running) everything
#   scripts/dev_up.sh --build    extra arguments go to `docker compose up -d`
#
# GRAFANA_API_KEY (the Loki write token) reaches alloy through --env-file
# interpolation; the file is never sourced and no other variable is passed on.
# It does not start the API server itself: that is still
#   docker compose exec api-dev cargo run
set -euo pipefail
cd "$(dirname "$0")/.."

if [ ! -f .env.local ]; then
    echo ".env.local not found in $(pwd): alloy would start without its Loki token." >&2
    exit 1
fi

docker compose --env-file .env.local --profile dev --profile logs up -d "$@"
echo
docker compose --profile dev --profile logs ps --format 'table {{.Service}}\t{{.State}}\t{{.Status}}'
echo
echo "Next: docker compose exec api-dev cargo run   (the API server is started by hand)"
