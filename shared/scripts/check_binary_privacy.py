#!/usr/bin/env python3
"""Fail when a shipped binary contains a known telemetry endpoint marker.

`--ort-runtime LIB` also loads an ONNX Runtime shared library the way FileID's
engines do (ORT_DISABLE_TELEMETRY=1, no CI variables, network blocked where the
OS allows it) and fails if creating an environment leaves any file in a
throwaway home folder. Official ONNX Runtime builds from 1.29 on queue
telemetry there before uploading it.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


FORBIDDEN = (
    b"sentry.io",
    b"io.sentry",
    b"applicationinsights",
    b"applicationinsights.azure.com",
    b"googletagmanager",
    b"google-analytics.com",
    b"segment.io",
    b"segment.com",
    b"mixpanel.com",
    b"amplitude.com",
    b"posthog.com",
    b"datadoghq",
    b"bugsnag",
    b"rollbar.com",
    b"honeycomb.io",
    b"newrelic.com",
    b"raygun.io",
    b"firebase",
    b"firebaseio.com",
    b"appcenter.ms",
    b"in.appcenter.ms",
    b"crashpad",
    b"breakpad",
    # Microsoft's 1DS uploader, as compiled into official ONNX Runtime builds
    # from 1.29 on. "telemetry flush" and "DeviceId:" are not markers: they
    # appear in ONNX Runtime builds that carry no uploader at all.
    b"mobile.events.data.microsoft.com",
    b"onecollector",
    b"microsoft/developertools/.onnxruntime",
)

LIBRARY_SUFFIXES = {".exe", ".dll", ".dylib", ".so"}

# Variables ONNX Runtime reads to suppress telemetry on CI machines. The
# behaviour check drops them so only FileID's own setting is tested.
ORT_CI_VARIABLES = (
    "CI", "TF_BUILD", "GITHUB_ACTIONS", "GITLAB_CI", "CIRCLECI", "TRAVIS", "JENKINS_URL",
    "CODEBUILD_BUILD_ID", "BUILDKITE", "TEAMCITY_VERSION", "APPVEYOR", "BITBUCKET_BUILD_NUMBER",
    "SYSTEM_TEAMFOUNDATIONCOLLECTIONURI", "ORT_RUNNING_UNIT_TESTS",
)

# Creates and releases one environment through the stable C API table
# (CreateEnv = 3, DisableTelemetryEvents = 6, ReleaseEnv = 92 since API 1).
ORT_PROBE = r"""
import ctypes, os, sys
library = sys.argv[1]
if hasattr(os, "add_dll_directory"):
    os.add_dll_directory(os.path.dirname(os.path.abspath(library)))
ort = ctypes.CDLL(library)
get_base = ort.OrtGetApiBase
get_base.restype = ctypes.c_void_p
base = get_base()
pointer = ctypes.sizeof(ctypes.c_void_p)
get_api = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_uint32)(ctypes.c_void_p.from_address(base).value)
api = get_api(1)
def entry(index, restype, *argtypes):
    return ctypes.CFUNCTYPE(restype, *argtypes)(ctypes.c_void_p.from_address(api + index * pointer).value)
create_env = entry(3, ctypes.c_void_p, ctypes.c_int, ctypes.c_char_p, ctypes.POINTER(ctypes.c_void_p))
disable_telemetry = entry(6, ctypes.c_void_p, ctypes.c_void_p)
release_env = entry(92, None, ctypes.c_void_p)
env = ctypes.c_void_p()
if create_env(3, b"fileid-privacy-check", ctypes.byref(env)):
    sys.exit("CreateEnv failed")
