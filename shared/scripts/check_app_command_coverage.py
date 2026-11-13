#!/usr/bin/env python3
"""Fail when a command the Mac app can send isn't served by the Rust engine it hosts.

The Mac app starts the shared Rust engine (`fileidd`, the `apple-adapters`
build) by default. Every command it can send must therefore be

1. dispatched by the Rust engine: a `CommandPayload::<Name>` arm in
   `server.rs` that does more than `{}`, and, for catalog, tool and chat
   requests, the action named in the matching handler; and
2. exercised on that engine by a conformance case CI runs against the
   app-hosted build (pinned to the Swift engine's replies), or listed in
   OUTSIDE_CONFORMANCE with the reason and where it was measured instead.

"App-reachable" is read from the app's sources: a command case of
`IPCCommand.Payload` written as `.name` there, and every catalog/tool/chat
action and tool recipe kind of the IPC schema written there as a string
literal. Commands served by neither engine would be listed in UNSUPPORTED_BY_BOTH;
there are none.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP_SOURCES = "platforms/apple/app/Sources"
PROTOCOL = "platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift"
SCHEMA = "shared/ipc-schema/ipc.schema.json"
CASES = "shared/test-corpus/conformance"
ENGINE = "platforms/windows/src/engine/src"
HANDLERS = {
    "catalog": ["commands/catalog.rs"],
    "tool": ["commands/tools.rs"],
    "chat": ["commands/chat.rs"],
    "recipe": ["commands/tools.rs", "commands/media_recipes.rs", "commands/video_export.rs", "apple/media.rs"],
}

# Reachable commands that no conformance case can run on CI's runner, with the
# reason and where the Rust engine was checked against the Swift engine instead.
# Empty: every app-reachable command has a case. An entry looks like
# ("command", "benchmarkModel"): "needs an installed model; STATE 2026-11-11 Stage 7".
OUTSIDE_CONFORMANCE: dict[tuple[str, str], str] = {}

# App-reachable commands that both engines answer as unsupported. Empty: the
# hosted Rust engine serves everything the app sends.
UNSUPPORTED_BY_BOTH: dict[tuple[str, str], str] = {}

FAMILY_COMMAND = {"catalog": "catalogRequest", "tool": "toolRequest", "chat": "chatRequest"}


def app_texts(root: Path) -> list[str]:
    return [path.read_text(encoding="utf-8") for path in sorted((root / APP_SOURCES).rglob("*.swift"))
            if not path.name.endswith(" 2.swift")]


def command_cases(root: Path) -> list[str]:
    source = (root / PROTOCOL).read_text(encoding="utf-8")
    start = source.index("public enum Payload: Codable")
    end = source.index("\n    }\n", start)
    return re.findall(r"^\s*case (\w+)", source[start:end], re.M)


def schema_enums(root: Path) -> dict[str, list[str]]:
    defs = json.loads((root / SCHEMA).read_text(encoding="utf-8"))["$defs"]
    return {
        "catalog": defs["CatalogRequest"]["properties"]["action"]["enum"],
        "tool": defs["ToolRequest"]["properties"]["action"]["enum"],
        "chat": defs["ChatRequest"]["properties"]["action"]["enum"],
        "recipe": defs["ToolRecipe"]["properties"]["kind"]["enum"],
    }


def reachable(root: Path) -> set[tuple[str, str]]:
    texts = app_texts(root)
    found = {("command", name) for name in command_cases(root)
             if any(re.search(rf"\.{name}\b(?!\s*[=:])", text) for text in texts)}
    literals = {literal for text in texts for literal in re.findall(r'"([A-Za-z][A-Za-z0-9]*)"', text)}
    for family, values in schema_enums(root).items():
        found |= {(family, value) for value in values if value in literals}
    return found


def pascal(name: str) -> str:
    return name[0].upper() + name[1:]


def rust_dispatch_gaps(root: Path, wanted: set[tuple[str, str]]) -> list[str]:
    server = (root / ENGINE / "server.rs").read_text(encoding="utf-8")
    gaps = []
    for family, name in sorted(wanted):
        if family == "command":
            arm = re.search(rf"CommandPayload::{pascal(name)}\b[^=]*=>\s*(\{{\s*\}}|\S)", server)
            if not arm:
                gaps.append(f"{name}: no CommandPayload::{pascal(name)} arm in server.rs")
            elif arm.group(1).replace(" ", "") == "{}":
                gaps.append(f"{name}: the server.rs arm does nothing")
            continue
        handlers = "\n".join((root / ENGINE / rel).read_text(encoding="utf-8")
                             for rel in HANDLERS[family] if (root / ENGINE / rel).is_file())
        if f'"{name}"' not in handlers:
            gaps.append(f"{family} {name}: not named in {', '.join(HANDLERS[family])}")
    return gaps


def load_cases(root: Path) -> list[dict]:
    cases = []
    for path in sorted((root / CASES).glob("*.json")):
        cases.extend(json.loads(path.read_text(encoding="utf-8")))
    return cases


def blocked_on_hosted_rust(case: dict) -> bool:
    """CI runs the hosted build with --test-trash-root; a darwin `run: false` that
    the test Trash root lifts still runs there."""
    for item in case.get("knownDivergences", []):
        if item.get("engine") != "rust" or "darwin" not in item.get("os", ["darwin"]):
            continue
        if item.get("liftedByTestTrashRoot"):
            continue
        return True
    return False


def step_commands(step: dict):
    if "command" in step:
        (name, body), = step["command"].items()
        yield "command", name
        family = {value: key for key, value in FAMILY_COMMAND.items()}.get(name)
        if family:
            request = body.get("request", {})
            yield family, request.get("action")
            if family == "tool" and isinstance(request.get("recipe"), dict):
                yield "recipe", request["recipe"].get("kind")
    for family, command in FAMILY_COMMAND.items():
        if family in step:
            yield "command", command
            yield family, step[family].get("action")
            recipe = step[family].get("recipe")
            if family == "tool" and isinstance(recipe, dict):
                yield "recipe", recipe.get("kind")
    if "shutdown" in step or "restart" in step:
        yield "command", "shutdown"


def exercised_on_hosted_rust(cases: list[dict]) -> dict[tuple[str, str], list[str]]:
    """Commands each case sends to the Rust engine when CI runs it: as the lead
    engine (the case's `engines` include rust) or as the peer a Swift-led
    cross-engine case restarts into."""
    seen: dict[tuple[str, str], list[str]] = {}
    for case in cases:
        if blocked_on_hosted_rust(case):
            continue
        for lead in case.get("engines", ["swift", "rust"]):
            if lead != "rust" and "rust" not in case.get("requires", []):
                continue
            current = case.get("start", lead)
            for step in case.get("steps", []):
                if "restart" in step:
                    if current == "rust":
                        seen.setdefault(("command", "shutdown"), []).append(case["name"])
                    current = step["restart"] if isinstance(step["restart"], str) else lead
                    continue
                if current != "rust":
                    continue
                for key in step_commands(step):
                    seen.setdefault(key, []).append(case["name"])
    return seen


def check(root: Path) -> list[str]:
    wanted = reachable(root)
    failures = rust_dispatch_gaps(root, wanted - set(UNSUPPORTED_BY_BOTH))
    exercised = exercised_on_hosted_rust(load_cases(root))
    for key in sorted(wanted):
        family, name = key
        label = name if family == "command" else f"{family} {name}"
        if key in UNSUPPORTED_BY_BOTH:
            continue
        if key not in exercised and key not in OUTSIDE_CONFORMANCE:
            failures.append(f"{label}: the Mac app sends it but no conformance case runs it on the Rust engine "
                            "(add a case pinned to the Swift engine, or a reason to OUTSIDE_CONFORMANCE)")
    for key in sorted(OUTSIDE_CONFORMANCE):
        if key not in wanted:
            failures.append(f"OUTSIDE_CONFORMANCE {key}: the app no longer sends it; remove the entry")
        elif key in exercised:
            failures.append(f"OUTSIDE_CONFORMANCE {key}: a conformance case now runs it; remove the entry")
    for key in sorted(UNSUPPORTED_BY_BOTH):
        if key not in wanted:
            failures.append(f"UNSUPPORTED_BY_BOTH {key}: the app no longer sends it; remove the entry")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--list", action="store_true", help="print each app-reachable command and its evidence")
    args = parser.parse_args()
    root = args.root.resolve()
    if args.list:
        exercised = exercised_on_hosted_rust(load_cases(root))
        for family, name in sorted(reachable(root)):
            evidence = exercised.get((family, name)) or [OUTSIDE_CONFORMANCE.get((family, name), "-")]
            print(f"{family:8} {name:24} {', '.join(sorted(set(evidence)))[:110]}")
    failures = check(root)
    if failures:
        print("App command coverage failed:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    count = len(reachable(root))
    print(f"App command coverage passed: {count} app-reachable commands and actions are served by the hosted Rust engine.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
