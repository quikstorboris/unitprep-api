# Server logs: where they go and how to read them

`unitprep-api` writes structured logs. In dev they end up in three places:

| Where | What | Kept |
|---|---|---|
| The terminal running `cargo run` | Human-readable text | Until the terminal closes |
| `./logs/unitprep-api.YYYY-MM-DD.log` (set by `LOG_DIR`, see `src/startup/logging.rs`) | JSON, one object per line | Last 7 daily files |
| Grafana Cloud Loki (shipped by the `alloy` compose service, `dev-tools/alloy/config.alloy`) | The same JSON | Per the Grafana plan (trial ends 2026-10-30; permanent limits unverified) |

Run the shipper with: `docker compose --env-file .env.local --profile dev --profile logs up -d`.
A running server only picks up logging/code changes after it is restarted (`cargo run` does not reload).

## Reading logs

**Grafana UI** (https://joyfulpelican1551.grafana.net): Explore with `{job="unitprep-api"}`, Drilldown > Logs, or the
"UnitPrep API Logs" dashboard (source: `dev-tools/grafana/unitprep-api-logs.dashboard.json`; re-import after changes).
Alerts "UnitPrep API panic" and "UnitPrep API errors" email the `email-boris` contact point.

**Terminal, `logcli`** (installed at `~/.local/bin/logcli`, wrapper `scripts/logcli.sh`):

```bash
scripts/logcli.sh query '{job="unitprep-api"}' --since=1h --limit=50
scripts/logcli.sh query '{job="unitprep-api", level="ERROR"}' --since=24h
scripts/logcli.sh query '{job="unitprep-api"} |= "request completed" | json | fields_status >= 500' --since=1h
```

**Plain `curl`** against the Loki HTTP API (what `logcli` wraps), reading the token from `.env.local` without sourcing it:

```bash
TOKEN=$(grep -m1 '^GRAFANA_LOGS_READ_TOKEN=' .env.local | cut -d= -f2-)
curl -s -u "1836339:$TOKEN" -G https://logs-prod-021.grafana.net/loki/api/v1/query_range \
  --data-urlencode 'query={job="unitprep-api", level="ERROR"}' --data-urlencode 'since=1h' --data-urlencode 'limit=20'
```

**Claude Code, the `grafana` MCP server** (registered in the repo-local `.mcp.json`, started by `scripts/grafana-mcp.sh`):
[Grafana's official MCP server](https://github.com/grafana/mcp-grafana) at `~/.local/bin/mcp-grafana`. Lets an AI session
query Loki, list dashboards and alert rules. Runs **read-only** (`--disable-write`), with only the search, datasource,
loki, alerting, dashboard, folder and navigation tool groups, and with its anonymous usage statistics turned off.
Claude Code asks you to approve a new project MCP server the first time a session starts in this repo.

**The files themselves**: `./logs/*.log` (JSON lines), readable with `jq` or `grep`; the last 7 days only.

## One-time token setup (two tokens, both read-only, both go in `.env.local`)

The write token Alloy uses (`GRAFANA_API_KEY`) cannot read. Create these yourself in the Grafana portal and paste each
straight into `.env.local`, never into chat:

1. **`GRAFANA_LOGS_READ_TOKEN`**, for `logcli` and `curl`. grafana.com > your org > Security > Access Policies > New
   access policy; realm: the `joyfulpelican1551` stack; scope: **logs: Read** only; then Add token (set an expiry).
2. **`GRAFANA_SERVICE_ACCOUNT_TOKEN`**, for the MCP server. Open the stack
   (https://joyfulpelican1551.grafana.net) > Administration > Users and access > Service accounts > Add service account;
   role **Viewer**; then Add service account token (set an expiry). It starts with `glsa_`.

Add them as plain lines: `GRAFANA_LOGS_READ_TOKEN=...` and `GRAFANA_SERVICE_ACCOUNT_TOKEN=...`. Tokens expire; when one
does, make a new one and replace the line. Delete any token that was ever pasted into a chat.

## Installed tools and versions

| Tool | Version | Source | Path |
|---|---|---|---|
| `logcli` | 3.7.8 | github.com/grafana/loki release `logcli-linux-amd64.zip` (SHA256 verified) | `~/.local/bin/logcli` |
| `mcp-grafana` | 2.0.2 | github.com/grafana/mcp-grafana release `mcp-grafana_Linux_x86_64.tar.gz` (SHA256 verified) | `~/.local/bin/mcp-grafana` |

Upgrade by downloading the newer release asset the same way and replacing the file.

## When this moves to a cloud host

Nothing here depends on where the server runs: the Loki address, user and tokens are the same. Only the *shipping*
side changes (see the vault note "UnitPrep Fly.io and Deployment Notes"). Prod logs should carry `env=prod`; add
`env="prod"` to the queries above.
