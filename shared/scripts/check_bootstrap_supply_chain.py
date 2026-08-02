#!/usr/bin/env python3
"""Reject mutable remote-code execution in developer/package bootstrap scripts."""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import shlex
import subprocess
from pathlib import Path

DOWNLOAD_RE = re.compile(r"\b(?:curl|wget)\b", re.IGNORECASE)
MAKE_EXECUTABLE_RE = re.compile(
    r"\b(?:chmod(?:\s+--)?(?:\s+-[^\s;]+)*\s+(?:\+x|[0-7]*[1357][0-7]*)\s+[^\n;]+"
    r"|install\s+[^\n;]*-m\s*[0-7]*[1357][0-7]*\s+[^\n;]+)",
    re.IGNORECASE,
)
GIT_REQUIREMENT_RE = re.compile(r"git\+https?://[^\s\"']+")
PINNED_GIT_RE = re.compile(r"@[0-9a-fA-F]{40}(?:$|[#&])")
FORBIDDEN_INSTALLER_URLS = (
    "raw.githubusercontent.com/Homebrew/install/",
    "sh.rustup.rs",
)
SKIP_PARTS = {".git", ".ralph", "target", "node_modules", ".venv", ".venv-ramplus"}
DIRECT_PIN_RE = re.compile(r"^[A-Za-z0-9_.-]+==[^\s]+$")
DOWNLOAD_EXEC_ALLOWLIST = {"packaging/appimage/build-appimage.sh"}
REVIEWED_SHELL_SCRIPT_SHA256 = {
    "build.sh": "9e18d3ed14e88eab1cbb642ff5e3fff47d5ffd988bdf668002621c583f67caf5",
    "packaging/appimage/build-appimage.sh": "1f281b23f3fb3bf12025b0a72f66de6b8901356b71392b9478b41b29859a970f",
    "packaging/aur/PKGBUILD": "37f45e9aabd9a0f0d0ed15bf3700706741712a81e4cee7021ab72a76557dacfc",
    "platforms/apple/run.sh": "09fbe948fd1488f5bca529bf97059df1be4da74b5227b5b1ac0fa787dabf4864",
    "platforms/apple/scripts/assemble_app.sh": "4e191d83b63e17814c7a42b3476cc07ceef6238a0fc10df17fe627dda3b32f5f",
    "platforms/apple/scripts/build_corpus.sh": "a5f53f4df77c07dc7aefd4e0c31dbbaa90dac92e68d3e614c789c36320cced87",
    "platforms/apple/scripts/build_dmg.sh": "af9c3c636e4bfeb0a9e9697dff93780da121985712eb18d5c3a8aa90412ca241",
    "platforms/apple/scripts/iterate.sh": "58b9e08c70f18001b92446e4a8553677ee3da07f1a03d2bdd667c069e3c44422",
    "platforms/apple/scripts/release.sh": "fadd5f3477849f2cf80bff1ee1d0bd25670a92354b01eb35a00773cd7ce4d298",
    "platforms/apple/scripts/wipe_local_state.sh": "3f522f095384110849c618f10f62f75025eb6cabe4f6e66488aaea21a716df69",
    "platforms/linux/build/build.sh": "8fb81d5a508916a72009d494ccdbee85c09a2b54e7bdfcf43910ab07af6f8550",
    "scripts/build-tools.sh": "44846aea1eadbe97166bacc47486701894542681114effddcb895762e7c81bf6",
    "shared/scripts/check_tls_pins.sh": "3096b3be8c3e93030cb5c69c1157c76413e51a333c785e70392feaf46c527024",
    "shared/scripts/compare_face_clustering.sh": "79af61febba1cfbe7eae6fcd4e4450b149ab32d35ef73aa13a4dfb5c6abd7d9f",
    "shared/scripts/install_onnxruntime_macos.sh": "d5c4f189b2bd770e1454fec5cc813b252a626b98c45bb8c3d7593f948d3ecb97",
    "shared/scripts/run_local_audit_gate.sh": "96f0d2d77eddece17aba99229b92d7385d2c06bbead05f275b890e3eec99a2a7",
    "shared/scripts/setup-dev.sh": "a0fc33122370e895ca2ecb2528d1d5ef8e3ef0458874eafa33e42f2e2770b1d5",
    "tools/git-hooks/pre-commit": "30ce0c4982fb3b163e84948dd3c84f268761e03ace1428346f21f79b59e9da5b",
}
INTERPRETERS = {
    "sh", "bash", "zsh", "dash", "ksh", "fish", "python", "python3",
    "perl", "ruby", "node", "php", "pwsh",
}
SOURCE_COMMANDS = {"source", "."}
COMMAND_PREFIXES = {
    "command", "exec", "sudo", "env", "builtin", "nice", "nohup", "setsid", "stdbuf", "timeout",
}
CONTROL_PREFIXES = {"if", "while", "until", "then", "do", "!"}
WRAPPER_OPTIONS_WITH_VALUE = {
    "sudo": {"-u", "--user", "-g", "--group", "-h", "--host", "-p", "--prompt",
             "-C", "--close-from", "-T", "--command-timeout", "-r", "--role", "-t", "--type"},
    "exec": {"-a"},
    "env": {"-u", "--unset", "-C", "--chdir", "-S", "--split-string"},
    "command": set(),
    "builtin": set(),
    "nice": {"-n", "--adjustment"},
    "nohup": set(),
    "setsid": set(),
    "stdbuf": {"-i", "--input", "-o", "--output", "-e", "--error"},
    "timeout": {"-k", "--kill-after", "-s", "--signal"},
}


