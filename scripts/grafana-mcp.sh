#!/usr/bin/env bash
# Starts Grafana's official MCP server (stdio) for Claude Code, READ-ONLY.
# Registered in the repo-local .mcp.json as the "grafana" server. It lets an
# AI session search dashboards, list alert rules and query Loki logs through
# Grafana, without a browser.
#
# Needs GRAFANA_SERVICE_ACCOUNT_TOKEN in .env.local: a token for a Grafana
# *service account* with the Viewer role, created inside the stack (not a Cloud
# access-policy token). Extracts just that key; never `source`s .env.local.
# --disable-write blocks every create/update tool; --enabled-tools drops the
# groups we do not use; --usage-stats disabled turns off the server's anonymous
# telemetry to Grafana Labs. See docs/LOGS.md.
set -euo pipefail
cd "$(dirname "$0")/.."

env_val() {
    grep -m1 "^$1=" .env.local 2>/dev/null | cut -d= -f2- \
        | sed -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//" || true
}

TOKEN=$(env_val GRAFANA_SERVICE_ACCOUNT_TOKEN)
if [ -z "$TOKEN" ]; then
    echo "GRAFANA_SERVICE_ACCOUNT_TOKEN is not set in .env.local. See docs/LOGS.md (\"One-time token setup\")." >&2
    exit 1
fi

export GRAFANA_URL="https://joyfulpelican1551.grafana.net"
export GRAFANA_SERVICE_ACCOUNT_TOKEN="$TOKEN"
exec "$HOME/.local/bin/mcp-grafana" \
    --disable-write \
    --enabled-tools "search,datasource,loki,alerting,dashboard,folder,navigation" \
    --max-loki-log-limit 500 \
    --usage-stats disabled
