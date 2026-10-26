# Server logs: where they go and how to read them

`unitprep-api` writes structured logs. In dev they end up in three places:

| Where | What | Kept |
|---|---|---|
| The terminal running `cargo run` | Human-readable text | Until the terminal closes |
| `./logs/unitprep-api.YYYY-MM-DD.log` (set by `LOG_DIR`, see `src/startup/logging.rs`) | JSON, one object per line | Last 7 daily files by default; set `LOG_RETENTION_DAYS` to change it |
| Grafana Cloud Loki (shipped by the `alloy` compose service, `dev-tools/alloy/config.alloy`) | The same JSON | Set by the Grafana plan, not by this repo: the Free plan keeps **14 days** and allows 50 GB ingested per month (verified on Grafana's pricing page 2026-10-22; Pro is 30 days) |

Run the shipper (and the rest of the dev stack) with **`scripts/dev_up.sh`**, which runs `docker compose --env-file .env.local --profile dev --profile logs up -d`. The shipper has `restart: unless-stopped`, so it also comes back by itself after a Docker/WSL restart. `python3 scripts/anomaly_check.py` ends its report with a **warnings-only** "DEV STACK STATE" section (API answering on :8080? containers running? last request seen) and a `WARNINGS=n` line; warnings never count as anomalies and nothing acts on or notifies about them.
A running server only picks up logging/code changes after it is restarted (`cargo run` does not reload).

**Log-file retention**: `LOG_RETENTION_DAYS` (environment variable, read at startup) is the number of daily log files kept in `LOG_DIR`. Default **7**; accepted values are whole numbers from **1 to 365**. Anything else (empty, `0`, negative, text, over 365) prints one warning line to stderr and keeps the default. It counts daily *files*, not calendar days: a day with no log writes creates no file, so the files kept can span more than that many days. Pruning happens when a new daily file is created, and a restart is needed to change the value. Example: `LOG_RETENTION_DAYS=14 cargo run`; for the dev container set it under `api-dev` in `docker-compose.yml` next to `LOG_DIR`.

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

**The files themselves**: `./logs/*.log` (JSON lines), readable with `jq` or `grep`; the last 7 daily files unless `LOG_RETENTION_DAYS` says otherwise.

## Alert emails (Grafana alerting, contact point `email-boris`)

**Decision 2026-10-16: Sentry owns error and panic emails; Grafana is for logs and log-derived health (5xx rate, latency).**
The two rules below ("UnitPrep API errors", "UnitPrep API panic") are RETIRED: pause or delete them in Grafana so each error
does not email twice. They are kept here only as a recipe. The two active Grafana rules are under "Health alerts" further down.

Alert rules live in Grafana, not in this repo; their definitions are recorded here so they can be recreated (for
example for a prod stack). Both evaluate every minute in folder `GrafanaCloud`, group `unitprep-api`, no pending period,
no-data = OK, notify `email-boris`.

**"UnitPrep API errors"**: fires once per distinct error, so the email says *which* one. Query (instant, 5 m):

```
sum by (target, fields_message, span_method, span_path, err) (count_over_time({job="unitprep-api", level="ERROR"} | json | label_format err=`{{ trunc 140 .fields_error }}` [5m]))
```

Condition: above 0. Annotations: `summary` = `{{ $labels.fields_message }} on {{ $labels.span_method }} {{ $labels.span_path }}`;
`description` = occurrence count, module (`$labels.target`), the error text (`$labels.err`) and a note that in the email's
Values section **A is the number of ERROR lines in the window and C=1 means firing**; `runbook_url` = the dashboard;
dashboard + panel link = "UnitPrep API Logs" / "Errors and warnings" (`__dashboardUid__`, `__panelId__`=6).

**"UnitPrep API panic"**: same shape, one instance per distinct panic. Query (instant, 5 m):

```
sum by (target, panic_info, span_method, span_path) (count_over_time({job="unitprep-api", level="ERROR"} |= "panicked" | json | label_format panic_info=`{{ trunc 200 .fields_panic }}` [5m]))
```

Condition above 0; `summary` = `PANIC: {{ $labels.panic_info }}`; `description` = count, module, request, a one-line explanation of
what a panic is, and what A and C mean; same runbook and dashboard-panel link. The `panic` field is the Rust panic message
plus its source location (the panic hook in `src/startup/logging.rs` logs `panic = %panic_info`). Not yet seen firing: there
have been no panics; the query returns no data over 24 h and the label template was checked on real error lines.

The contact point's email subject is the template
`[UnitPrep {{ .Status }}] {{ .CommonLabels.alertname }}{{ with .CommonAnnotations.summary }}: {{ . }}{{ end }}`.
Grafana's alert email is a fixed layout: the content (labels, annotations, values) can be shaped as above, but its
HTML and logo cannot be replaced from the contact point.

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

## Sentry (error reporting)

Errors and panics also go to Sentry (org `quikstor-wf`, project `orchestrator`) when `SENTRY_DSN` is set in `.env.local`
(the DSN is a public ingest key, not a secret). Code: `src/startup/sentry.rs`; the tracing hookup is in
`src/startup/logging.rs` and the per-request layers are in `src/api/router/mod.rs`. Set `SENTRY_ENVIRONMENT=production`
on the prod host. Check the wiring with
`SENTRY_DSN=... cargo test sends_a_test_event -- --ignored`, then read the issue with `sentry issue list quikstor-wf/orchestrator`.
Never log a tenant value in a `tracing::error!` field: field text is sent to Sentry unredacted.

Sentry emails (set up 2026-10-16, via the Sentry CLI, both projects): "new or regressed issue" and "error spike (10+ events
in 1 hour)", sent to Boris. They currently fire for every environment, dev included. **When prod exists, restrict them to
`environment:production`** (`sentry alert issues edit quikstor-wf/<project>/<rule-id> --environment production`).