def _normalize_shell(text: str) -> str:
    return text.replace("\r\n", "\n").replace("\r", "\n").replace("\\\n", "")


def _is_shell_script(path: Path) -> bool:
    if path.name in {"PKGBUILD", "APKBUILD"} \
            or path.suffix.lower() in {".sh", ".bash", ".zsh", ".ksh", ".fish", ".ebuild", ".install"}:
        return True
    try:
        with path.open("rb") as source:
            raw_first_line = source.readline(4097)
    except OSError:
        return False
    if len(raw_first_line) == 4097 and not raw_first_line.endswith(b"\n"):
        return raw_first_line.startswith(b"#!")
    try:
        first_line = raw_first_line.decode("utf-8", errors="strict")
    except UnicodeDecodeError:
        return raw_first_line.startswith(b"#!")
    if not first_line.startswith("#!"):
        return False
    interpreter = first_line[2:]
    if re.search(r"(?:^|[/\s])(?:bash|dash|sh|zsh|ksh|fish)(?:\s|$)", interpreter):
        return True
    return "/usr/bin/env" in interpreter \
        and ("-S" in interpreter or "--split-string" in interpreter) \
        and re.search(r"(?:bash|dash|zsh|ksh|fish|(?:^|[^A-Za-z])sh)", interpreter) is not None


def _tracked_symlinks(root: Path) -> set[Path]:
    try:
        tracked = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z", "--stage"],
            check=True,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return set()
    symlinks = set()
    for record in tracked.stdout.split(b"\0"):
        if not record or b"\t" not in record:
            continue
        metadata, encoded_path = record.split(b"\t", 1)
        if metadata.split(maxsplit=1)[0] == b"120000":
            symlinks.add(root / os.fsdecode(encoded_path))
    return symlinks


def _shell_scripts(root: Path) -> list[Path]:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            check=True,
            capture_output=True,
        )
        paths = [root / os.fsdecode(raw) for raw in result.stdout.split(b"\0") if raw]
        symlinks = _tracked_symlinks(root)
        candidates = [
            path for path in paths
            if (path.is_file() and _is_shell_script(path)) or path.is_symlink()
        ]
        return sorted(set(candidates) | symlinks)
    except (OSError, subprocess.CalledProcessError):
        pass
    return sorted(
        path for path in root.rglob("*")
        if (path.is_file() or path.is_symlink())
        and not any(part in SKIP_PARTS for part in path.relative_to(root).parts)
        and (path.is_symlink() or _is_shell_script(path))
    )


