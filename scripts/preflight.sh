#!/usr/bin/env bash
# Tier 0 of the UnitPrep CI/CD framework -- see the vault's
# reference/UnitPrep CI-CD Framework.md for the full design and why
# each step exists. Run this before every push, not every commit
# (matches this project's own "batch, don't checkpoint" cadence).
#
# Deliberately does NOT run anything under `#[ignore]` -- those are
# real-DB integration tests against the live Neon dev branch, and this
# script must never spend Neon compute automatically. Run those by
# hand, deliberately, pre-release: `cargo test -- --ignored <name>`.
set -euo pipefail

cd "$(dirname "$0")/.."

fail=0

step() {
    echo
    echo "==> $1"
}

step "1/8 cargo fmt --check"
if ! cargo fmt --check; then
    echo "FAILED: run 'cargo fmt' to fix."
    fail=1
fi

step "2/8 cargo clippy --workspace --all-targets -- -D warnings"
if ! cargo clippy --workspace --all-targets -- -D warnings; then
    echo "FAILED: fix the clippy warnings above."
    fail=1
fi

step "3/8 cargo test --workspace (fast suite only -- #[ignore]'d real-DB tests are skipped by design)"
if ! cargo test --workspace; then
    echo "FAILED: fix the failing tests above."
    fail=1
fi

step "4/8 cargo audit (dependency vulnerability scan)"
# Exits non-zero only for actual vulnerabilities, not for
# unmaintained/yanked advisory-grade warnings -- those print but don't
# block a push. Bump the offending crate (or its dependent) to clear a
# real hit; see the vault's CI-CD Framework doc for why this stays
# blocking rather than advisory.
if ! cargo audit; then
    echo "FAILED: a real vulnerability was found above -- upgrade the affected crate before pushing."
    fail=1
fi

step "5/8 gitleaks (real secret scan, diff-scoped)"
# Only scans commits about to be pushed, not the whole history --
# matches the grep backstop below's scope. Known false positives go in
# .gitleaks.toml's allowlist, never a blanket disable.
gitleaks_range="$(git merge-base HEAD origin/main 2>/dev/null || echo HEAD)"
if command -v gitleaks >/dev/null 2>&1; then
    if ! gitleaks git --log-opts="${gitleaks_range}..HEAD"; then
        echo "FAILED: gitleaks found a likely secret above -- review before pushing."
        fail=1
    fi
else
    echo "SKIPPED: gitleaks not installed -- see the vault's CI-CD Framework doc for the install step."
fi

step "6/8 version/tag consistency (advisory, does not block a push)"
current_version=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/\1/')
latest_tag=$(git describe --tags --abbrev=0 2>/dev/null || echo "")
if [ -n "$latest_tag" ]; then
    tag_version="${latest_tag#v}"
    commits_since_tag=$(git rev-list "${latest_tag}..HEAD" --count 2>/dev/null || echo "0")
    if [ "$commits_since_tag" != "0" ] && [ "$current_version" = "$tag_version" ]; then
        echo "NOTE: $commits_since_tag commit(s) since $latest_tag, but Cargo.toml is still at $current_version."
        echo "      If any of those are real code changes (not docs-only), bump the version before releasing."
        echo "      This is advisory only -- a docs-only push is expected to look like this."
    else
        echo "OK: version $current_version vs. latest tag $latest_tag (${commits_since_tag} commits since)."
    fi
else
    echo "No tags found yet -- skipping."
fi

step "7/8 secret-pattern scan (grep-based backstop, redundant with gitleaks above by design)"
# Deliberately narrow and low-false-positive: private key headers, a
# handful of well-known cloud-provider key prefixes, and an assignment
# to something that looks like a password/secret/api key with a
# literal (not env-var-referencing) value. See the framework doc for
# why this stays advisory-grade until gitleaks/trufflehog is actually
# installed.
diff_range="$(git merge-base HEAD origin/main 2>/dev/null || echo HEAD)..HEAD"
secret_hits=$(git diff "$diff_range" -- . ':!*.lock' 2>/dev/null | grep -E '^\+' | grep -iE \
    -e '-----BEGIN [A-Z ]*PRIVATE KEY-----' \
    -e 'AKIA[0-9A-Z]{16}' \
    -e '(password|secret|api_key|apikey)\s*[:=]\s*"[^"$][^"]{7,}"' \
    || true)
if [ -n "$secret_hits" ]; then
    echo "POSSIBLE SECRET FOUND in the diff about to be pushed:"
    echo "$secret_hits"
    echo "FAILED: review the lines above before pushing. If this is a false positive, push anyway with git push --no-verify (if hooked) or just re-run without this script."
    fail=1
else
    echo "OK: no obvious secret patterns found."
fi

step "8/8 workflow secret/permissions guard (CI isolation control #1)"
# No GitHub workflow may reference a secret, a NEON_* name or a bare
# DATABASE_URL, and each must declare least-privilege permissions.
if ! ./scripts/check_workflow_secrets.sh; then
    fail=1
fi

echo
if [ "$fail" -ne 0 ]; then
    echo "preflight FAILED -- fix the issues above before pushing."
    exit 1
fi
echo "preflight passed."
