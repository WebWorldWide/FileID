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


def scan(paths: list[Path]) -> list[str]:
    failures: list[str] = []
    for path in paths:
        if not path.is_file():
            failures.append(f"missing binary: {path}")
            continue
        data = path.read_bytes().lower()
        for marker in FORBIDDEN:
            if marker in data:
                failures.append(f"{path}: {marker.decode('ascii')}")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", nargs="+", type=Path)
    args = parser.parse_args()

    failures = scan(args.binary)
    if failures:
        print("Telemetry markers found in shipped binaries:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    print(f"Privacy scan clean: {len(args.binary)} binary/binaries")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
