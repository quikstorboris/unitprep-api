#!/usr/bin/env python3
"""Read-only anomaly check for UnitPrep: Sentry, the local server logs, and
whether the checkouts / running server are stale.

Run it with:  python3 scripts/anomaly_check.py [--hours 14] [--include-resolved]

It changes nothing (no git writes beyond `git fetch`, no Sentry writes). It
prints a plain-text report and ends with two machine-readable lines:

    ANOMALIES=<n>
    STATUS=ALL_CLEAR | ATTENTION

An anomaly is: an UNRESOLVED Sentry issue in either project, an ERROR log
line in the window, or an HTTP 5xx in the window. (A panic is also an ERROR
line, so it is not counted twice.) Known-benign patterns (see the vault note
"Routine Monitoring and Troubleshooting Playbook") are reported as
INFORMATION only and never counted.

Used by the twice-daily scheduled routine and by hand. Needs the Sentry CLI
(`~/.local/bin/sentry`, OAuth login) for the Sentry part; if it cannot reach
Sentry the report says so and counts that as an anomaly (a blind check is not
"all clear").
"""
import argparse
import datetime as dt
import glob
import json
import os
import re
import shutil
import subprocess
import sys

ORG = "quikstor-wf"
PROJECTS = ["orchestrator", "orchestrator-ui"]
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UI_ROOT = os.path.join(os.path.dirname(ROOT), "unitprep-ui")


def sh(args, cwd=None, timeout=90):
    try:
        r = subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, (r.stdout or "") + (r.stderr or "")
    except Exception as e:  # noqa: BLE001
        return 99, "%s: %s" % (type(e).__name__, e)


def sentry_bin():
    return shutil.which("sentry") or os.path.expanduser("~/.local/bin/sentry")


def parse_ts(s):
    s = (s or "").replace("Z", "+00:00")
    try:
        t = dt.datetime.fromisoformat(s)
        return t if t.tzinfo else t.replace(tzinfo=dt.timezone.utc)
    except ValueError:
        return None


