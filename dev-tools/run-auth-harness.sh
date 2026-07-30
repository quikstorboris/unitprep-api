#!/usr/bin/env bash
# Dev-only. Starts the API and a static server for auth-test.html with
# matching WebAuthn origin settings, then waits. Ctrl-C stops both.
#
#   bash dev-tools/run-auth-harness.sh
#
# Then open http://localhost:3200/auth-test.html in a Windows browser
# (Chrome or Edge) -- NOT a WSL browser, since the passkey prompt has to
# reach Windows Hello. WSL2 forwards localhost, so both ports resolve.
set -euo pipefail
cd "$(dirname "$0")/.."

HARNESS_PORT="${HARNESS_PORT:-3200}"
API_PORT="${API_PORT:-8080}"
ORIGIN="http://localhost:${HARNESS_PORT}"

# Every one of these matters, and three of them are the difference between
# "works" and "fails with an opaque browser error":
#
#   WEBAUTHN_RP_ORIGIN   must equal the page's origin exactly. The browser
#                        checks this itself and refuses before the server is
#                        involved.
#   WEBAUTHN_RP_ID       must be a domain suffix of that origin's host. Port
#                        is not part of it, so plain "localhost" is right.
#   CORS_ALLOWED_ORIGINS the API defaults to :3000 and :5173 only, so the
#                        harness origin has to be added or every fetch fails
#                        preflight.
#   SESSION_COOKIE_SECURE=false  the cookies default to Secure, which a
#                        browser silently drops over plain http. The ceremony
#                        would then "work" while losing its cookie between
#                        the two requests.
#
# There is deliberately no AUTH_BOOTSTRAP_ENABLED here any more. Phase 2 task 6
# deleted that variable along with the unauthenticated path it gated, so
# exporting it would do nothing except mislead the next reader. Enrolling a
# first passkey now needs an invite token pasted into the harness page --
# printed by `unitprep bootstrap-admin` (or its --reissue-invite), and once
# task 7 lands, issued by an admin.
export WEBAUTHN_RP_ID="localhost"
export WEBAUTHN_RP_ORIGIN="$ORIGIN"
export CORS_ALLOWED_ORIGINS="$ORIGIN"
export SESSION_COOKIE_SECURE="false"
export PORT="$API_PORT"

echo "building..."
cargo build --workspace 2>&1 | tail -1

# Same-site note: localhost:3200 -> localhost:8080 is CROSS-ORIGIN (different
# port) but SAME-SITE (site is scheme+registrable domain, port excluded), so
# the SameSite=Lax cookies are still sent. That is why this works at all
# without SameSite=None.
./target/debug/unitprep > /tmp/harness-api.log 2>&1 &
API_PID=$!

python3 -m http.server "$HARNESS_PORT" --directory dev-tools --bind 0.0.0.0 \
  > /tmp/harness-static.log 2>&1 &
WEB_PID=$!

cleanup() {
  echo
  echo "stopping..."
  kill "$API_PID" "$WEB_PID" 2>/dev/null || true
  wait "$API_PID" "$WEB_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Wait for BOTH ports. Waiting only on the API is a real race: it usually
# wins the startup race against python, so the script would announce a
# harness URL that is not listening yet and the first click would fail
# with a connection error for no visible reason.
wait_for() {
  local url="$1" label="$2"
  for _ in $(seq 1 60); do
    if curl -s --max-time 1 -o /dev/null "$url" 2>/dev/null; then return 0; fi
    sleep 0.25
  done
  echo "WARNING: $label did not come up at $url" >&2
  return 1
}
wait_for "http://127.0.0.1:${API_PORT}/health" "API"
wait_for "http://127.0.0.1:${HARNESS_PORT}/auth-test.html" "static server"

echo
echo "  API      http://localhost:${API_PORT}   (log: /tmp/harness-api.log)"
echo "  harness  ${ORIGIN}/auth-test.html"
echo
echo "  rp_id=${WEBAUTHN_RP_ID}  rp_origin=${WEBAUTHN_RP_ORIGIN}"
echo -n "  db: "; curl -s "http://127.0.0.1:${API_PORT}/health/db"; echo
echo
echo "Open the harness URL in a WINDOWS browser. Ctrl-C here to stop both."
echo
echo "--- API log (live) ---"
tail -f /tmp/harness-api.log
