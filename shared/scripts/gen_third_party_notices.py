#!/usr/bin/env python3
"""Build and verify THIRD_PARTY_NOTICES.txt.

Inputs (all committed, reviewed):
  shared/licenses/rust-crates.json   every registry crate in the four Cargo.lock files
  shared/licenses/components.json    Swift packages, NuGet packages, native runtimes, fonts
  shared/licenses/texts/             licence and NOTICE texts
  shared/models/manifest.json        model weights FileID can download

Modes:
  (default)   write THIRD_PARTY_NOTICES.txt
  --check     offline policy gate: every lock/Package.resolved/NuGet entry is covered, every
              elected licence is on the allowlist, copyleft is documented, and the committed
              notices file is byte-identical to the generated one
  --refresh   developer step: rebuild rust-crates.json from `cargo metadata` and the local
              cargo registry (needs the crates downloaded; keeps entries it cannot resolve)
"""
import argparse
import glob
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LICENSES = ROOT / "shared" / "licenses"
TEXTS = LICENSES / "texts"
NOTICES = ROOT / "THIRD_PARTY_NOTICES.txt"
LOCKS = {
    "engine": "platforms/windows/src/engine",
    "cli": "platforms/cli",
    "tui": "platforms/tui",
    "linux": "platforms/linux",
}
REFRESH_TARGETS = ("aarch64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu")

# Election order when an SPDX expression offers a choice.
PREFERENCE = [
    "MIT", "Apache-2.0", "BSD-3-Clause", "BSD-2-Clause", "ISC", "Zlib", "Unicode-3.0", "Unicode-DFS-2016",
    "CDLA-Permissive-2.0", "Apache-2.0 WITH LLVM-exception", "0BSD", "MIT-0", "Unlicense",
    "CC0-1.0", "BSL-1.0", "BlueOak-1.0.0", "WTFPL", "MPL-2.0",
]
ALLOWED = set(PREFERENCE)
# File-level copyleft, statically linked unmodified; allowed only with the compliance note below.
COPYLEFT_NOTES = {
    "MPL-2.0": "MPL-2.0 is file-level copyleft. FileID links these crates unmodified, so the "
               "Covered Software is the upstream source, available at the repository listed "
               "for each crate and from crates.io at the exact version shown. FileID's own "
               "source stays under Apache-2.0.",
}
TEXT_FOR = {
    "Apache-2.0 WITH LLVM-exception": "Apache-2.0-LLVM-exception.txt",
    "MIT": "MIT.txt", "BSD-3-Clause": "BSD-3-Clause.txt", "BSD-2-Clause": "BSD-2-Clause.txt",
    "ISC": "ISC.txt", "Zlib": "Zlib.txt", "Unicode-3.0": "Unicode-3.0.txt", "Unicode-DFS-2016": "Unicode-DFS-2016.txt",
    "MPL-2.0": "MPL-2.0.txt", "CDLA-Permissive-2.0": "CDLA-Permissive-2.0.txt",
    "WTFPL": "WTFPL.txt",
}
SPECIAL_CRATE_TEXTS = {"ring": "ring-LICENSE.txt"}
KNOWN_NONSPDX = {
    "LicenseRef-Microsoft-DirectML-Redistributable", "LicenseRef-Microsoft-SDK",
    "LicenseRef-NVIDIA-CUDA", "LicenseRef-NVIDIA-cuDNN", "LicenseRef-Public-Domain",
}


