#!/usr/bin/env python3
"""Fail when a shipped binary contains a known telemetry endpoint marker."""

from __future__ import annotations

import argparse
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
)


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
                if candidate.is_file() and candidate.suffix.lower() in {".exe", ".dll"}
            )
            if discovered:
                binaries.extend(discovered)
            else:
                failures.append(f"no EXE/DLL binaries found under: {path}")
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", nargs="+", type=Path)
    args = parser.parse_args()

    binaries, failures = resolve_binary_paths(args.binary)
    failures.extend(scan_binary_paths(binaries))
    if failures:
        print("Telemetry markers found in shipped binaries:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    print(f"Privacy scan clean: {len(binaries)} binary/binaries")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
