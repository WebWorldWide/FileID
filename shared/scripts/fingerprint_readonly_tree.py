#!/usr/bin/env python3
import argparse
import collections
import hashlib
import heapq
import json
import os
import secrets
import stat
from datetime import datetime, timezone
from pathlib import Path


def update_field(digest: "hashlib._Hash", value: object) -> None:
    data = str(value).encode("utf-8", "surrogateescape")
    digest.update(len(data).to_bytes(8, "big"))
    digest.update(data)


def fingerprint(root: Path, sample_count: int, max_sample_bytes: int) -> dict[str, object]:
    root = root.resolve(strict=True)
    if not root.is_dir():
        raise ValueError("root must be a directory")

    digest = hashlib.sha256()
    root_info = os.stat(root, follow_symlinks=False)
    update_field(digest, ".")
    update_field(digest, "directory")
    update_field(digest, stat.S_IMODE(root_info.st_mode))
    update_field(digest, root_info.st_size)
    update_field(digest, root_info.st_mtime_ns)
    counts = collections.Counter()
    extensions = collections.Counter()
    errors = collections.Counter()
    total_bytes = 0
    candidates: list[tuple[int, str, int, int]] = []
    stack: list[tuple[Path, str]] = [(root, "")]

    while stack:
        directory, relative_dir = stack.pop()
        try:
            entries = sorted(os.scandir(directory), key=lambda item: os.fsencode(item.name))
        except OSError as error:
            errors[f"scandir:{error.errno}"] += 1
            continue

        child_dirs: list[tuple[Path, str]] = []
        for entry in entries:
            relative = f"{relative_dir}/{entry.name}" if relative_dir else entry.name
            try:
                info = entry.stat(follow_symlinks=False)
            except OSError as error:
                errors[f"stat:{error.errno}"] += 1
                continue

            mode = info.st_mode
            if stat.S_ISREG(mode):
                kind = "file"
                counts["files"] += 1
                total_bytes += info.st_size
                suffix = Path(entry.name).suffix.lower()
                extensions[suffix if suffix else "<none>"] += 1
                if info.st_size <= max_sample_bytes and sample_count > 0:
                    rank = int.from_bytes(
                        hashlib.sha256(relative.encode("utf-8", "surrogateescape")).digest(),
                        "big",
                    )
                    candidate = (-rank, relative, info.st_size, info.st_mtime_ns)
                    if len(candidates) < sample_count:
                        heapq.heappush(candidates, candidate)
                    elif candidate > candidates[0]:
                        heapq.heapreplace(candidates, candidate)
            elif stat.S_ISDIR(mode):
                kind = "directory"
                counts["directories"] += 1
                child_dirs.append((Path(entry.path), relative))
            elif stat.S_ISLNK(mode):
                kind = "symlink"
                counts["symlinks"] += 1
            else:
                kind = "other"
                counts["other"] += 1

            update_field(digest, relative)
            update_field(digest, kind)
            update_field(digest, stat.S_IMODE(mode))
            update_field(digest, info.st_size)
            update_field(digest, info.st_mtime_ns)
            if kind == "symlink":
                try:
                    update_field(digest, os.readlink(entry.path))
                except OSError as error:
                    errors[f"readlink:{error.errno}"] += 1

        stack.extend(reversed(child_dirs))

    samples = []
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    for _, relative, expected_size, expected_mtime_ns in sorted(candidates, reverse=True):
        path = root / relative
        content = hashlib.sha256()
        try:
            descriptor = os.open(path, os.O_RDONLY | nofollow)
            try:
                live = os.fstat(descriptor)
                if live.st_size != expected_size or live.st_mtime_ns != expected_mtime_ns:
                    errors["sample:changed"] += 1
                    continue
                with os.fdopen(descriptor, "rb", closefd=False) as handle:
                    while chunk := handle.read(1024 * 1024):
                        content.update(chunk)
            finally:
                os.close(descriptor)
            samples.append(
                {
                    "pathSha256": hashlib.sha256(
                        relative.encode("utf-8", "surrogateescape")
                    ).hexdigest(),
                    "size": expected_size,
                    "sha256": content.hexdigest(),
                }
            )
        except OSError as error:
            errors[f"sample:{error.errno}"] += 1

    return {
        "schemaVersion": 2,
        "generatedAt": datetime.now(timezone.utc).isoformat(),
        "rootName": root.name,
        "counts": dict(sorted(counts.items())),
        "totalFileBytes": total_bytes,
        "metadataSha256": digest.hexdigest(),
        "extensionCounts": dict(sorted(extensions.items())),
        "contentSamples": samples,
        "errors": dict(sorted(errors.items())),
    }


def write_output_atomically(output: Path, root: Path, encoded: str) -> None:
    parent = output.parent.resolve(strict=True)
    resolved_output = parent / output.name
    if resolved_output == root or root in resolved_output.parents:
        raise ValueError("--output must be outside the fingerprinted root")

    temporary_name = f".{output.name}.fileid-{os.getpid()}-{secrets.token_hex(8)}.tmp"
    if os.name == "nt":
        temporary_path = parent / temporary_name
        opened_parent = os.stat(parent, follow_symlinks=False)
        descriptor: int | None = None
        try:
            descriptor = os.open(
                temporary_path,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
                0o600,
            )
            with os.fdopen(descriptor, "w", encoding="utf-8", closefd=True) as handle:
                descriptor = None
                handle.write(encoded)
                handle.flush()
                os.fsync(handle.fileno())
            current_parent = os.stat(parent, follow_symlinks=False)
            if (opened_parent.st_dev, opened_parent.st_ino) != (
                current_parent.st_dev,
                current_parent.st_ino,
            ):
                raise ValueError("--output parent changed during validation")
            os.replace(temporary_path, resolved_output)
        finally:
            if descriptor is not None:
                os.close(descriptor)
            try:
                os.unlink(temporary_path)
            except FileNotFoundError:
                pass
        return

    directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    directory_fd = os.open(parent, directory_flags)
    descriptor: int | None = None
    try:
        opened = os.fstat(directory_fd)
        current = os.stat(parent, follow_symlinks=False)
        if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
            raise ValueError("--output parent changed during validation")
        descriptor = os.open(
            temporary_name,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
            0o600,
            dir_fd=directory_fd,
        )
        with os.fdopen(descriptor, "w", encoding="utf-8", closefd=True) as handle:
            descriptor = None
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(
            temporary_name,
            output.name,
            src_dir_fd=directory_fd,
            dst_dir_fd=directory_fd,
        )
        os.fsync(directory_fd)
    finally:
        if descriptor is not None:
            os.close(descriptor)
        try:
            os.unlink(temporary_name, dir_fd=directory_fd)
        except FileNotFoundError:
            pass
        os.close(directory_fd)


def main() -> int:
    parser = argparse.ArgumentParser(description="Read-only filesystem-tree fingerprint")
    parser.add_argument("root", type=Path)
    parser.add_argument("--samples", type=int, default=16)
    parser.add_argument("--max-sample-bytes", type=int, default=8 * 1024 * 1024)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.samples < 0 or args.samples > 1_000:
        parser.error("--samples must be between 0 and 1000")
    if args.max_sample_bytes < 0:
        parser.error("--max-sample-bytes must be non-negative")

    root = args.root.resolve(strict=True)

    result = fingerprint(root, args.samples, args.max_sample_bytes)
    encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        try:
            write_output_atomically(args.output, root, encoded)
        except (OSError, ValueError) as error:
            parser.error(str(error))
    else:
        print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