def sentry_issues(project, query):
    code, out = sh([sentry_bin(), "issue", "list", "%s/%s" % (ORG, project), "--query", query,
                    "--limit", "50", "--fresh", "--json"])
    if code != 0:
        return None, out.strip()[:300]
    out = out.strip()
    if not out or out.lower().startswith("no issues"):
        return [], ""
    try:
        d = json.loads(out)
    except ValueError:
        return None, "unparseable output: " + out[:200]
    items = d if isinstance(d, list) else d.get("data", d.get("items", []))
    return items, ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hours", type=float, default=14.0, help="look-back window for the log part (default 14)")
    ap.add_argument("--include-resolved", action="store_true", help="also list resolved Sentry issues (informational)")
    a = ap.parse_args()
    now = dt.datetime.now(dt.timezone.utc)
    since = now - dt.timedelta(hours=a.hours)
    anomalies = 0

    print("UnitPrep anomaly check")
    print("now      : %s UTC  (%s local)" % (now.strftime("%Y-%m-%d %H:%M:%S"), dt.datetime.now().strftime("%a %Y-%m-%d %H:%M:%S")))
    print("log window: last %.1f h (since %s UTC)\n" % (a.hours, since.strftime("%Y-%m-%d %H:%M")))

    # ---------------- Sentry ----------------
    print("== SENTRY (org %s)" % ORG)
    code, out = sh([sentry_bin(), "auth", "status"])
    exp = re.search(r"Access token expires\s*[|│]\s*([^|│\n]+)", out)
    print("auth: %s%s" % ("OK" if code == 0 and "Authenticated" in out else "PROBLEM",
                          ("  (token expires %s)" % exp.group(1).strip()) if exp else ""))
    if code != 0 or "Authenticated" not in out:
        anomalies += 1
        print("  !! Sentry CLI is not authenticated; this part of the check is BLIND:", out.strip()[:200])
    for p in PROJECTS:
        items, err = sentry_issues(p, "is:unresolved")
        if items is None:
            anomalies += 1
            print("%s: !! could not read Sentry: %s" % (p, err))
            continue
        print("%s: %d unresolved issue(s)" % (p, len(items)))
        for i in items:
            anomalies += 1
            first = parse_ts(i.get("firstSeen"))
            new = " NEW IN WINDOW" if first and first >= since else ""
            print("  - %s | events %s | first %s | last %s%s" % (
                i.get("shortId"), i.get("count"), str(i.get("firstSeen"))[:19], str(i.get("lastSeen"))[:19], new))
            print("      %s" % str(i.get("title"))[:160])
            if i.get("culprit"):
                print("      culprit: %s" % str(i.get("culprit"))[:120])
        if a.include_resolved:
            res, err = sentry_issues(p, "is:resolved")
            for i in res or []:
                print("  (resolved, information) %s | events %s | last %s | %s" % (
                    i.get("shortId"), i.get("count"), str(i.get("lastSeen"))[:19], str(i.get("title"))[:80]))
    print()

    # ---------------- local logs ----------------
    print("== SERVER LOG FILES (%s/logs)" % ROOT)
    rows = []
    for f in sorted(glob.glob(os.path.join(ROOT, "logs", "unitprep-api.*.log"))):
        try:
            with open(f, encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    try:
                        r = json.loads(line)
                    except ValueError:
                        continue
                    t = parse_ts(r.get("timestamp"))
                    if t:
                        r["_t"] = t
                        rows.append(r)
        except OSError:
            pass
    if not rows:
        anomalies += 1
        print("!! no readable log lines at all (is LOG_DIR set / is the server running?)")
    else:
        last = max(r["_t"] for r in rows)
        starts = [r["_t"] for r in rows if "listening on" in r.get("fields", {}).get("message", "")]
        print("newest log line: %s UTC (%.1f h ago)" % (last.strftime("%Y-%m-%d %H:%M:%S"), (now - last).total_seconds() / 3600))
        if starts:
            print("server last started: %s UTC (%.1f h ago)" % (max(starts).strftime("%Y-%m-%d %H:%M:%S"), (now - max(starts)).total_seconds() / 3600))
        win = [r for r in rows if r["_t"] >= since]
        reqs = [r for r in win if r.get("fields", {}).get("message") == "request completed"]
        errs = [r for r in win if r.get("level") == "ERROR"]
        five = [r for r in reqs if isinstance(r.get("fields", {}).get("status"), int) and r["fields"]["status"] >= 500]
        print("in window: %d lines, %d request(s), %d ERROR, %d HTTP 5xx" % (len(win), len(reqs), len(errs), len(five)))
        if not reqs:
            print("  (no requests in the window: the app was idle, so a quiet log is expected)")
        for r in errs[:15]:
            f = r.get("fields", {})
            anomalies += 1
            kind = "PANIC" if ("panic" in f or "panicked" in str(f.get("message", ""))) else "ERROR"
            print("  - %s %s %s | %s | %s" % (r["_t"].strftime("%m-%d %H:%M:%S"), kind, r.get("target"),
                                              str(f.get("message"))[:90], str(f.get("error", f.get("panic", "")))[:140]))
        if len(errs) > 15:
            anomalies += len(errs) - 15
            print("  ... %d more ERROR lines not shown" % (len(errs) - 15))
        for r in five[:15]:
            sp = r.get("span", {})
            anomalies += 1
            print("  - %s HTTP %s %s %s (request_id %s)" % (r["_t"].strftime("%m-%d %H:%M:%S"), r["fields"]["status"],
                                                           sp.get("method"), sp.get("path"), sp.get("request_id")))
        if len(five) > 15:
            anomalies += len(five) - 15
        # information only (known-benign patterns, never counted)
        ba = [r for r in win if "before_acquire" in str(r.get("fields", {}).get("message", ""))]
        slow = [r for r in win if "slow statement" in str(r.get("fields", {}).get("message", ""))]
        cu = [r for r in win if "ClickUp rate limiter full" in str(r.get("fields", {}).get("message", ""))]
        ps429 = [r for r in win if "rate limit reached" in str(r.get("fields", {}).get("message", ""))]
        print("information only (known-benign, not counted): %d pool before_acquire warning(s), %d slow statement(s), "
              "%d ClickUp limiter wait(s), %d Process Street 429 wait(s)" % (len(ba), len(slow), len(cu), len(ps429)))
    print()

    # ---------------- freshness ----------------
    print("== CHECKOUT / SERVER FRESHNESS (information)")
    for name, path in (("unitprep-api", ROOT), ("unitprep-ui", UI_ROOT)):
        if not os.path.isdir(path):
            print("%s: checkout not found at %s" % (name, path))
            continue
        sh(["git", "fetch", "-q", "origin", "--tags"], cwd=path)
        _, head = sh(["git", "rev-parse", "--short", "HEAD"], cwd=path)
        _, behind = sh(["git", "rev-list", "--count", "HEAD..origin/main"], cwd=path)
        _, ahead = sh(["git", "rev-list", "--count", "origin/main..HEAD"], cwd=path)
        _, dirty = sh(["git", "status", "--porcelain"], cwd=path)
        print("%s: HEAD %s | behind origin/main by %s | ahead by %s | uncommitted files: %d" % (
            name, head.strip(), behind.strip(), ahead.strip(), len([l for l in dirty.splitlines() if l.strip()])))
    if rows and starts:
        _, ct = sh(["git", "log", "-1", "--format=%cI"], cwd=ROOT)
        ctime = parse_ts(ct.strip())
        if ctime and ctime > max(starts):
            print("note: the checkout's newest commit (%s) is NEWER than the running server's last start; "
                  "the server is on older code (cargo run does not reload)." % ct.strip()[:19])
    print()

    print("ANOMALIES=%d" % anomalies)
    print("STATUS=%s" % ("ALL_CLEAR" if anomalies == 0 else "ATTENTION"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