def parse_spdx(expression: str) -> list[frozenset[str]]:
    tokens = re.findall(r"\(|\)|[^\s()]+", expression.replace("/", " OR "))
    position = 0

    def parse_or() -> list[frozenset[str]]:
        nonlocal position
        alternatives = parse_and()
        while position < len(tokens) and tokens[position] == "OR":
            position += 1
            alternatives = alternatives + parse_and()
        return alternatives

    def parse_and() -> list[frozenset[str]]:
        nonlocal position
        alternatives = parse_atom()
        while position < len(tokens) and tokens[position] == "AND":
            position += 1
            right = parse_atom()
            alternatives = [left | other for left in alternatives for other in right]
        return alternatives

    def parse_atom() -> list[frozenset[str]]:
        nonlocal position
        token = tokens[position]
        position += 1
        if token == "(":
            inner = parse_or()
            position += 1
            return inner
        name = token
        if position + 1 < len(tokens) and tokens[position] == "WITH":
            name = f"{token} WITH {tokens[position + 1]}"
            position += 2
        return [frozenset([name])]

    return parse_or()


def elect(expression: str) -> list[str] | None:
    """Pick the preferred allowed alternative, or None when none is fully allowed."""
    best = None
    for alternative in parse_spdx(expression):
        if not alternative <= ALLOWED:
            continue
        rank = sorted(PREFERENCE.index(name) for name in alternative)
        if best is None or rank < best[0]:
            best = (rank, sorted(alternative, key=PREFERENCE.index))
    return best[1] if best else None


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def lock_packages(directory: str) -> set[tuple[str, str]]:
    text = (ROOT / directory / "Cargo.lock").read_text(encoding="utf-8")
    return set(re.findall(r'name = "([^"]+)"\nversion = "([^"]+)"\nsource = "registry', text))


def resolved_swift() -> dict[str, str]:
    pins = read_json(ROOT / "platforms/apple/Package.resolved")["pins"]
    return {pin["identity"]: pin["state"].get("version", "") for pin in pins}


def declared_nuget() -> dict[str, str]:
    props = (ROOT / "platforms/windows/Directory.Packages.props").read_text(encoding="utf-8")
    return dict(re.findall(r'<PackageVersion Include="([^"]+)" Version="([^"]+)"', props))


def crate_key(entry: dict) -> tuple[str, str]:
    return entry["name"], entry["version"]


def load_crates() -> list[dict]:
    return sorted(read_json(LICENSES / "rust-crates.json")["crates"], key=crate_key)


def check() -> list[str]:
    failures: list[str] = []
    crates = load_crates()
    inventory = {crate_key(entry): entry for entry in crates}
    for label, directory in LOCKS.items():
        missing = sorted(lock_packages(directory) - set(inventory))
        if missing:
            shown = ", ".join(f"{name} {version}" for name, version in missing[:8])
            failures.append(f"{directory}/Cargo.lock: {len(missing)} crates not in rust-crates.json "
                            f"(run gen_third_party_notices.py --refresh): {shown}")
    for entry in crates:
        name = f"{entry['name']} {entry['version']}"
        elected = elect(entry["license"]) if entry.get("license") else None
        if elected is None:
            failures.append(f"{name}: licence '{entry.get('license')}' is missing or not on the allowlist")
            continue
        if entry.get("elected") != elected:
            failures.append(f"{name}: recorded election {entry.get('elected')} differs from {elected}")
        for licence in elected:
            if licence == "Apache-2.0":
                continue
            if licence not in TEXT_FOR or not (TEXTS / TEXT_FOR[licence]).is_file():
                failures.append(f"{name}: no licence text for {licence}")
        if entry["name"] in SPECIAL_CRATE_TEXTS and not (TEXTS / SPECIAL_CRATE_TEXTS[entry["name"]]).is_file():
            failures.append(f"{name}: missing {SPECIAL_CRATE_TEXTS[entry['name']]}")
    components = read_json(LICENSES / "components.json")
    swift_listed = {entry["id"]: entry["version"] for entry in components["swift"]}
    for identity, version in resolved_swift().items():
        if identity not in swift_listed:
            failures.append(f"Package.resolved: {identity} {version} is not in components.json")
        elif swift_listed[identity] != version:
            failures.append(f"Package.resolved: {identity} is {version}, components.json says {swift_listed[identity]}")
    nuget_listed = {entry["id"]: entry["version"] for entry in components["nuget"]}
    for identity, version in declared_nuget().items():
        if identity not in nuget_listed:
            failures.append(f"Directory.Packages.props: {identity} {version} is not in components.json")
        elif nuget_listed[identity] != version:
            failures.append(f"Directory.Packages.props: {identity} is {version}, components.json says {nuget_listed[identity]}")
    for group in ("swift", "nuget", "native"):
        for entry in components[group]:
            for licence in re.findall(r"[A-Za-z0-9.+-]+(?: WITH [A-Za-z0-9.+-]+)?", entry["license"]):
                if licence in ("AND", "OR", "WITH"):
                    continue
                known = licence in ALLOWED or licence in KNOWN_NONSPDX or licence in (
                    "OFL-1.1", "LGPL-2.1-or-later", "MS-RL", "Apache-2.0 WITH Swift-exception")
                if not known:
                    failures.append(f"components.json {entry['id']}: unreviewed licence {licence}")
            for text in entry.get("texts", []) + entry.get("noticeFiles", []):
                if not (TEXTS / text).is_file():
                    failures.append(f"components.json {entry['id']}: missing text {text}")
            if "LGPL" in entry["license"] and not entry.get("dynamicLink"):
                failures.append(f"components.json {entry['id']}: LGPL needs a dynamicLink note")
    manifest = read_json(ROOT / "shared/models/manifest.json")
    for artifact in manifest["artifacts"]:
        if artifact["license"] not in manifest["licensePolicies"]:
            failures.append(f"manifest {artifact['id']}: licence {artifact['license']} has no policy")
    if not (ROOT / "LICENSE").is_file():
        failures.append("root LICENSE is missing")
    expected = generate()
    if not NOTICES.is_file() or NOTICES.read_text(encoding="utf-8") != expected:
        failures.append("THIRD_PARTY_NOTICES.txt is out of date (run gen_third_party_notices.py)")
    return failures


