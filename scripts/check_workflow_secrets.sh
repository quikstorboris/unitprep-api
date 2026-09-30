#!/usr/bin/env bash
# Repo-side guard for the UnitPrep CI/CD framework's isolation control #1
# (credential absence) -- see the vault's reference/UnitPrep CI-CD
# Framework.md. No GitHub workflow may receive a secret, or name a real
# database connection variable, unless this script is deliberately
# updated first: turning that on should be a visible, reviewed decision,
# never a side effect of a copy-pasted YAML snippet.
#
# Fails (exit 1) if any .github/workflows/*.yml|yaml:
#   - references secrets.<anything> other than GITHUB_TOKEN, passes
#     `secrets: inherit`, or dumps/indexes the whole secrets context
#   - mentions a NEON_* name or a bare DATABASE_URL (TEST_DATABASE_URL,
#     the ephemeral-Postgres variable, is allowed)
#   - has no top-level `permissions:` block (GITHUB_TOKEN least privilege)
#
# Full-line YAML comments are ignored; a trailing comment that trips a
# rule is a false positive that fails closed -- reword it.
set -euo pipefail

cd "$(dirname "$0")/.."

shopt -s nullglob
files=(.github/workflows/*.yml .github/workflows/*.yaml)

if [ "${#files[@]}" -eq 0 ]; then
    echo "OK: no workflow files to check."
    exit 0
fi

fail=0

for f in "${files[@]}"; do
    # Drop full-line comments but keep line numbers for useful output.
    body="$(grep -nvE '^[[:space:]]*#' "$f" || true)"

    # `secrets` must be a whole word, or "check_workflow_secrets.sh" matches.
    hits="$(printf '%s\n' "$body" | grep -E '(^|[^A-Za-z0-9_])secrets\.[A-Za-z0-9_]+' | grep -vE '(^|[^A-Za-z0-9_])secrets\.GITHUB_TOKEN([^A-Za-z0-9_]|$)' || true)"
    hits+="$(printf '%s\n' "$body" | grep -E '(^|[^A-Za-z0-9_])secrets:[[:space:]]*inherit|(^|[^A-Za-z0-9_])secrets\[|toJSON\([[:space:]]*secrets' || true)"
    if [ -n "$hits" ]; then
        echo "FAILED ($f): references a secret other than GITHUB_TOKEN:"
        echo "$hits"
        fail=1
    fi

    db_hits="$(printf '%s\n' "$body" | grep -E 'NEON_[A-Z0-9_]*|(^|[^A-Za-z0-9_])DATABASE_URL' || true)"
    if [ -n "$db_hits" ]; then
        echo "FAILED ($f): names a real database connection variable (only TEST_DATABASE_URL is allowed in CI):"
        echo "$db_hits"
        fail=1
    fi

    if ! grep -qE '^permissions:' "$f"; then
        echo "FAILED ($f): no top-level 'permissions:' block -- add 'permissions: contents: read'."
        fail=1
    fi
done

if [ "$fail" -ne 0 ]; then
    exit 1
fi
echo "OK: ${#files[@]} workflow file(s) reference no secrets, no real-DB variables, and declare permissions."