disable_telemetry(env)
release_env(env)
"""

MACOS_DENY_NETWORK = "(version 1)(allow default)(deny network-outbound)(deny network-bind)"


def resolve_binary_paths(paths: list[Path]) -> tuple[list[Path], list[str]]:
    binaries: list[Path] = []
    failures: list[str] = []
    for path in paths:
        if path.is_file():
            binaries.append(path)
        elif path.is_dir():
            discovered = sorted(
                candidate
                for candidate in path.rglob("*")
                if candidate.is_file()
                and (candidate.suffix.lower() in LIBRARY_SUFFIXES or ".so." in candidate.name.lower())
            )
            if discovered:
                binaries.extend(discovered)
            else:
                failures.append(f"no EXE/DLL/dylib/so binaries found under: {path}")
        else:
            failures.append(f"missing binary: {path}")
    return list(dict.fromkeys(binaries)), failures


def scan_binary_paths(paths: list[Path]) -> list[str]:
    failures: list[str] = []
    for path in paths:
        data = path.read_bytes().lower()
        for marker in FORBIDDEN:
            text = marker.decode("ascii")
            variants = (marker, text.encode("utf-16le"), text.encode("utf-16be"))
            if any(variant in data for variant in variants):
                failures.append(f"{path}: {text}")
    return failures


def scan(paths: list[Path]) -> list[str]:
    binaries, failures = resolve_binary_paths(paths)
    failures.extend(scan_binary_paths(binaries))
    return failures


def check_ort_runtime(library: Path, *, fileid_environment: bool = True, timeout: float = 60) -> list[str]:
    """Load `library` with FileID's ONNX Runtime settings in a child process
    whose home folders are empty temporaries; report every file it leaves there."""
    if not library.is_file():
        return [f"missing ONNX Runtime library: {library}"]
    with tempfile.TemporaryDirectory(prefix="fileid-ort-privacy-") as directory:
        root = Path(directory)
        homes = {name: root / name for name in ("home", "xdg-cache", "local-app-data", "roaming-app-data")}
        for folder in (*homes.values(), root / "tmp"):
            folder.mkdir()
        env = {key: value for key, value in os.environ.items() if key in ("PATH", "SYSTEMROOT", "WINDIR")}
        env.update({
            "HOME": str(homes["home"]), "CFFIXED_USER_HOME": str(homes["home"]), "USERPROFILE": str(homes["home"]),
            "XDG_CACHE_HOME": str(homes["xdg-cache"]), "LOCALAPPDATA": str(homes["local-app-data"]),
            "APPDATA": str(homes["roaming-app-data"]), "TMPDIR": str(root / "tmp"), "TEMP": str(root / "tmp"),
            "TMP": str(root / "tmp"), "HTTPS_PROXY": "http://127.0.0.1:9", "HTTP_PROXY": "http://127.0.0.1:9",
            "ALL_PROXY": "http://127.0.0.1:9", "NO_PROXY": "",
        })
        for variable in ORT_CI_VARIABLES:
            env.pop(variable, None)
        if fileid_environment:
            env["ORT_DISABLE_TELEMETRY"] = "1"
        command = [sys.executable, "-I", "-c", ORT_PROBE, str(library.resolve())]
        if sys.platform == "darwin" and shutil.which("sandbox-exec"):
            command = ["sandbox-exec", "-p", MACOS_DENY_NETWORK, *command]
        try:
            result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=timeout, check=False)
        except subprocess.TimeoutExpired:
            return [f"{library}: ONNX Runtime probe timed out"]
        if result.returncode != 0:
            detail = (result.stderr.strip().splitlines() or ["no output"])[-1]
            return [f"{library}: could not create an ONNX Runtime environment ({detail})"]
        return [
            f"{library}: ONNX Runtime wrote {path.relative_to(root).as_posix()}"
            for folder in homes.values()
            for path in sorted(folder.rglob("*"))
            if path.is_file()
        ]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", nargs="*", type=Path)
    parser.add_argument("--ort-runtime", action="append", default=[], type=Path, metavar="LIB",
                        help="also load this ONNX Runtime library with FileID's settings and fail on any telemetry state")
    args = parser.parse_args()
    if not args.binary and not args.ort_runtime:
        parser.error("nothing to check")

    binaries, failures = resolve_binary_paths(args.binary + args.ort_runtime)
    failures.extend(scan_binary_paths(binaries))
    for library in args.ort_runtime:
        failures.extend(check_ort_runtime(library))
    if failures:
        print("Telemetry markers found in shipped binaries:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    checked = f" (+{len(args.ort_runtime)} ONNX Runtime load check(s))" if args.ort_runtime else ""
    print(f"Privacy scan clean: {len(binaries)} binary/binaries{checked}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