def _shell_segments(text: str) -> list[str]:
    segments: list[str] = []
    start = 0
    quote: str | None = None
    escaped = False
    index = 0
    while index < len(text):
        char = text[index]
        if escaped:
            escaped = False
            index += 1
            continue
        if quote == '"' and char == "\\":
            escaped = True
            index += 1
            continue
        if quote:
            if char == quote:
                quote = None
            index += 1
            continue
        if char == "#" and (index == 0 or text[index - 1].isspace()):
            end = text.find("\n", index)
            index = len(text) if end < 0 else end
            continue
        if char in {"'", '"'}:
            quote = char
            index += 1
            continue
        if char == "\\":
            escaped = True
            index += 1
            continue
        width = 0
        if char in {"\n", ";", "|"}:
            width = 2 if char == "|" and index + 1 < len(text) and text[index + 1] == "|" else 1
        elif char == "&" and index + 1 < len(text) and text[index + 1] == "&":
            width = 2
        if width:
            segments.append(text[start:index])
            start = index + width
            index += width
        else:
            index += 1
    segments.append(text[start:])
    return segments


def _has_unquoted_pipe(text: str) -> bool:
    normalized = _normalize_shell(text)
    quote: str | None = None
    escaped = False
    index = 0
    while index < len(normalized):
        char = normalized[index]
        if escaped:
            escaped = False
        elif quote == '"' and char == "\\":
            escaped = True
        elif quote:
            if char == quote:
                quote = None
        elif char == "#" and (index == 0 or normalized[index - 1].isspace()):
            end = normalized.find("\n", index)
            index = len(normalized) if end < 0 else end
            continue
        elif char in {"'", '"'}:
            quote = char
        elif char == "|" and not normalized.startswith("||", index):
            return True
        index += 1
    return False


def _has_unparseable_segment(text: str) -> bool:
    for segment in _shell_segments(_normalize_shell(text)):
        try:
            shlex.split(segment, comments=True, posix=True)
        except ValueError:
            return True
    return False


def _commands(text: str) -> list[tuple[str, list[str]]]:
    commands: list[tuple[str, list[str]]] = []
    logical = _normalize_shell(text)
    for segment in _shell_segments(logical):
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
            if wrapper == "timeout" and tokens:
                tokens.pop(0)
        while tokens and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", tokens[0]):
            tokens.pop(0)
        if not tokens:
            continue
        raw_command = tokens[0]
        command = (
            raw_command.lower()
            if raw_command == "." or raw_command.startswith(("./", "../", "/tmp/", "/var/tmp/"))
            else Path(raw_command).name.lower()
        )
        commands.append((command, tokens[1:]))
    return commands


def _interpreter_reads_file(command: str, arguments: list[str]) -> bool:
    if command.startswith("python") and "-m" in arguments[:2]:
        return False
    for index, argument in enumerate(arguments[:2]):
        if argument == "-c" or (argument.startswith("-") and "c" in argument[1:]):
            if argument == "-c" or not argument.startswith("-c"):
                code = arguments[index + 1] if index + 1 < len(arguments) else ""
            else:
                code = argument[2:]
            return bool(re.search(r"\b(?:source|eval|exec|open|runpy)\b|(?:^|[;&|])\s*\.\s+", code))
    return any(not argument.startswith("-") for argument in arguments)


def _command_executes_code(command: str, arguments: list[str]) -> bool:
    if command in SOURCE_COMMANDS or command == "eval":
        return True
    if command in INTERPRETERS or re.fullmatch(r"python3?\.[0-9]+", command):
        return _interpreter_reads_file(command, arguments)
    if command == "xargs":
        return any(
            Path(argument).name.lower() in INTERPRETERS
            or argument.startswith(("/", "./", "../"))
            for argument in arguments
        )
    if command == "find" and any(argument in {"-exec", "-execdir"} for argument in arguments):
        return True
    if command == "chmod":
        mode = next((argument for argument in arguments if not argument.startswith("-")), "")
        if "$" in mode or "x" in mode.lower():
            return True
        return bool(re.fullmatch(r"[0-7]*[1357][0-7]{0,2}", mode))
    return command.startswith(("./", "../", "/tmp/", "/var/tmp/"))


def _nested_downloader_shell_present(commands: list[tuple[str, list[str]]]) -> bool:
    for command, arguments in commands:
        if command not in {"sh", "bash", "zsh", "dash", "ksh", "fish"}:
            continue
        for index, argument in enumerate(arguments):
            if not argument.startswith("-") or "c" not in argument[1:]:
                continue
            if argument == "-c" or not argument.startswith("-c"):
                code = arguments[index + 1] if index + 1 < len(arguments) else ""
            else:
                code = argument[2:]
            if DOWNLOAD_RE.search(code):
                return True
    return False


