#!/usr/bin/env bash
# Query the UnitPrep server logs in Grafana Cloud Loki from the terminal.
# Thin wrapper over `logcli` (installed at ~/.local/bin/logcli): it supplies the
# Loki address, user and a READ token, so the command line stays short.
#
#   scripts/logcli.sh query '{job="unitprep-api"}' --since=1h --limit=50
#   scripts/logcli.sh query '{job="unitprep-api", level="ERROR"}' --since=24h
#   scripts/logcli.sh query '{job="unitprep-api"} |= "request completed"' --since=15m -o raw
#
# Needs GRAFANA_LOGS_READ_TOKEN in .env.local: a Grafana Cloud access-policy
# token with the `logs:read` scope ONLY (not the logs:write one Alloy uses).
# Extracts just that one key; never `source`s .env.local. See docs/LOGS.md.
set -euo pipefail
cd "$(dirname "$0")/.."

env_val() {
    grep -m1 "^$1=" .env.local 2>/dev/null | cut -d= -f2- \
        | sed -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//" || true
}

TOKEN=$(env_val GRAFANA_LOGS_READ_TOKEN)
if [ -z "$TOKEN" ]; then
    echo "GRAFANA_LOGS_READ_TOKEN is not set in .env.local. See docs/LOGS.md (\"One-time token setup\")." >&2
    exit 1
fi

export LOKI_ADDR="https://logs-prod-021.grafana.net"
export LOKI_USERNAME="1836339"
export LOKI_PASSWORD="$TOKEN"
exec "$HOME/.local/bin/logcli" "$@"
