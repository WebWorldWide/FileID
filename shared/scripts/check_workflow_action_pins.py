#!/usr/bin/env python3
"""Fail when a GitHub workflow executes code through a mutable reference."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

MAPPING_RE = re.compile(
    r"^(?P<spaces> *)(?P<dash>- +)?(?P<key>[A-Za-z0-9_-]+):(?P<value>.*)$"
)
USES_RE = re.compile(r"^[^\s#]+$")
REMOTE_ACTION_RE = re.compile(
    r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*@[0-9a-f]{40}$"
)
REMOTE_WORKFLOW_RE = re.compile(
    r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/\.github/workflows/"
    r"[A-Za-z0-9_.-]+\.ya?ml@[0-9a-f]{40}$"
)
IMAGE_BODY = (
    r"[a-z0-9]+(?:[._-][a-z0-9]+)*(?::[0-9]+)?"
    r"(?:/[a-z0-9]+(?:[._-][a-z0-9]+)*)*"
    r"(?::[A-Za-z0-9_][A-Za-z0-9_.-]{0,127})?@sha256:[0-9a-f]{64}"
)
DOCKER_RE = re.compile(rf"^docker://{IMAGE_BODY}$")
CONTAINER_RE = re.compile(rf"^{IMAGE_BODY}$")
QUOTED_KEY_RE = re.compile(r"^ *(?:- +)?['\"][^'\"]+['\"] *:")
BLOCK_SCALAR_VALUE_RE = re.compile(r"^[>|](?:[1-9][+-]?|[+-][1-9]?)?$")
PROPERTY_RE = re.compile(
    r"(?:^|\s)(?:[&*][A-Za-z0-9_-]+|!<[^>]+>|!+(?:[A-Za-z0-9_-]+)?)"
)
STRUCTURAL_FLOW_KEYS = {"jobs", "steps", "container", "services"}
CONTAINER_MAPPING_KEYS = {"image", "credentials", "env", "ports", "volumes", "options"}
NONCANONICAL_SECURITY_KEY_RE = re.compile(
    r"^ *(?:- +)?(?P<key>uses|container|image) +:"
)
NONCANONICAL_MAPPING_RE = re.compile(r"^ *(?:- +)?[A-Za-z0-9_-]+ +:")


def _indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _without_comment(line: str) -> str:
    single = False
    double = False
    expression_depth = 0
    index = 0
    while index < len(line):
        if not single and not double and line.startswith("${{", index):
            expression_depth += 1
            index += 3
            continue
        if expression_depth and line.startswith("}}", index):
            expression_depth -= 1
            index += 2
            continue
        char = line[index]
        if expression_depth == 0:
            if char == "'" and not double:
                single = not single
            elif char == '"' and not single:
                backslashes = 0
                cursor = index - 1
                while cursor >= 0 and line[cursor] == "\\":
                    backslashes += 1
                    cursor -= 1
                if backslashes % 2 == 0:
                    double = not double
            elif (
                char == "#"
                and not single
                and not double
                and (index == 0 or line[index - 1] == " ")
            ):
                return line[:index].rstrip(" ")
        index += 1
    return line.rstrip(" ")


def _structural_text(line: str) -> str:
    result: list[str] = []
    single = False
    double = False
    expression_depth = 0
    index = 0
    while index < len(line):
        if not single and not double and line.startswith("${{", index):
            expression_depth += 1
            result.extend("   ")
            index += 3
            continue
        if expression_depth and line.startswith("}}", index):
            expression_depth -= 1
            result.extend("  ")
            index += 2
            continue
        char = line[index]
        if expression_depth:
            result.append(" ")
        elif char == "'" and not double:
            single = not single
            result.append(" ")
        elif char == '"' and not single:
            backslashes = 0
            cursor = index - 1
            while cursor >= 0 and line[cursor] == "\\":
                backslashes += 1
                cursor -= 1
            if backslashes % 2 == 0:
                double = not double
            result.append(" ")
        elif single or double:
            result.append(" ")
        elif char == "#" and (index == 0 or line[index - 1] == " "):
            break
        else:
            result.append(char)
        index += 1
    return "".join(result)


def _local_path_valid(reference: str, *, workflow: bool) -> bool:
    if not reference.startswith("./") or "\\" in reference or "@" in reference:
        return False
    parts = reference[2:].split("/")
    if not parts or any(not part or part in {".", ".."} for part in parts):
        return False
    if workflow:
        return (
            len(parts) == 3
            and parts[:2] == [".github", "workflows"]
            and re.fullmatch(r"[A-Za-z0-9_.-]+\.ya?ml", parts[2]) is not None
        )
    return True


def _reference_failure(reference: str, *, job_level: bool) -> str | None:
    if reference.startswith("docker://"):
        if job_level:
            return "Docker references are valid only for steps"
        if not DOCKER_RE.fullmatch(reference):
            return "Docker actions must use canonical docker://IMAGE@sha256:<64-lowercase-hex> form"
        return None
    if reference.startswith("./"):
        if not _local_path_valid(reference, workflow=job_level):
            kind = "workflow" if job_level else "action"
            return f"invalid local {kind} reference"
        return None
    if job_level:
        if not REMOTE_WORKFLOW_RE.fullmatch(reference):
            return "remote reusable workflows must use owner/repo/.github/workflows/file.yml@<40-lowercase-hex>"
        target = reference.rsplit("@", 1)[0]
        if any(part in {".", ".."} for part in target.split("/")):
            return "reusable workflow coordinates cannot contain dot or traversal segments"
        return None
    if "/.github/workflows/" in reference:
        return "reusable workflows cannot be invoked from a step"
    if not REMOTE_ACTION_RE.fullmatch(reference):
        return "remote actions must use owner/repo[/path]@<40-lowercase-hex>"
    target = reference.rsplit("@", 1)[0]
    if any(part in {".", ".."} for part in target.split("/")):
        return "remote action paths cannot contain dot or traversal segments"
    return None


def _direct_job_field(parents: tuple[str, ...]) -> bool:
    return len(parents) == 2 and parents[0] == "jobs"


def _step_field(parents: tuple[str, ...]) -> bool:
    return len(parents) == 3 and parents[0] == "jobs" and parents[2] == "steps"


def _container_image_field(parents: tuple[str, ...]) -> bool:
    return (
        len(parents) == 3
        and parents[0] == "jobs"
        and parents[2] == "container"
    ) or (
        len(parents) == 4
        and parents[0] == "jobs"
        and parents[2] == "services"
    )


def violations(workflows: Path) -> list[str]:
    failures: list[str] = []
    paths = sorted((*workflows.glob("*.yml"), *workflows.glob("*.yaml")))
    if not paths:
        return [f"no workflow files found under {workflows}"]
    for path in paths:
        try:
            text = path.read_text(encoding="utf-8")
            if text.startswith("\ufeff"):
                failures.append(f"{path}: UTF-8 BOM is unsupported in workflow YAML")
                text = text.removeprefix("\ufeff")
            lines = text.splitlines()
        except (OSError, UnicodeError) as error:
            failures.append(f"{path}: cannot read workflow as UTF-8: {error}")
            continue
        block_scalar_indent: int | None = None
        context: list[tuple[int, str]] = []
        for line_number, raw_line in enumerate(lines, 1):
            if block_scalar_indent is not None:
                if not raw_line.strip() or _indent(raw_line) > block_scalar_indent:
                    continue
                block_scalar_indent = None
            if "\t" in raw_line:
                failures.append(f"{path}:{line_number}: tabs are forbidden in workflow YAML")
                continue
            line = _without_comment(raw_line)
            if not line.strip():
                continue

            mapping = MAPPING_RE.fullmatch(line)
            effective_indent = _indent(line)
            parents: tuple[str, ...] = ()
            key: str | None = None
            value = ""
            if mapping:
                effective_indent = len(mapping.group("spaces")) + len(mapping.group("dash") or "")
                while context and context[-1][0] >= effective_indent:
                    context.pop()
                parents = tuple(item[1] for item in context)
                key = mapping.group("key")
                value = mapping.group("value").strip(" ")
            else:
                while context and context[-1][0] >= effective_indent:
                    context.pop()
                parents = tuple(item[1] for item in context)

            executable_uses = key == "uses" and (_direct_job_field(parents) or _step_field(parents))
            executable_container = key == "container" and _direct_job_field(parents)
            executable_image = key == "image" and _container_image_field(parents)

            if mapping and BLOCK_SCALAR_VALUE_RE.fullmatch(value):
                if executable_uses or executable_container or executable_image:
                    failures.append(
                        f"{path}:{line_number}: executable references must be one immutable literal, not a block scalar"
                    )
                else:
                    block_scalar_indent = effective_indent
                if key is not None:
                    context.append((effective_indent, key))
                continue

            if key is not None and not value:
                context.append((effective_indent, key))

            stripped = line.lstrip(" ")
            structure = _structural_text(raw_line)
            if re.match(r"^(?:---|\.\.\.)(?: |$)", stripped) or stripped.startswith("%"):
                failures.append(
                    f"{path}:{line_number}: YAML directives and document markers are unsupported"
                )
                continue
            if QUOTED_KEY_RE.match(line):
                failures.append(
                    f"{path}:{line_number}: quoted mapping keys are unsupported; use canonical plain block keys"
                )
                continue
            if NONCANONICAL_MAPPING_RE.match(line):
                failures.append(
                    f"{path}:{line_number}: mapping keys cannot contain whitespace before `:`"
                )
                continue
            if (
                re.match(r"^(?:- +)?\?", stripped)
                or re.match(r"^(?:- +)?<< *:", stripped)
                or (key not in {"run", "name", "if"} and PROPERTY_RE.search(structure))
            ):
                failures.append(
                    f"{path}:{line_number}: YAML properties, explicit keys, aliases, merges, and tags are unsupported"
                )
                continue
            noncanonical = NONCANONICAL_SECURITY_KEY_RE.match(line)
            if noncanonical:
                candidate = noncanonical.group("key")
                if (
                    (candidate == "uses" and (_direct_job_field(parents) or _step_field(parents)))
                    or (candidate == "container" and _direct_job_field(parents))
                    or (candidate == "image" and _container_image_field(parents))
                ):
                    failures.append(
                        f"{path}:{line_number}: executable keys require canonical `key: literal` form"
                    )
                    continue
            if mapping is None and re.match(r"^(?:[\[{]|- +[\[{])", stripped):
                failures.append(
                    f"{path}:{line_number}: multiline flow structure is unsupported by the pin policy"
                )
                continue
            dynamic_job_or_service = (
                parents == ("jobs",)
                or (
                    len(parents) == 3
                    and parents[0] == "jobs"
                    and parents[2] == "services"
                )
            )
            if (
                (key in STRUCTURAL_FLOW_KEYS and value.startswith(("[", "{")))
                or (dynamic_job_or_service and value.startswith("{"))
            ):
                failures.append(
                    f"{path}:{line_number}: flow form is unsupported for security-sensitive workflow structure"
                )
                continue

            direct_job_container_child = (
                len(parents) == 3
                and parents[0] == "jobs"
                and parents[2] == "container"
            )
            if direct_job_container_child and key not in CONTAINER_MAPPING_KEYS:
                failures.append(
                    f"{path}:{line_number}: job container mappings require canonical supported keys and an immutable image"
                )
                continue

            if executable_container:
                if value and not CONTAINER_RE.fullmatch(value):
                    failures.append(
                        f"{path}:{line_number}: job containers must use IMAGE@sha256:<64-lowercase-hex>: {value}"
                    )
                continue
            if executable_image:
                if not value or not CONTAINER_RE.fullmatch(value):
                    failures.append(
                        f"{path}:{line_number}: container images must use IMAGE@sha256:<64-lowercase-hex>: {value}"
                    )
                continue
            if not executable_uses:
                continue
            if not value or not USES_RE.fullmatch(value):
                failures.append(
                    f"{path}:{line_number}: executable uses entries require one unquoted literal on the same line"
                )
                continue
            failure = _reference_failure(value, job_level=_direct_job_field(parents))
            if failure is not None:
                failures.append(f"{path}:{line_number}: {failure}: {value}")
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
    print("GitHub Action pin policy passed: external actions use commits and containers use digests.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