## Health alerts to create in Grafana (5xx rate, p95 latency)

Not created from the repo (Grafana alert rules live in Grafana, and the tokens here are read-only). Create both by hand in
folder `GrafanaCloud`, group `unitprep-api`, evaluate every minute, no-data = OK, notify `email-boris`, same email template
as above. Both group by `env`, so one rule covers dev and prod. A "no logs at all" alert is deliberately NOT created until
there is a live host (an idle dev server would fire it constantly).

**"UnitPrep API 5xx rate"**, instant query over 5 m, condition above 5:

```
sum by (env) (count_over_time({job="unitprep-api"} |= "request completed" | json | fields_status >= 500 [5m]))
```

**"UnitPrep API p95 latency"**, instant query over 5 m, condition above 2000 (ms):

```
quantile_over_time(0.95, {job="unitprep-api"} |= "request completed" | json | unwrap fields_latency_ms [5m]) by (env)
```

Thresholds are a first guess; tune them once real traffic exists. Dev's debug build is slower than a release build (see
`CLAUDE.md`), so expect a higher p95 in dev than prod.

## Correlating Grafana logs with Sentry issues

Every request has a `request_id` (the `x-request-id` header). It is a field on every log line and a tag on every Sentry
event, so either tool leads to the other:

- Sentry issue -> logs: copy the `request_id` tag, then in Grafana Explore run
  `{job="unitprep-api"} | json | request_id="<id>"`.
- Logs -> Sentry: in Sentry search `request_id:<id>`.

Also on every request span (so on every log line from inside a request, as `span_<name>` in `| json`) and as Sentry tags:
`client_id` and `facility_id` (read off the URL path; UUIDs only), and, once a session resolves, `user_id` and `roles`
(role keys such as `admin`). IDs only: never a name, email or phone. Code: `src/api/router/request_context.rs` and
`record_request_identity` in `src/auth/authenticated_user.rs`.

## The `env` label

Every shipped log line carries `env` (a Loki label): `dev` by default, or whatever `LOG_ENV` is set to for the Alloy
service (`LOG_ENV=prod` on the prod host's shipper). The dashboard has an Environment picker that filters every panel by it.
Add `env="prod"` to queries and alerts when you want only prod.
