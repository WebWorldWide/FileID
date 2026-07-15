#!/usr/bin/env python3
import argparse
import hashlib
import json
import sys
import tomllib
from pathlib import Path

REGISTRY_PREFIX = "registry+https://github.com/rust-lang/crates.io-index"


def generate(lock_path: Path) -> list[dict[str, object]]:
    packages = tomllib.loads(lock_path.read_text(encoding="utf-8"))["package"]
    sources: list[dict[str, object]] = []
    seen: set[tuple[str, str]] = set()

    for package in packages:
        source = package.get("source")
        if source is None:
            continue
        if source != REGISTRY_PREFIX:
            raise ValueError(
                f"unsupported Cargo source for {package['name']}: {source}; "
                "pin and handle it explicitly before regenerating"
            )
        name = package["name"]
        version = package["version"]
        checksum = package.get("checksum")
        if not checksum:
            raise ValueError(f"missing checksum for {name} {version}")
        key = (name, version)
        if key in seen:
            continue
        seen.add(key)
        destination = f"cargo/vendor/{name}-{version}"
        sources.extend(
            [
                {
                    "type": "archive",
                    "archive-type": "tar-gzip",
                    "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                    "sha256": checksum,
                    "dest": destination,
                },
                {
                    "type": "inline",
                    "contents": json.dumps(
                        {"package": checksum, "files": {}}, separators=(",", ":")
                    ),
                    "dest": destination,
                    "dest-filename": ".cargo-checksum.json",
                },
            ]
        )

    sources.append(
        {
            "type": "inline",
            "contents": (
                '[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
                '[source.vendored-sources]\ndirectory = "cargo/vendor"\n'
            ),
            "dest": "cargo",
            "dest-filename": "config.toml",
        }
    )
    return sources


def encoded(sources: list[dict[str, object]]) -> str:
    return json.dumps(sources, indent=2, ensure_ascii=False) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument(
        "--lock", type=Path, default=Path("platforms/linux/Cargo.lock")
    )
    parser.add_argument(
        "--output", type=Path, default=Path("packaging/flatpak/cargo-sources.json")
    )
    args = parser.parse_args()

    try:
        content = encoded(generate(args.lock))
    except (OSError, KeyError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"Flatpak Cargo source generation failed: {error}", file=sys.stderr)
        return 1

    if args.check:
        try:
            existing = args.output.read_text(encoding="utf-8")
        except OSError as error:
            print(f"Flatpak Cargo sources are missing: {error}", file=sys.stderr)
            return 1
        if existing != content:
            digest = hashlib.sha256(content.encode()).hexdigest()
            print(
                "Flatpak Cargo sources are stale; run "
                f"{Path(__file__).as_posix()} (expected sha256 {digest})",
                file=sys.stderr,
            )
            return 1
        print(f"Flatpak Cargo sources match {args.lock}")
        return 0

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(content, encoding="utf-8", newline="\n")
    print(f"Wrote {args.output} with {len(generate(args.lock)) - 1} source entries")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
