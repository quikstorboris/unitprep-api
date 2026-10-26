#!/usr/bin/env python3
"""Read-only anomaly check for UnitPrep: Sentry, the local server logs,
dependency advisories (cargo audit, npm audit, dated exceptions), CI health
(GitHub Actions) and whether the checkouts / running server are stale.

Run it with:  python3 scripts/anomaly_check.py [--hours 14] [--include-resolved]
              [--deps-only] [--skip-deps]

It changes nothing (no git writes beyond `git fetch`, no Sentry writes). It
prints a plain-text report and ends with two machine-readable lines:

    ANOMALIES=<n>
    STATUS=ALL_CLEAR | ATTENTION

plus an informational `WARNINGS=<n>` line for the dev stack state (is the
API answering, are the test-db / api-dev / alloy containers running). Warnings
are NEVER counted as anomalies, never change STATUS, and nothing acts on them
or notifies anyone: they exist so a stopped server or log shipper is visible
in the report instead of silent (Boris, 2026-10-26).

An anomaly is: an UNRESOLVED Sentry issue in either project, an ERROR log
line in the window, an HTTP 5xx in the window, a dependency vulnerability
(`cargo audit` / `npm audit`), an advisory exception past its review date, a
failed latest GitHub Actions run on a branch or tag, or a check that could
not run (blind). (A panic is also an ERROR
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
import urllib.request

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


NVM_PREFIX = '. "$HOME/.nvm/nvm.sh" >/dev/null 2>&1; nvm use default >/dev/null 2>&1; '


def bash(script, cwd=None, timeout=180):
    """Run a snippet through bash with nvm's Node first on PATH (a bare `npm`
    in WSL can resolve to the Windows one)."""
    return sh(["bash", "-c", NVM_PREFIX + script], cwd=cwd, timeout=timeout)


def check_dependencies(today, since_unused=None):
    """Dependency advisories, dated advisory exceptions and CI health.
    Returns (anomaly_count, lines_printed). Prints as it goes."""
    n = 0
    print("== DEPENDENCIES AND ADVISORIES")

    # -- cargo audit (api). Exceptions in .cargo/audit.toml are already applied by cargo audit.
    code, out = sh([os.path.expanduser("~/.cargo/bin/cargo"), "audit", "--json"], cwd=ROOT, timeout=240)
    try:
        report = json.loads(out[out.index("{"):])
        found = report["vulnerabilities"]["list"]
        warn = report.get("warnings", {})
    except (ValueError, KeyError):
        report = None
    if report is None:
        n += 1
        print("api cargo audit: !! could not run or parse (BLIND): %s" % out.strip()[:200])
    else:
        print("api cargo audit: %d vulnerabilit%s" % (len(found), "y" if len(found) == 1 else "ies"))
        for v in found:
            n += 1
            adv, pkg = v.get("advisory", {}), v.get("package", {})
            print("  - %s %s %s | %s" % (adv.get("id"), pkg.get("name"), pkg.get("version"), str(adv.get("title"))[:110]))
        print("  information only: %s" % (", ".join("%d %s" % (len(v), k) for k, v in warn.items()) or "no warnings"))

    # -- dated exceptions in .cargo/audit.toml ("REVIEW by YYYY-MM-DD")
    try:
        with open(os.path.join(ROOT, ".cargo", "audit.toml"), encoding="utf-8") as fh:
            toml_text = fh.read()
    except OSError:
        toml_text = ""
    reviews = re.findall(r"REVIEW by (\d{4}-\d{2}-\d{2})", toml_text)
    for d in sorted(set(reviews)):
        due = dt.date.fromisoformat(d)
        left = (due - today).days
        if left < 0:
            n += 1
            print("  !! advisory exception in .cargo/audit.toml is %d day(s) PAST its review date (%s)" % (-left, d))
        else:
            print("  advisory exception review date %s (in %d day(s))%s" % (d, left, "  <- due soon" if left <= 14 else ""))
    if not reviews:
        print("  no dated advisory exceptions")

    # -- npm audit (ui)
    if os.path.isdir(UI_ROOT):
        code, out = bash("npm audit --json 2>/dev/null", cwd=UI_ROOT, timeout=180)
        try:
            j = json.loads(out[out.index("{"):])
            meta = j["metadata"]["vulnerabilities"]
            vulns = j.get("vulnerabilities", {})
        except (ValueError, KeyError):
            n += 1
            print("ui npm audit: !! could not run or parse (BLIND): %s" % out.strip()[:200])
            vulns = None
        if vulns is not None:
            total = meta.get("total", 0)
            print("ui npm audit: %d vulnerabilit%s (%s)" % (
                total, "y" if total == 1 else "ies",
                ", ".join("%d %s" % (meta.get(k, 0), k) for k in ("critical", "high", "moderate", "low") if meta.get(k, 0)) or "none"))
            for name, v in sorted(vulns.items()):
                n += 1
                titles = [x.get("title") for x in v.get("via", []) if isinstance(x, dict)]
                print("  - %s (%s) fix: %s | %s" % (name, v.get("severity"), "yes" if v.get("fixAvailable") else "NONE yet",
                                                  "; ".join(t for t in titles[:2] if t)[:140]))
        # Installed vs locked: `npm audit` reads package-lock.json, so a checkout whose
        # node_modules is older than its lockfile (after a pull) shows "0 vulnerabilities"
        # while the dev server still runs the vulnerable code. `npm ls` reports that.
        code, out = bash("npm ls --depth=0 --json 2>/dev/null", cwd=UI_ROOT, timeout=120)
        try:
            problems = json.loads(out[out.index("{"):]).get("problems", [])
        except (ValueError, KeyError):
            problems = None
        if problems is None:
            n += 1
            print("ui node_modules: !! could not run `npm ls` (BLIND)")
        elif problems:
            n += 1
            print("ui node_modules: !! %d package(s) installed differ from package-lock.json "
                  "(run `npm ci` in unitprep-ui, then restart the dev server):" % len(problems))
            for pr in problems[:6]:
                print("  - %s" % pr[:140])
        else:
            print("ui node_modules: match package-lock.json")
        code, out = bash("node -e 'console.log(require(\"next/package.json\").version)'; npm view next version 2>/dev/null", cwd=UI_ROOT, timeout=60)
        vers = [l.strip() for l in out.splitlines() if re.match(r"^\d+\.\d+\.\d+", l.strip())]
        if len(vers) >= 2:
            note = "" if vers[0] == vers[1] else "  <- newer release exists (information; `npm audit` decides whether it is a security fix)"
            print("  Next.js installed %s, latest %s%s" % (vers[0], vers[1], note))
            # `npm ls` accepts any version inside package.json's range, so also compare Next
            # (the one that carries our internet-facing risk) with the version the lockfile pins.
            try:
                with open(os.path.join(UI_ROOT, "package-lock.json"), encoding="utf-8") as fh:
                    locked = json.load(fh)["packages"]["node_modules/next"]["version"]
                if locked != vers[0]:
                    n += 1
                    print("  !! Next.js installed %s but package-lock.json pins %s (run `npm ci` in unitprep-ui, "
                          "then restart the dev server)" % (vers[0], locked))
            except (OSError, KeyError, ValueError):
                pass
    else:
        print("ui: checkout not found at %s" % UI_ROOT)

    # -- CI health (GitHub Actions) for both repos
    gh = shutil.which("gh") or os.path.expanduser("~/.local/bin/gh")
    for name, path in (("api", ROOT), ("ui", UI_ROOT)):
        if not os.path.isdir(path):
            continue
        code, out = sh([gh, "run", "list", "--limit", "12", "--json",
                        "status,conclusion,headBranch,displayTitle,createdAt,workflowName"], cwd=path, timeout=60)
        try:
            runs = json.loads(out)
        except ValueError:
            n += 1
            print("%s CI: !! could not read GitHub Actions (BLIND): %s" % (name, out.strip()[:160]))
            continue
        latest = {}
        for r in runs:  # newest first, so setdefault keeps the newest per branch/tag
            latest.setdefault(r.get("headBranch"), r)
        bad = [r for r in latest.values() if r.get("conclusion") in ("failure", "timed_out", "startup_failure")]
        running = [r for r in latest.values() if r.get("status") != "completed"]
        print("%s CI: latest run per branch/tag: %d ok, %d failed, %d still running" % (
            name, len(latest) - len(bad) - len(running), len(bad), len(running)))
        for r in bad:
            n += 1
            print("  - FAILED %s on %s (%s) | %s" % (r.get("workflowName"), r.get("headBranch"),
                                                     str(r.get("createdAt"))[:16], str(r.get("displayTitle"))[:80]))
    print()
    return n


def check_stack_state(last_request):
    """Warnings only: is the local dev stack actually up?

    Prints WARNING lines and returns how many there were. These are never
    counted as anomalies, never change STATUS, and nothing acts on them: a
    dead process cannot log that it is dead, so this looks from the outside.
    `last_request` is the newest "request completed" log time (or None).
    """
    print("== DEV STACK STATE (warnings only; nothing acts on these)")
    warnings = 0
    try:
        with urllib.request.urlopen("http://127.0.0.1:8080/health", timeout=4) as resp:
            answering = resp.status == 200
    except Exception:  # noqa: BLE001 - refused, reset, timeout: all mean "not answering"
        answering = False
    if answering:
        print("API server: answering on http://127.0.0.1:8080/health")
    else:
        warnings += 1
        print("WARNING: the API server is not answering on http://127.0.0.1:8080/health "
              "(not running, or still starting). Start it with `docker compose exec api-dev cargo run`.")
    code, out = sh(["docker", "compose", "ps", "-a", "--format", "{{.Service}}|{{.State}}"], cwd=ROOT)
    if code != 0:
        warnings += 1
        print("WARNING: could not ask Docker about the containers: %s" % out.strip()[:160])
    else:
        states = dict(line.split("|", 1) for line in out.splitlines() if "|" in line)
        for svc in ("test-db", "api-dev", "alloy"):
            state = states.get(svc)
            if state == "running":
                print("container %s: running" % svc)
                continue
            warnings += 1
            print("WARNING: container %s is %s." % (svc, state or "missing"))
            if svc == "alloy":
                print("         The log shipper is in its own compose profile: until it runs, Grafana gets no new logs "
                      "while ./logs keeps filling. Start everything with scripts/dev_up.sh.")
    if last_request is not None:
        age = (dt.datetime.now(dt.timezone.utc) - last_request).total_seconds() / 3600
        print("last request seen in the logs: %s UTC (%.1f h ago)" % (last_request.strftime("%Y-%m-%d %H:%M:%S"), age))
    else:
        print("last request seen in the logs: none in the local files")
    print()
    return warnings


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hours", type=float, default=14.0, help="look-back window for the log part (default 14)")
    ap.add_argument("--include-resolved", action="store_true", help="also list resolved Sentry issues (informational)")
    ap.add_argument("--deps-only", action="store_true", help="only the dependency / CI section (fast, for hand use)")
    ap.add_argument("--skip-deps", action="store_true", help="skip the dependency / CI section")
    a = ap.parse_args()
    now = dt.datetime.now(dt.timezone.utc)
    since = now - dt.timedelta(hours=a.hours)
    anomalies = 0

    print("UnitPrep anomaly check")
    print("now      : %s UTC  (%s local)" % (now.strftime("%Y-%m-%d %H:%M:%S"), dt.datetime.now().strftime("%a %Y-%m-%d %H:%M:%S")))
    print("log window: last %.1f h (since %s UTC)\n" % (a.hours, since.strftime("%Y-%m-%d %H:%M")))

    if a.deps_only:
        anomalies += check_dependencies(dt.date.today())
        print("ANOMALIES=%d" % anomalies)
        print("STATUS=%s" % ("ALL_CLEAR" if anomalies == 0 else "ATTENTION"))
        return 0

    # ---------------- Sentry ----------------
    print("== SENTRY (org %s)" % ORG)
    code, out = sh([sentry_bin(), "auth", "status"])
    exp = re.search(r"Access token expires\s*[|│]\s*([^|│\n]+)", out)
    print("auth: %s%s" % ("OK" if code == 0 and "Authenticated" in out else "PROBLEM",
                          ("  (token expires %s)" % exp.group(1).strip()) if exp else ""))
    if exp:
        try:
            left = (parse_ts(exp.group(1).strip().replace(" ", "T", 1)) - now).days
            if left is not None and left < 7:
                anomalies += 1
                print("  !! the Sentry CLI login expires in %d day(s); renew it (`sentry auth login`)" % left)
        except (TypeError, ValueError):
            pass
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
        # Named limiter (api >= 1.9.135); the older hard-coded "ClickUp rate limiter full" text was
        # also what the Process Street pacer logged, so pre-1.9.135 lines cannot be split by source.
        limiter_waits = [r for r in win if "rate limiter full" in str(r.get("fields", {}).get("message", "")).lower()]
        cu = [r for r in limiter_waits if r.get("fields", {}).get("limiter") == "clickup"]
        psw = [r for r in limiter_waits if r.get("fields", {}).get("limiter") == "process_street"]
        unnamed = len(limiter_waits) - len(cu) - len(psw)
        ps429 = [r for r in win if "rate limit reached" in str(r.get("fields", {}).get("message", ""))]
        print("information only (known-benign, not counted): %d pool before_acquire warning(s), %d slow statement(s), "
              "%d ClickUp limiter wait(s), %d Process Street pacer wait(s), %d unlabelled limiter wait(s) "
              "(older logs), %d Process Street 429 wait(s)" % (len(ba), len(slow), len(cu), len(psw), unnamed, len(ps429)))
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

    last_request = max(
        (r["_t"] for r in rows if r.get("fields", {}).get("message") == "request completed"), default=None)
    warnings = check_stack_state(last_request)

    if not a.skip_deps:
        anomalies += check_dependencies(dt.date.today())

    print("ANOMALIES=%d" % anomalies)
    print("STATUS=%s" % ("ALL_CLEAR" if anomalies == 0 else "ATTENTION"))
    print("WARNINGS=%d" % warnings)
    return 0


if __name__ == "__main__":
    sys.exit(main())