def fenced(title: str) -> str:
    return f"\n{'=' * 78}\n{title}\n{'=' * 78}\n\n"


def read_text(name: str) -> str:
    return (TEXTS / name).read_text(encoding="utf-8").rstrip("\n") + "\n"


def generate() -> str:
    components = read_json(LICENSES / "components.json")
    manifest = read_json(ROOT / "shared/models/manifest.json")
    crates = load_crates()
    out: list[str] = []
    out.append(
        "FileID third-party notices\n"
        "==========================\n\n"
        "FileID is licensed under the Apache License, Version 2.0 (the LICENSE file shipped next\n"
        "to this one). It includes or loads the third-party software below. Each component stays\n"
        "under its own licence; the texts are reproduced in the last section. FileID collects no\n"
        "telemetry and makes no network connection except model downloads you start yourself.\n"
        "\nThis file is generated by shared/scripts/gen_third_party_notices.py from\n"
        "shared/licenses/ and shared/models/manifest.json; do not edit it by hand.\n"
    )

    out.append(fenced("1. Application components, runtimes, fonts"))
    for entry in components["swift"]:
        out.append(component_block(entry, "macOS Swift package"))
    for entry in components["native"]:
        out.append(component_block(entry, "Runtime / asset"))

    out.append(fenced("2. Windows (.NET) packages"))
    for entry in components["nuget"]:
        if entry.get("scope") == "redistributed":
            out.append(component_block(entry, "NuGet package"))
    build_only = [entry for entry in components["nuget"] if entry.get("scope") != "redistributed"]
    out.append("Build-time and test-only packages (not redistributed): "
               + ", ".join(f"{e['id']} {e['version']} ({e['license']})" for e in build_only) + "\n\n")

    out.append(fenced("3. AI model weights (downloaded on request, never bundled)"))
    out.append("FileID does not ship model weights. When you ask for a model it is downloaded from\n"
               "Hugging Face at a pinned revision and checked against a pinned SHA-256. Each model\n"
               "stays under its publisher's licence, listed here. Models under licences that need\n"
               "your agreement (Google Gemma Terms, NVIDIA CUDA and cuDNN terms) are opt-in and\n"
               "FileID shows the terms before installing them.\n\n")
    seen_repos: set[str] = set()
    rows = [(a["repo"], a["license"], a["id"]) for a in manifest["artifacts"]]
    rows += [(r["repo"], r["license"], r["kind"]) for r in manifest.get("vlmRepos", [])]
    for repo, licence, identifier in sorted(rows):
        if repo in seen_repos:
            continue
        seen_repos.add(repo)
        policy = manifest["licensePolicies"][licence]
        terms = f"  (terms: {policy['licenseUrl']}, shown before install)" if policy["termsRequired"] else ""
        out.append(f"  https://huggingface.co/{repo}\n    licence: {policy['spdx']}{terms}\n")

    out.append(fenced(f"4. Rust crates ({len(crates)})"))
    for note in sorted({n for entry in crates for n in entry.get("elected", []) if n in COPYLEFT_NOTES}):
        out.append(f"Note on {note}: {COPYLEFT_NOTES[note]}\n\n")
    for entry in crates:
        out.append(crate_block(entry))

    out.append(fenced("5. Licence and notice texts"))
    out.append(text_section("Apache License 2.0", (ROOT / "LICENSE").read_text(encoding="utf-8")))
    used = set()
    for entry in crates:
        used.update(TEXT_FOR[name] for name in entry["elected"] if name in TEXT_FOR)
    for group in ("swift", "native", "nuget"):
        for entry in components[group]:
            used.update(entry.get("texts", []))
    used.update(SPECIAL_CRATE_TEXTS[entry["name"]] for entry in crates if entry["name"] in SPECIAL_CRATE_TEXTS)
    notice_files = sorted({n for entry in components["swift"] for n in entry.get("noticeFiles", [])})
    for name in sorted(used):
        out.append(text_section(name, read_text(name)))
    for name in notice_files:
        out.append(text_section(f"{name} (Apache-2.0 NOTICE file)", read_text(name)))
    return "".join(out)


