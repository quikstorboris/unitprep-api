#!/usr/bin/env python3
"""Summarises a cargo-llvm-cov JSON report per area, production code only.

Usage: coverage_by_area.py <cov.json> [min_uncovered_lines_to_list]

Test files and test support code are excluded so they do not inflate the
number. Prints line coverage per area and the files with the most uncovered
lines. A map of where the tests are thin -- not a target.
"""
import collections
import json
import re
import sys

TEST_FILE = re.compile(r"(_tests?|/tests?)\.rs$|/tests/|test_support|testdata")


def area(path: str) -> str:
    parts = path.split("/")
    if parts[0] == "src":
        return "src/" + parts[1] if len(parts) > 2 else "src/(root)"
    return parts[0]


def main() -> None:
    report = json.load(open(sys.argv[1], encoding="utf8"))
    threshold = int(sys.argv[2]) if len(sys.argv) > 2 else 40

    rows = []
    for file in report["data"][0]["files"]:
        name = re.sub(r".*/unitprep-api/", "", file["filename"])
        if TEST_FILE.search(name):
            continue
        lines = file["summary"]["lines"]
        rows.append((name, lines["count"], lines["covered"]))

    areas = collections.defaultdict(lambda: [0, 0])
    for name, count, covered in rows:
        areas[area(name)][0] += count
        areas[area(name)][1] += covered

    total = sum(c for c, _ in areas.values())
    covered = sum(v for _, v in areas.values())
    print(f"\nWhole workspace, production lines: {covered}/{total} = {100 * covered / total:.1f}%\n")
    for key, (count, cov) in sorted(areas.items(), key=lambda kv: -kv[1][0]):
        print(f"  {key:26} {cov:6}/{count:6} {100 * cov / count:5.1f}%")

    print(f"\nFiles with at least {threshold} uncovered lines:")
    for name, count, cov in sorted(rows, key=lambda r: -(r[1] - r[2])):
        if count - cov >= threshold:
            print(f"  {count - cov:5} uncovered of {count:5} ({100 * cov / count:3.0f}%)  {name}")


if __name__ == "__main__":
    main()
