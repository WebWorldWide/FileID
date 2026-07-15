#!/usr/bin/env python3
"""Reject mutable remote-code execution in developer/package bootstrap scripts."""

from __future__ import annotations

import argparse
import hashlib
import re
import shlex
from pathlib import Path

REMOTE_PIPE_RE = re.compile(r"(?:curl|wget)[^\n|]*\|", re.IGNORECASE)
DOWNLOAD_RE = re.compile(r"\b(?:curl|wget)\b", re.IGNORECASE)
MAKE_EXECUTABLE_RE = re.compile(r"\bchmod\s+(?:[^\n]*\s)?\+x\b", re.IGNORECASE)
GIT_REQUIREMENT_RE = re.compile(r"git\+https?://[^\s\"']+")
PINNED_GIT_RE = re.compile(r"@[0-9a-fA-F]{40}(?:$|[#&])")
FORBIDDEN_INSTALLER_URLS = (
    "raw.githubusercontent.com/Homebrew/install/",
    "sh.rustup.rs",
)
SKIP_PARTS = {".git", ".ralph", "target", "node_modules", ".venv", ".venv-ramplus"}
DIRECT_PIN_RE = re.compile(r"^[A-Za-z0-9_.-]+==[^\s]+$")
DOWNLOAD_EXEC_ALLOWLIST = {"packaging/appimage/build-appimage.sh"}
APPIMAGE_SCRIPT_SHA256 = "1f281b23f3fb3bf12025b0a72f66de6b8901356b71392b9478b41b29859a970f"
INTERPRETERS = {
    "sh", "bash", "zsh", "dash", "ksh", "fish", "python", "python3",
    "perl", "ruby", "node", "php", "pwsh",
}
COMMAND_PREFIXES = {"command", "exec", "sudo", "env"}
CONTROL_PREFIXES = {"if", "while", "until", "then", "do", "!"}
WRAPPER_OPTIONS_WITH_VALUE = {
    "sudo": {"-u", "--user", "-g", "--group", "-h", "--host", "-p", "--prompt",
             "-C", "--close-from", "-T", "--command-timeout", "-r", "--role", "-t", "--type"},
    "exec": {"-a"},
    "env": {"-u", "--unset", "-C", "--chdir", "-S", "--split-string"},
    "command": set(),
}


def _shell_scripts(root: Path) -> list[Path]:
    return sorted(
        path for path in root.rglob("*.sh")
        if not any(part in SKIP_PARTS for part in path.relative_to(root).parts)
    )


def _commands(text: str) -> list[tuple[str, list[str]]]:
    commands: list[tuple[str, list[str]]] = []
    logical = text.replace("\\\n", " ")
    for segment in re.split(r"\n|;|&&|\|\||\||\bthen\b|\bdo\b", logical):
        try:
            tokens = shlex.split(segment, comments=True, posix=True)
        except ValueError:
            continue
        while tokens and Path(tokens[0]).name.lower() in CONTROL_PREFIXES:
            tokens.pop(0)
        while tokens and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", tokens[0]):
            tokens.pop(0)
        if not tokens:
            continue
        while tokens and Path(tokens[0]).name.lower() in COMMAND_PREFIXES:
            wrapper = Path(tokens.pop(0)).name.lower()
            value_options = WRAPPER_OPTIONS_WITH_VALUE[wrapper]
            while tokens:
                option = tokens[0]
                if wrapper == "env" and option in {"-S", "--split-string"}:
                    tokens.pop(0)
                    if tokens:
                        try:
                            expanded = shlex.split(tokens.pop(0), comments=True, posix=True)
                        except ValueError:
                            expanded = []
                        tokens[0:0] = expanded
                elif wrapper == "env" and option.startswith("-S") and option != "-S":
                    tokens.pop(0)
                    try:
                        expanded = shlex.split(option[2:], comments=True, posix=True)
                    except ValueError:
                        expanded = []
                    tokens[0:0] = expanded
                elif wrapper == "env" and option.startswith("--split-string="):
                    tokens.pop(0)
                    try:
                        expanded = shlex.split(option.split("=", 1)[1], comments=True, posix=True)
                    except ValueError:
                        expanded = []
                    tokens[0:0] = expanded
                elif wrapper == "env" and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", option):
                    tokens.pop(0)
                elif option == "--":
                    tokens.pop(0)
                    break
                elif option in value_options:
                    tokens.pop(0)
                    if tokens:
                        tokens.pop(0)
                elif option.startswith("-"):
                    tokens.pop(0)
                else:
                    break
        if not tokens:
            continue
        command = Path(tokens[0]).name.lower()
        commands.append((command, tokens[1:]))
    return commands


