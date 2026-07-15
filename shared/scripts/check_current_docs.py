#!/usr/bin/env python3
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def main() -> int:
    failures: list[str] = []
    manifest = read("packaging/flatpak/io.github.fileid.FileID.yaml")
    runtime_match = re.search(r"^runtime-version:\s*['\"]?([0-9]+)", manifest, re.M)
    if not runtime_match:
        failures.append("Flatpak runtime-version is missing")
        runtime = ""
    else:
        runtime = runtime_match.group(1)

    if "build-args:" in manifest or "sh.rustup.rs" in manifest:
        failures.append("Flatpak manifest restores build-network/rustup bootstrap")
    cargo_builds = [line.strip() for line in manifest.splitlines() if "cargo build" in line]
    if not cargo_builds or any("--locked --offline" not in line for line in cargo_builds):
        failures.append("every Flatpak cargo build must be locked and offline")

    for path in (
        "packaging/README.md",
        "packaging/appimage/README.md",
        "shared/docs/CONTRIBUTING.md",
    ):
        if runtime and f"GNOME {runtime}" not in read(path):
            failures.append(f"{path} does not name current GNOME {runtime} runtime")

    migrations = read("platforms/windows/src/engine/src/db/migrations.rs")
    registry = migrations.split("fn registry()", 1)[-1].split("/// v12:", 1)[0]
    versions = [int(value) for value in re.findall(r'\("v([0-9]+)_', registry)]
    if not versions:
        failures.append("could not derive latest database migration")
    else:
        latest = max(versions)
        if f"through v{latest}" not in read("shared/docs/ARCHITECTURE.md"):
            failures.append(f"ARCHITECTURE.md does not name latest migration v{latest}")

    root_readme = read("README.md")
    if "./build.sh -linux" not in root_readme:
        failures.append("README Linux quickstart does not use the launch dispatcher")
    if "Pre-built release binaries aren't published" in root_readme:
        failures.append("README still claims no release artifacts are published")

    if failures:
        print("Current-document contract failed:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    print("Current-document contract passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