def text_section(title: str, body: str) -> str:
    return f"--- {title} ---\n\n{body.rstrip(chr(10))}\n\n"


def component_block(entry: dict, kind: str) -> str:
    name = entry.get("name") or entry["id"]
    lines = [f"{name} {entry['version']}".rstrip(), f"  {kind}; licence: {entry['license']}"]
    for copyright_line in entry.get("copyright", []):
        lines.append(f"  {copyright_line}")
    if entry.get("url"):
        lines.append(f"  {entry['url']}")
    lines.append(f"  distribution: {entry['distribution']}")
    for item in entry.get("bundled", []):
        lines.append(f"  includes: {item}")
    if entry.get("note"):
        lines.append(f"  note: {entry['note']}")
    return "\n".join(lines) + "\n\n"


def crate_block(entry: dict) -> str:
    head = f"{entry['name']} {entry['version']}  [{entry['license']}]"
    lines = [head]
    for copyright_line in entry.get("copyright", [])[:3]:
        lines.append(f"  {copyright_line}")
    if entry.get("repository"):
        lines.append(f"  {entry['repository']}")
    return "\n".join(lines) + "\n"


COPYRIGHT_RE = re.compile(r"^\s*(Copyright\b.*|\(c\)\s*\d{4}.*|©.*)$", re.I)


def source_copyright(source: Path) -> list[str]:
    found: list[str] = []
    for path in sorted(source.glob("*")):
        if not re.match(r"(LICEN[SC]E|COPYING|NOTICE|UNLICENSE)", path.name, re.I) or not path.is_file():
            continue
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            match = COPYRIGHT_RE.match(line)
            text = line.strip()
            if not match or "[yyyy]" in text.lower() or "copyright owner" in text.lower() \
                    or "copyright license" in text.lower() or "copyright notice" in text.lower() \
                    or "copyright holder" in text.lower() or len(text) > 200:
                continue
            if text not in found:
                found.append(text)
    return found[:3]


