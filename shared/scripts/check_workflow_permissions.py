#!/usr/bin/env python3
"""Enforce a canonical, least-privilege GitHub workflow permission shape."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

APPROVED_JOB_WRITES = {
    ("pages.yml", "deploy"): {"pages", "id-token"},
    ("release.yml", "publish-release"): {"contents"},
}
PUBLISH_GATE = "needs.windows-release.outputs.publish == 'true'"
JOB_RE = re.compile(r"^  ([A-Za-z0-9_-]+):$")


def _indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _permission_block(lines: list[str], start: int, indent: int, end: int) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in lines[start + 1 : end]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        current = _indent(line)
        if current <= indent:
            break
        if current == indent + 2 and ":" in line:
            key, value = line.strip().split(":", 1)
            result[key] = value.strip().strip("'\"")
    return result


def _jobs(lines: list[str], jobs_start: int, path: Path) -> tuple[dict[str, tuple[int, int]], list[str]]:
    failures: list[str] = []
    starts: list[tuple[str, int]] = []
    block_end = len(lines)
    for index in range(jobs_start + 1, len(lines)):
        line = lines[index]
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = _indent(line)
        if indent == 0:
            block_end = index
            break
        if indent != 2:
            continue
        match = JOB_RE.fullmatch(line)
        if not match:
            failures.append(
                f"{path}:{index + 1}: job declarations must use canonical `  job-name:` block form"
            )
            continue
        starts.append((match.group(1), index))
    jobs: dict[str, tuple[int, int]] = {}
    for offset, (name, start) in enumerate(starts):
        end = starts[offset + 1][1] if offset + 1 < len(starts) else block_end
        if name in jobs:
            failures.append(f"{path}:{start + 1}: duplicate job name {name}")
        jobs[name] = (start, end)
    return jobs, failures


def violations(workflows: Path) -> list[str]:
    failures: list[str] = []
    paths = sorted((*workflows.glob("*.yml"), *workflows.glob("*.yaml")))
    if not paths:
        return [f"no workflow files found under {workflows}"]
    for path in paths:
        lines = path.read_text(encoding="utf-8").splitlines()
        if any("\t" in line for line in lines):
            failures.append(f"{path}: tabs are forbidden in workflow YAML")

        top_permission_indices = [
            index for index, line in enumerate(lines) if line == "permissions:"
        ]
        if len(top_permission_indices) != 1:
            failures.append(
                f"{path}: expected exactly one canonical top-level permissions block"
            )
        else:
            index = top_permission_indices[0]
            top_permissions = _permission_block(lines, index, 0, len(lines))
            if top_permissions != {"contents": "read"}:
                failures.append(
                    f"{path}: top-level permissions must be exactly contents: read; got {top_permissions}"
                )

        try:
            jobs_start = lines.index("jobs:")
        except ValueError:
            failures.append(f"{path}: canonical jobs block not found")
            continue
        jobs, job_failures = _jobs(lines, jobs_start, path)
        failures.extend(job_failures)
        if not jobs:
            failures.append(f"{path}: no canonical jobs found")
            continue

        for job_name, (start, end) in jobs.items():
            permission_lines = [
                index for index in range(start + 1, end)
                if _indent(lines[index]) == 4
                and lines[index].strip().startswith("permissions:")
            ]
            if len(permission_lines) > 1:
                failures.append(f"{path}:{start + 1}: job {job_name} has duplicate permissions")
                continue
            permissions: dict[str, str] = {}
            if permission_lines:
                index = permission_lines[0]
                if lines[index].strip() != "permissions:":
                    failures.append(
                        f"{path}:{index + 1}: scalar/flow permissions are forbidden for job {job_name}"
                    )
                    continue
                permissions = _permission_block(lines, index, 4, end)
            writes = {key for key, value in permissions.items() if value == "write"}
            approved = APPROVED_JOB_WRITES.get((path.name, job_name), set())
            if writes != approved or any(
                value not in {"read", "write", "none"} for value in permissions.values()
            ):
                failures.append(
                    f"{path}:{start + 1}: unapproved permissions for job {job_name}: {permissions}"
                )

        for (approved_file, approved_job), required in APPROVED_JOB_WRITES.items():
            if approved_file != path.name:
                continue
            span = jobs.get(approved_job)
            if span is None:
                failures.append(f"{path}: approved privileged job {approved_job} is missing")
                continue
            start, end = span
            permission_index = next(
                (index for index in range(start + 1, end)
                 if _indent(lines[index]) == 4 and lines[index].strip() == "permissions:"),
                None,
            )
            if permission_index is None:
                failures.append(f"{path}: privileged job {approved_job} lacks job permissions")
                continue
            actual = _permission_block(lines, permission_index, 4, end)
            if {key for key, value in actual.items() if value == "write"} != required:
                failures.append(
                    f"{path}:{permission_index + 1}: {approved_job} must grant only {sorted(required)} write"
                )
            if approved_job == "publish-release":
                direct_if_values = [
                    lines[index].strip()[3:].strip()
                    for index in range(start + 1, end)
                    if _indent(lines[index]) == 4 and lines[index].strip().startswith("if:")
                ]
                if direct_if_values != [PUBLISH_GATE]:
                    failures.append(
                        f"{path}:{start + 1}: publish-release must have exact job gate `{PUBLISH_GATE}`"
                    )
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
        print("GitHub workflow permission policy failed:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    print("GitHub workflow permission policy passed: write tokens are isolated to approved jobs.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