def _interpreter_reads_file(command: str, arguments: list[str]) -> bool:
    if command.startswith("python") and any(flag in arguments[:2] for flag in ("-m", "-c")):
        return False
    if command in {"sh", "bash", "zsh", "dash", "ksh", "fish", "node", "php", "pwsh"} and "-c" in arguments[:2]:
        return False
    return any(not argument.startswith("-") for argument in arguments)


def _remote_substitution_present(text: str) -> bool:
    lowered = text.lower()
    for opener in ("$(", "<("):
        cursor = 0
        while (start := lowered.find(opener, cursor)) >= 0:
            depth = 1
            index = start + 2
            while index < len(lowered) and depth > 0:
                if lowered[index] == "(":
                    depth += 1
                elif lowered[index] == ")":
                    depth -= 1
                index += 1
            body = lowered[start + 2:index - 1 if depth == 0 else len(lowered)]
            if DOWNLOAD_RE.search(body):
                return True
            cursor = max(index, start + 2)
    cursor = 0
    while (start := lowered.find("`", cursor)) >= 0:
        end = lowered.find("`", start + 1)
        if end < 0:
            break
        if DOWNLOAD_RE.search(lowered[start + 1:end]):
            return True
        cursor = end + 1
    return False


def _download_exec_contract_valid(relative: Path, text: str) -> bool:
    name = relative.as_posix()
    if name not in DOWNLOAD_EXEC_ALLOWLIST:
        return False
    if name == "packaging/appimage/build-appimage.sh":
        normalized = text.replace("\r\n", "\n")
        return hashlib.sha256(normalized.encode("utf-8")).hexdigest() == APPIMAGE_SCRIPT_SHA256
    return False


def violations(root: Path) -> list[str]:
    failures: list[str] = []
    scripts = _shell_scripts(root)
    if not scripts:
        failures.append(f"no shell scripts found under {root}")
    for path in scripts:
        text = path.read_text(encoding="utf-8")
        relative = path.relative_to(root)
        logical_text = text.replace("\\\n", " ")
        if REMOTE_PIPE_RE.search(logical_text):
            failures.append(f"{relative}: pipes a remote response into another process")
        if _remote_substitution_present(text):
            failures.append(f"{relative}: exposes a remote response through shell substitution")
        commands = _commands(logical_text)
        network_invoked = any(command in {"curl", "wget"} for command, _ in commands)
        interpreter_invoked = any(
            (command in INTERPRETERS or re.fullmatch(r"python3?\.[0-9]+", command))
            and _interpreter_reads_file(command, arguments)
            for command, arguments in commands
        )
        downloaded_code = network_invoked and (
            MAKE_EXECUTABLE_RE.search(text) or interpreter_invoked
        )
        if downloaded_code and not _download_exec_contract_valid(relative, text):
            failures.append(
                f"{relative}: downloaded code execution is not covered by an artifact-bound reviewed SHA-256 contract"
            )
        for forbidden in FORBIDDEN_INSTALLER_URLS:
            if forbidden in text:
                failures.append(f"{relative}: references mutable remote installer {forbidden}")
        for match in GIT_REQUIREMENT_RE.finditer(text):
            requirement = match.group(0)
            if not PINNED_GIT_RE.search(requirement):
                failures.append(f"{relative}: unpinned git requirement {requirement}")

    requirements = root / "shared" / "scripts" / "requirements-ramplus.txt"
    if not requirements.is_file():
        failures.append(f"missing {requirements.relative_to(root)}")
    else:
        for line_number, line in enumerate(
            requirements.read_text(encoding="utf-8").splitlines(), 1
        ):
            value = line.split("#", 1)[0].strip()
            if value and not DIRECT_PIN_RE.fullmatch(value):
                failures.append(
                    f"{requirements.relative_to(root)}:{line_number}: direct dependency is not exactly pinned: {value}"
                )
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--root", type=Path,
        default=Path(__file__).resolve().parents[2],
    )
    args = parser.parse_args()
    failures = violations(args.root.resolve())
    if failures:
        print("Bootstrap supply-chain policy failed:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    print("Bootstrap supply-chain policy passed: remote-shell execution patterns are absent and direct inputs are pinned.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
