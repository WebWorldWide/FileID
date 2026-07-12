#!/usr/bin/env python3
"""Stage and archive the already-built FileID CLI/TUI/engine toolset."""

from __future__ import annotations

import argparse
import hashlib
import platform
import shutil
import subprocess
import sys
import sysconfig
import tomllib
from pathlib import Path


def windows_runtime_sources(root: Path) -> dict[str, Path]:
    machine = "-".join(
        filter(
            None,
            [
                platform.machine().lower(),
                sysconfig.get_platform().lower(),
            ],
        )
    )
    if "arm64" in machine or "aarch64" in machine:
        architecture = "arm64"
    elif "amd64" in machine or "x86_64" in machine:
        architecture = "x64"
    else:
        raise SystemExit(f"unsupported Windows architecture: {platform.machine()}")

    powershell = shutil.which("pwsh") or shutil.which("powershell")
    if powershell is None:
        raise SystemExit("pwsh or powershell is required to stage Windows runtime DLLs")
    script = root / "platforms/windows/build/fetch-runtime-deps.ps1"
    result = subprocess.run(
        [
            powershell,
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            str(script),
            "-Architecture",
            architecture,
        ],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(
            "Windows runtime staging failed:\n"
            + result.stdout
            + ("\n" if result.stdout and result.stderr else "")
            + result.stderr
        )
    resolved: dict[str, Path] = {}
    for line in result.stdout.splitlines():
        if line.startswith("RUNTIME_DLL="):
            path = Path(line.removeprefix("RUNTIME_DLL="))
            resolved[path.name] = path
    required = {
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
        "DirectML.dll",
        "pdfium.dll",
    }
    missing = sorted(required - resolved.keys())
    if missing:
        raise SystemExit(
            "runtime fetch did not resolve required DLL(s): " + ", ".join(missing)
        )
    return {name: resolved[name] for name in sorted(required)}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--label", required=True, help="artifact suffix, e.g. windows-X64")
    parser.add_argument(
        "--version",
        default="auto",
        help="FileID version, or 'auto' to read platforms/windows/VERSION",
    )
    parser.add_argument("--output", type=Path, default=Path("dist/tools"))
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    version = args.version
    if version == "auto":
        version = (root / "platforms/windows/VERSION").read_text(encoding="utf-8").strip()
    version = version.removeprefix("v")
    manifests = (
        root / "platforms/windows/src/engine/Cargo.toml",
        root / "platforms/cli/Cargo.toml",
        root / "platforms/tui/Cargo.toml",
    )
    for manifest in manifests:
        package_version = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]["version"]
        if package_version != version:
            raise SystemExit(
                f"version mismatch: {manifest} declares {package_version}, expected {version}"
            )
    windows = sys.platform == "win32"
    exe = ".exe" if windows else ""
    release = Path("target/release")
    sources = {
        f"fileid{exe}": root / "platforms/cli" / release / f"fileid{exe}",
        f"fileid-tui{exe}": root / "platforms/tui" / release / f"fileid-tui{exe}",
        f"FileIDEngine{exe}": root
        / "platforms/windows/src/engine"
        / release
        / f"FileIDEngine{exe}",
    }
    if windows:
        sources.update(windows_runtime_sources(root))

    missing = [str(path) for path in sources.values() if not path.is_file()]
    if missing:
        raise SystemExit("missing release artifact(s):\n  " + "\n  ".join(missing))

    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    stem = f"FileID-tools-{version}-{args.label}"
    stage = output / stem
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir()
    for name, source in sources.items():
        shutil.copy2(source, stage / name)
    shutil.copy2(root / "LICENSE", stage / "LICENSE")

    runtime = (
        "Keep all DLLs beside the three executables."
        if windows
        else (
            "For full AI scans on macOS, run `fileid runtime status` and install "
            "Homebrew ONNX Runtime when requested."
            if sys.platform == "darwin"
            else "The Linux engine contains the CPU ONNX Runtime."
        )
    )
    (stage / "README.txt").write_text(
        "FileID command-line tools\n\n"
        "Add this directory to PATH. Run `fileid --help` or `fileid-tui --help`.\n"
        "Install AI weights explicitly with `fileid models download --all`.\n"
        f"{runtime}\n\nNo telemetry. Apache-2.0.\n",
        encoding="utf-8",
    )

    archive_format = "zip" if windows else "gztar"
    archive = Path(shutil.make_archive(str(output / stem), archive_format, output, stem))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(f"{archive.name}.sha256").write_text(
        f"{digest}  {archive.name}\n", encoding="ascii"
    )
    print(archive)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
