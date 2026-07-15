#!/usr/bin/env python3
"""Fail when a GitHub workflow executes an external action by a mutable ref."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

USES_RE = re.compile(r"^\s*(?:-\s*)?uses:\s*([^\s#]+)")
PINNED_RE = re.compile(r"^[^/@\s]+/[^@\s]+@[0-9a-fA-F]{40}$")


def violations(workflows: Path) -> list[str]:
    failures: list[str] = []
    paths = sorted((*workflows.glob("*.yml"), *workflows.glob("*.yaml")))
    if not paths:
        return [f"no workflow files found under {workflows}"]
    for path in paths:
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            match = USES_RE.match(line)
            if not match:
                continue
            reference = match.group(1)
            if reference.startswith(("./", "docker://")):
                continue
            if not PINNED_RE.fullmatch(reference):
                failures.append(f"{path}:{line_number}: mutable external action: {reference}")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--workflows",
        type=Path,
        default=Path(__file__).resolve().parents[2] / ".github" / "workflows",
    )
    args = parser.parse_args()
    failures = violations(args.workflows)
    if failures:
        print("GitHub Action pin policy failed:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    print("GitHub Action pin policy passed: every external uses: ref is a 40-hex commit.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