def refresh(roots: list[str]) -> int:
    registry = glob.glob(os.path.expanduser("~/.cargo/registry/src/*/"))
    path = LICENSES / "rust-crates.json"
    existing = {crate_key(entry): entry for entry in read_json(path)["crates"]} if path.is_file() else {}
    merged = dict(existing)
    unresolved: set[tuple[str, str]] = set()
    for root in roots:
        for directory in LOCKS.values():
            locked = lock_packages_at(Path(root) / directory)
            resolved: dict[tuple[str, str], dict] = {}
            for target in REFRESH_TARGETS:
                metadata = cargo_metadata_at(Path(root) / directory, target)
                for package in (metadata or {}).get("packages", []):
                    if package.get("source"):
                        resolved[(package["name"], package["version"])] = package
            for key in sorted(locked):
                package = resolved.get(key) or manifest_package(registry, key)
                if package is None:
                    if key not in merged:
                        unresolved.add(key)
                    continue
                license_expression = package.get("license") or ""
                elected = elect(license_expression) if license_expression else None
                copyright_lines: list[str] = []
                for base in registry:
                    candidate = Path(base) / f"{key[0]}-{key[1]}"
                    if candidate.is_dir():
                        copyright_lines = source_copyright(candidate)
                        break
                if not copyright_lines:
                    copyright_lines = [f"Authors: {', '.join(package.get('authors') or [])}"] if package.get("authors") else []
                merged[key] = {
                    "name": key[0], "version": key[1], "license": license_expression,
                    "elected": elected or [], "repository": package.get("repository") or "",
                    "copyright": copyright_lines,
                }
    for key in sorted(unresolved):
        print(f"cannot resolve offline and not in rust-crates.json: {key[0]} {key[1]}", file=sys.stderr)
    entries = [merged[key] for key in sorted(merged)]
    path.write_text(json.dumps({"crates": entries}, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"rust-crates.json: {len(entries)} crates")
    return 1 if unresolved else 0


def manifest_package(registry: list[str], key: tuple[str, str]) -> dict | None:
    """Crates outside the refresh targets (other-OS, wasm) still need a licence entry."""
    for base in registry:
        manifest = Path(base) / f"{key[0]}-{key[1]}" / "Cargo.toml"
        if manifest.is_file():
            package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
            return {"license": package.get("license") or "", "repository": package.get("repository") or "",
                    "authors": package.get("authors") or []}
    return None


def lock_packages_at(directory: Path) -> set[tuple[str, str]]:
    text = (directory / "Cargo.lock").read_text(encoding="utf-8")
    return set(re.findall(r'name = "([^"]+)"\nversion = "([^"]+)"\nsource = "registry', text))


def cargo_metadata_at(directory: Path, target: str) -> dict | None:
    result = subprocess.run(
        ["cargo", "metadata", "--offline", "--locked", "--format-version", "1", "--filter-platform", target],
        cwd=directory, capture_output=True, text=True,
    )
    return json.loads(result.stdout) if result.returncode == 0 and result.stdout else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="verify coverage, policy, and that the notices file is current")
    parser.add_argument("--refresh", action="store_true", help="rebuild rust-crates.json from cargo metadata")
    parser.add_argument("--root", action="append", default=[], help="extra checkout to read Cargo.lock/metadata from (repeatable; default: this repository)")
    arguments = parser.parse_args()
    if arguments.refresh:
        return refresh(arguments.root + [str(ROOT)])
    if arguments.check:
        failures = check()
        if failures:
            print("Third-party notices policy failed:")
            for failure in failures:
                print(f"  {failure}")
            return 1
        print(f"Third-party notices policy passed ({len(load_crates())} crates, "
              f"{len(read_json(LICENSES / 'components.json')['swift'])} Swift packages)")
        return 0
    NOTICES.write_text(generate(), encoding="utf-8")
    print(f"wrote {NOTICES.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