def _constant_command_indirection(text: str) -> tuple[bool, bool]:
    assignments = {
        match.group(1): Path(match.group(2)).name.lower()
        for match in re.finditer(
            r"(?:^|[;\n])\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*['\"]?([A-Za-z0-9_./+-]+)['\"]?\s*(?=;|\n|$)",
            text,
        )
    }
    aliases = {
        match.group(1): Path(match.group(2)).name.lower()
        for match in re.finditer(
            r"(?:^|[;\n])\s*alias\s+([A-Za-z_][A-Za-z0-9_]*)=['\"]([A-Za-z0-9_./+-]+)['\"]",
            text,
        )
    }
    assignments.update(aliases)
    network = False
    execution = False
    for name, value in assignments.items():
        if name in aliases:
            invoked = re.search(rf"(?m)(?:^|[;&|]\s*){re.escape(name)}(?:\s|$)", text)
        else:
            invoked = re.search(rf"(?m)(?:^|[;&|]\s*)['\"]?\$\{{?{re.escape(name)}\}}?['\"]?(?:\s|$)", text)
        if not invoked:
            continue
        network = network or value in {"curl", "wget"}
        execution = execution or value in INTERPRETERS | SOURCE_COMMANDS | {"eval"}
    network = network or re.search(r"(?:^|[;&|]\s*)\$\{[A-Za-z_][A-Za-z0-9_]*:-(?:curl|wget)\}(?:\s|$)", text) is not None
    execution = execution or re.search(r"(?:^|[;&|]\s*)\$\{[A-Za-z_][A-Za-z0-9_]*:-(?:source|eval|bash|sh|python3?)\}(?:\s|$)", text) is not None
    return network, execution


def _path_execution_present(text: str) -> bool:
    return re.search(
        r"(?:^|[;\n])\s*PATH\s*=\s*(?:/tmp|/var/tmp)[^\s;]*\s+[A-Za-z_][A-Za-z0-9_.-]*(?:\s|$)",
        text,
    ) is not None


def _remote_substitution_present(text: str) -> bool:
    lowered = _normalize_shell(text).lower()
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


def violations(root: Path) -> list[str]:
    failures: list[str] = []
    scripts = _shell_scripts(root)
    tracked_symlinks = _tracked_symlinks(root)
    for path in scripts:
        relative = path.relative_to(root)
        if path in tracked_symlinks or path.is_symlink() or not path.exists():
            failures.append(f"{relative}: shell/bootstrap symlinks are forbidden")
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            failures.append(f"{relative}: shell source is not readable UTF-8: {error}")
            continue
        logical_text = _normalize_shell(text)
        name = relative.as_posix()
        normalized_source = text.replace("\r\n", "\n")
        actual_digest = hashlib.sha256(normalized_source.encode("utf-8")).hexdigest()
        reviewed_digest = REVIEWED_SHELL_SCRIPT_SHA256.get(name)
        if reviewed_digest is None:
            failures.append(f"{relative}: shell script lacks an artifact-bound reviewed digest")
        elif actual_digest != reviewed_digest:
            failures.append(f"{relative}: reviewed artifact-bound shell script digest changed")
        if _remote_substitution_present(text):
            failures.append(f"{relative}: exposes a remote response through shell substitution")
        if DOWNLOAD_RE.search(logical_text) and _has_unparseable_segment(logical_text):
            failures.append(f"{relative}: downloader-bearing shell text is not safely parseable")
        commands = _commands(logical_text)
        indirect_network, indirect_execution = _constant_command_indirection(logical_text)
        nested_downloader_shell = _nested_downloader_shell_present(commands)
        pipeline_interpreter = _has_unquoted_pipe(logical_text) and any(
            command in INTERPRETERS or re.fullmatch(r"python3?\.[0-9]+", command)
            for command, _ in commands
        )
        execution_invoked = any(
            _command_executes_code(command, arguments) for command, arguments in commands
        ) or indirect_execution or nested_downloader_shell or pipeline_interpreter \
            or _path_execution_present(logical_text)
        network_invoked = (
            any(command in {"curl", "wget"} for command, _ in commands)
            or indirect_network
            or nested_downloader_shell
        )
        downloaded_code = network_invoked and (
            MAKE_EXECUTABLE_RE.search(logical_text) or execution_invoked
        )
        if downloaded_code and name not in DOWNLOAD_EXEC_ALLOWLIST:
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
