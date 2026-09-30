#!/usr/bin/env bash
# Fails if unitprep-ui's committed types/generated/*.ts have drifted from
# what ts-rs generates from this repo's Rust structs. Generates into a
# temp dir and diffs -- never writes into the UI repo. To fix a failure,
# run `npm run generate-types` in unitprep-ui and commit the result.
#
# Usage: scripts/check_ts_bindings.sh [path-to-unitprep-ui]
#   Default UI path is the sibling checkout, ../unitprep-ui (the standing
#   assumption both repos are developed under). Exits 0 with a SKIPPED
#   note if that checkout is absent, so preflight works in a lone clone.
#
# README.md and index.ts in the UI folder are hand-written, so they are
# excluded. A generated file present only in the UI (a stale leftover
# after a struct was removed) or only here (a new export never copied
# over) IS drift and fails.
#
# Covers only structs marked #[ts(export)]; see the vault's CI Backlog
# and CI-CD Framework notes.
set -euo pipefail

cd "$(dirname "$0")/.."

ui_dir="${1:-../unitprep-ui}"
generated="$ui_dir/types/generated"

if [ ! -d "$generated" ]; then
    echo "SKIPPED: $generated not found -- no UI checkout to compare against."
    exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

TS_RS_EXPORT_DIR="$tmp" cargo test --workspace export_bindings >"$tmp/cargo.log" 2>&1 || {
    echo "FAILED: binding generation itself failed:"
    tail -30 "$tmp/cargo.log"
    exit 1
}
rm -f "$tmp/cargo.log"

if diff -r -x README.md -x index.ts "$tmp" "$generated"; then
    echo "OK: $(find "$tmp" -maxdepth 1 -name '*.ts' | wc -l) generated type file(s) match $generated."
else
    echo
    echo "FAILED: types/generated has drifted from the Rust structs (left = freshly generated, right = committed)."
    echo "        Run 'npm run generate-types' in unitprep-ui and commit the result."
    exit 1
fi
