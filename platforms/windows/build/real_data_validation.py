#!/usr/bin/env python3
"""Read-only corpus validation against an isolated clone of a FileID catalog."""

from __future__ import annotations

import argparse
import collections
import contextlib
import ctypes
import hashlib
import heapq
import json
import math
import os
import re
import shutil
import sqlite3
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PureWindowsPath
from typing import Any, Iterable, Iterator


QUALITY_FLOOR = 0.33
PEOPLE_MIN_FACES_PER_CLUSTER = 6
PERSON_DISPLAY_NAME_SQL = (
    "COALESCE(NULLIF(TRIM(p.name),''),"
    "NULLIF(TRIM(COALESCE(p.title,'') || ' ' || "
    "COALESCE(p.first_name,'') || ' ' || COALESCE(p.middle_name,'') || ' ' || "
    "COALESCE(p.last_name,'') || ' ' || COALESCE(p.suffix,'')),''),'')"
)
RESTRUCTURE_PREVIEW_CAP = 5_000
RESTRUCTURE_PAGE_SIZE = 1_000
ALLOWED_CONFIDENCE = {"auto", "review", "ask"}
ALLOWED_TIERS = {"Anchor", "Mixed", "Junk"}
WINDOWS_INVALID_COMPONENT_CHARACTERS = frozenset('<>:"/\\|?*')
WINDOWS_RESERVED_COMPONENTS = frozenset(
    {
        "CON",
        "PRN",
        "AUX",
        "NUL",
        *(f"COM{index}" for index in range(10)),
        *(f"LPT{index}" for index in range(10)),
        "COM¹",
        "COM²",
        "COM³",
        "LPT¹",
        "LPT²",
        "LPT³",
    }
)
CONTROLLED_FILEID_ENV = {
    "FILEID_DB",
    "FILEID_LOG",
    "FILEID_MODELS_DIR",
    "FILEID_RESTRUCTURE_LARGE_PLAN_THRESHOLD",
}
FORBIDDEN_MUTATING_COMMAND_KINDS = {
    "applyRestructure",
    "bulkAction",
    "deleteFiles",
    "renameFile",
    "scan",
    "startScan",
    "trashFiles",
    "undoRestructure",
}
VLM_CANONICAL_IDS = {
    "gemma-3-4b": "gemma_3_4b",
    "gemma_3_4b": "gemma_3_4b",
    "mistral-small-3.2": "mistral_small_3_2",
    "mistral_small_3_2": "mistral_small_3_2",
    "qwen2.5-vl-7b": "qwen2_5_vl_7b",
    "qwen2_5_vl_7b": "qwen2_5_vl_7b",
}
VLM_ALIASES = {
    "gemma_3_4b": "gemma-3-4b",
    "mistral_small_3_2": "mistral-small-3.2",
    "qwen2_5_vl_7b": "qwen2.5-vl-7b",
}
BAD_LOG_MARKER = re.compile(
    r"(?i)(\bERROR\b|\bpanic(?:ked)?\b|\bfatal\b|access violation|0xC0000005)"
)
HARNESS_COMMAND_KINDS = {
    "cancelRestructure",
    "deepAnalyzeAll",
    "deepAnalyzeCancel",
    "findMergeSuggestions",
    "healthCheck",
    "planRestructure",
    "runFaceClustering",
    "shutdown",
}
if overlap := HARNESS_COMMAND_KINDS & FORBIDDEN_MUTATING_COMMAND_KINDS:
    raise RuntimeError(
        f"read-only harness command whitelist contains mutating commands: {sorted(overlap)}"
    )
KNOWN_EVENT_KINDS = {
    "batchSummary",
    "bulkActionResult",
    "clipTextEmbedding",
    "deepAnalyzeComplete",
    "deepAnalyzeFileDone",
    "deepAnalyzeProgress",
    "deepAnalyzeStarting",
    "discoveryComplete",
    "error",
    "faceClusteringComplete",
    "fileDone",
    "hardwareReprobed",
    "healthCheckResult",
    "libraryWiped",
    "log",
    "mergeSuggestions",
    "modelDownloadProgress",
    "phaseChanged",
    "progress",
    "queueState",
    "ready",
    "restructureApplyResult",
    "restructurePlan",
    "scanComplete",
    "thumbnailGenerated",
}
OPERATION_TERMINAL_KINDS = {
    "bulkActionResult",
    "deepAnalyzeComplete",
    "error",
    "faceClusteringComplete",
    "libraryWiped",
    "mergeSuggestions",
    "restructureApplyResult",
    "restructurePlan",
    "scanComplete",
}

_RETENTION_STATE: Path | None = None
_RETENTION_STATE_IDENTITY: tuple[int, int] | None = None
_RETENTION_ARTIFACTS: Path | None = None
_RETENTION_MARKER: str | None = None
_RETENTION_KEEP = True
_RETENTION_DELETE_APPROVED = False
_RETENTION_CLEANUP_DISPOSITION = "not-created"


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def normalized(path: str | Path) -> str:
    return os.path.normcase(os.path.normpath(os.path.abspath(os.fspath(path))))


def under_root(path: str | Path, root: str | Path) -> bool:
    try:
        return os.path.commonpath((normalized(path), normalized(root))) == normalized(root)
    except ValueError:
        return False


def require_outside(path: Path, root: Path, label: str) -> None:
    if under_root(path, root):
        raise ValueError(f"{label} must remain outside the corpus root: {path}")


def require_disjoint(
    path: Path, protected_paths: Iterable[Path], label: str
) -> None:
    for protected in protected_paths:
        if under_root(path, protected) or under_root(protected, path):
            raise ValueError(
                f"{label} overlaps protected input {protected}: {path}"
            )


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while block := handle.read(8 * 1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def is_reparse_point(info: os.stat_result) -> bool:
    attributes = int(getattr(info, "st_file_attributes", 0))
    return stat.S_ISLNK(info.st_mode) or bool(
        attributes & int(getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))
    )


def full_tree_manifest(root: Path) -> dict[str, Any]:
    root = root.resolve(strict=True)
    if not root.is_dir():
        raise ValueError(f"manifest root is not a directory: {root}")
    digest = hashlib.sha256()
    file_count = 0
    total_bytes = 0
    stack: list[tuple[Path, str]] = [(root, "")]
    while stack:
        directory, relative_directory = stack.pop()
        with os.scandir(directory) as iterator:
            entries = sorted(iterator, key=lambda entry: os.fsencode(entry.name))
        child_directories: list[tuple[Path, str]] = []
        for entry in entries:
            relative = (
                f"{relative_directory}/{entry.name}"
                if relative_directory
                else entry.name
            )
            info = entry.stat(follow_symlinks=False)
            if is_reparse_point(info):
                raise ValueError(f"reparse point is not allowed in manifest: {entry.path}")
            if stat.S_ISDIR(info.st_mode):
                child_directories.append((Path(entry.path), relative))
                continue
            if not stat.S_ISREG(info.st_mode):
                raise ValueError(f"non-regular model entry is not allowed: {entry.path}")
            content_sha256 = sha256_file(Path(entry.path))
            encoded = json.dumps(
                [relative.replace("\\", "/"), info.st_size, content_sha256],
                separators=(",", ":"),
                ensure_ascii=False,
            ).encode("utf-8")
            digest.update(len(encoded).to_bytes(8, "big"))
            digest.update(encoded)
            file_count += 1
            total_bytes += info.st_size
        stack.extend(reversed(child_directories))
    return {
        "files": file_count,
        "bytes": total_bytes,
        "sha256": digest.hexdigest(),
    }


def file_snapshot(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {"path": str(path), "exists": False, "size": 0, "sha256": None}
    info = path.stat()
    if not stat.S_ISREG(info.st_mode):
        raise ValueError(f"expected regular file: {path}")
    return {
        "path": str(path),
        "exists": True,
        "size": info.st_size,
        "modifiedNS": info.st_mtime_ns,
        "sha256": sha256_file(path),
    }


def stable_fingerprint(value: dict[str, Any]) -> dict[str, Any]:
    result = dict(value)
    result.pop("generatedAt", None)
    return result


def extended_windows_path(path: Path) -> Path:
    raw = os.fspath(path)
    if os.name == "nt" and raw.startswith("\\\\?\\"):
        return Path(raw)
    absolute = os.path.abspath(raw)
    if os.name != "nt":
        return Path(absolute)
    if absolute.startswith("\\\\"):
        return Path("\\\\?\\UNC\\" + absolute[2:])
    return Path("\\\\?\\" + absolute)


def path_identity_no_reparse(
    path: Path, *, expect_directory: bool | None = None
) -> tuple[os.stat_result, tuple[int, int]]:
    info = os.stat(extended_windows_path(path), follow_symlinks=False)
    if is_reparse_point(info):
        raise ValueError(f"reparse point is not allowed: {path}")
    if expect_directory is True and not stat.S_ISDIR(info.st_mode):
        raise ValueError(f"expected a directory: {path}")
    if expect_directory is False and not stat.S_ISREG(info.st_mode):
        raise ValueError(f"expected a regular file: {path}")
    identity = (int(info.st_dev), int(info.st_ino))
    if identity == (0, 0):
        raise ValueError(f"filesystem did not expose a stable identity: {path}")
    return info, identity


def fingerprint_update(digest: Any, value: object) -> None:
    encoded = str(value).encode("utf-8", "surrogateescape")
    digest.update(len(encoded).to_bytes(8, "big"))
    digest.update(encoded)


def safe_tree_fingerprint(
    root: Path, sample_count: int, max_sample_bytes: int
) -> dict[str, Any]:
    root = extended_windows_path(root)
    root_info, root_identity = path_identity_no_reparse(
        root, expect_directory=True
    )
    digest = hashlib.sha256()
    for value in (
        ".",
        "directory",
        stat.S_IMODE(root_info.st_mode),
        root_info.st_size,
        root_info.st_mtime_ns,
        *root_identity,
    ):
        fingerprint_update(digest, value)

    counts: collections.Counter[str] = collections.Counter()
    extensions: collections.Counter[str] = collections.Counter()
    total_bytes = 0
    candidates: list[tuple[int, str, int, int, int, int]] = []
    seen_directories = {root_identity: "."}
    stack: list[tuple[Path, str, tuple[int, int]]] = [
        (root, "", root_identity)
    ]
    while stack:
        directory, relative_directory, expected_identity = stack.pop()
        before, before_identity = path_identity_no_reparse(
            directory, expect_directory=True
        )
        if before_identity != expected_identity:
            raise ValueError(f"directory identity changed before traversal: {directory}")
        with os.scandir(directory) as iterator:
            entries = sorted(iterator, key=lambda entry: os.fsencode(entry.name))
        after, after_identity = path_identity_no_reparse(
            directory, expect_directory=True
        )
        if (
            after_identity != expected_identity
            or after.st_size != before.st_size
            or after.st_mtime_ns != before.st_mtime_ns
        ):
            raise ValueError(f"directory changed during traversal: {directory}")

        child_directories: list[tuple[Path, str, tuple[int, int]]] = []
        for entry in entries:
            relative = (
                f"{relative_directory}/{entry.name}"
                if relative_directory
                else entry.name
            )
            entry_path = Path(entry.path)
            info, identity = path_identity_no_reparse(entry_path)
            if stat.S_ISREG(info.st_mode):
                kind = "file"
                counts["files"] += 1
                total_bytes += info.st_size
                suffix = entry_path.suffix.lower()
                extensions[suffix if suffix else "<none>"] += 1
                if info.st_size <= max_sample_bytes and sample_count > 0:
                    rank = int.from_bytes(
                        hashlib.sha256(
                            relative.encode("utf-8", "surrogateescape")
                        ).digest(),
                        "big",
                    )
                    candidate = (
                        -rank,
                        relative,
                        info.st_size,
                        info.st_mtime_ns,
                        identity[0],
                        identity[1],
                    )
                    if len(candidates) < sample_count:
                        heapq.heappush(candidates, candidate)
                    elif candidate > candidates[0]:
                        heapq.heapreplace(candidates, candidate)
            elif stat.S_ISDIR(info.st_mode):
                kind = "directory"
                counts["directories"] += 1
                if identity in seen_directories:
                    raise ValueError(
                        "directory identity was visited twice: "
                        f"{entry_path} and {seen_directories[identity]}"
                    )
                seen_directories[identity] = relative
                child_directories.append((entry_path, relative, identity))
            else:
                raise ValueError(f"non-regular corpus entry is not allowed: {entry_path}")
            for value in (
                relative,
                kind,
                stat.S_IMODE(info.st_mode),
                info.st_size,
                info.st_mtime_ns,
                *identity,
            ):
                fingerprint_update(digest, value)
        stack.extend(reversed(child_directories))

    samples: list[dict[str, Any]] = []
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    for (
        _rank,
        relative,
        expected_size,
        expected_mtime_ns,
        expected_device,
        expected_inode,
    ) in sorted(candidates, reverse=True):
        path = root / Path(relative)
        before, before_identity = path_identity_no_reparse(
            path, expect_directory=False
        )
        expected_identity = (expected_device, expected_inode)
        if (
            before_identity != expected_identity
            or before.st_size != expected_size
            or before.st_mtime_ns != expected_mtime_ns
        ):
            raise ValueError(f"sample changed before it was opened: {path}")
        content = hashlib.sha256()
        descriptor = os.open(path, flags)
        try:
            live = os.fstat(descriptor)
            if (
                (int(live.st_dev), int(live.st_ino)) != expected_identity
                or not stat.S_ISREG(live.st_mode)
                or live.st_size != expected_size
                or live.st_mtime_ns != expected_mtime_ns
            ):
                raise ValueError(f"sample identity changed while opening: {path}")
            with os.fdopen(descriptor, "rb", closefd=False) as handle:
                while chunk := handle.read(1024 * 1024):
                    content.update(chunk)
        finally:
            os.close(descriptor)
        final, final_identity = path_identity_no_reparse(
            path, expect_directory=False
        )
        if (
            final_identity != expected_identity
            or final.st_size != expected_size
            or final.st_mtime_ns != expected_mtime_ns
        ):
            raise ValueError(f"sample changed while it was hashed: {path}")
        samples.append(
            {
                "pathSha256": hashlib.sha256(
                    relative.encode("utf-8", "surrogateescape")
                ).hexdigest(),
                "size": expected_size,
                "sha256": content.hexdigest(),
                "device": expected_device,
                "inode": expected_inode,
            }
        )

    return {
        "schemaVersion": 3,
        "generatedAt": utc_now(),
        "rootName": root.name,
        "rootIdentity": {"device": root_identity[0], "inode": root_identity[1]},
        "directoryIdentityCount": len(seen_directories),
        "counts": dict(sorted(counts.items())),
        "totalFileBytes": total_bytes,
        "metadataSha256": digest.hexdigest(),
        "extensionCounts": dict(sorted(extensions.items())),
        "contentSamples": samples,
        "errors": {},
    }


def sqlite_integrity_snapshot(
    db_path: Path, *, immutable: bool
) -> dict[str, Any]:
    with connect_readonly(db_path, immutable=immutable) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        quick_rows = [
            str(row[0])
            for row in connection.execute("PRAGMA quick_check").fetchmany(101)
        ]
        integrity_rows = [
            str(row[0])
            for row in connection.execute("PRAGMA integrity_check").fetchmany(101)
        ]
        foreign_key_rows = [
            {
                "table": str(row[0]),
                "rowID": row[1],
                "parent": str(row[2]),
                "foreignKeyIndex": int(row[3]),
            }
            for row in connection.execute("PRAGMA foreign_key_check").fetchmany(101)
        ]
    return {
        "quickCheck": quick_rows[:100],
        "integrityCheck": integrity_rows[:100],
        "foreignKeyViolations": foreign_key_rows[:100],
        "truncated": {
            "quickCheck": len(quick_rows) > 100,
            "integrityCheck": len(integrity_rows) > 100,
            "foreignKeyViolations": len(foreign_key_rows) > 100,
        },
        "checks": {
            "quickCheckPassed": quick_rows == ["ok"],
            "integrityCheckPassed": integrity_rows == ["ok"],
            "foreignKeyCheckPassed": not foreign_key_rows,
        },
    }


def write_json(path: Path, value: Any) -> None:
    encoded = json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    temporary.write_text(encoded, encoding="utf-8")
    os.replace(temporary, path)


def cleanup_validation_state() -> str | None:
    global _RETENTION_CLEANUP_DISPOSITION

    if _RETENTION_STATE is None:
        _RETENTION_CLEANUP_DISPOSITION = "not-created"
        return None
    state = _RETENTION_STATE
    if _RETENTION_KEEP:
        _RETENTION_CLEANUP_DISPOSITION = "retained-as-requested"
        return None
    if not _RETENTION_DELETE_APPROVED:
        _RETENTION_CLEANUP_DISPOSITION = "retained-not-proven-green"
        return None
    if not state.exists():
        _RETENTION_CLEANUP_DISPOSITION = "missing-before-cleanup"
        return f"FileNotFoundError: state cleanup target disappeared: {state}"
    try:
        _root_info, root_identity = path_identity_no_reparse(
            state, expect_directory=True
        )
        if _RETENTION_STATE_IDENTITY is None:
            raise ValueError("state cleanup identity was not captured at creation")
        if root_identity != _RETENTION_STATE_IDENTITY:
            raise ValueError(
                "state cleanup target identity changed; refusing deletion: "
                f"{state}"
            )
        if normalized(state.resolve(strict=True)) != normalized(state):
            raise ValueError(f"state cleanup target resolves elsewhere: {state}")
        marker = state / ".fileid-real-data-validation-state"
        marker_info, _marker_identity = path_identity_no_reparse(
            marker, expect_directory=False
        )
        if marker_info.st_size > 4096:
            raise ValueError(f"state cleanup marker is unexpectedly large: {marker}")
        if marker.read_text(encoding="utf-8") != _RETENTION_MARKER:
            raise ValueError(f"state cleanup marker does not match this run: {marker}")

        if not bool(getattr(shutil.rmtree, "avoids_symlink_attacks", False)):
            _RETENTION_CLEANUP_DISPOSITION = "retained-unsafe-rmtree"
            return None

        seen = {root_identity}
        stack = [state]
        while stack:
            directory = stack.pop()
            before, expected_identity = path_identity_no_reparse(
                directory, expect_directory=True
            )
            with os.scandir(directory) as iterator:
                entries = list(iterator)
            after, after_identity = path_identity_no_reparse(
                directory, expect_directory=True
            )
            if (
                after_identity != expected_identity
                or before.st_mtime_ns != after.st_mtime_ns
                or before.st_size != after.st_size
            ):
                raise ValueError(f"state changed during cleanup validation: {directory}")
            for entry in entries:
                info, identity = path_identity_no_reparse(Path(entry.path))
                if stat.S_ISDIR(info.st_mode):
                    if identity in seen:
                        raise ValueError(
                            f"state contains a repeated directory identity: {entry.path}"
                        )
                    seen.add(identity)
                    stack.append(Path(entry.path))
                elif not stat.S_ISREG(info.st_mode):
                    raise ValueError(
                        f"state contains a non-regular cleanup entry: {entry.path}"
                    )
        _final_info, final_identity = path_identity_no_reparse(
            state, expect_directory=True
        )
        if final_identity != _RETENTION_STATE_IDENTITY:
            raise ValueError(
                "state cleanup target identity changed before deletion; "
                f"refusing deletion: {state}"
            )
        shutil.rmtree(state)
        if state.exists():
            raise OSError(f"state cleanup target still exists: {state}")
        _RETENTION_CLEANUP_DISPOSITION = "deleted"
        return None
    except BaseException as error:
        _RETENTION_CLEANUP_DISPOSITION = "retained-cleanup-error"
        return f"{type(error).__name__}: {error}"


def record_retention_result(cleanup_error: str | None) -> None:
    if _RETENTION_ARTIFACTS is None:
        return
    summary_path = _RETENTION_ARTIFACTS / "summary.json"
    if not summary_path.is_file():
        return
    try:
        summary = json.loads(summary_path.read_text(encoding="utf-8"))
        state_exists = bool(_RETENTION_STATE and _RETENTION_STATE.exists())
        retained_safely = (
            _RETENTION_CLEANUP_DISPOSITION.startswith("retained-")
            and state_exists
        )
        removed_safely = (
            _RETENTION_CLEANUP_DISPOSITION == "deleted" and not state_exists
        )
        summary["retention"] = {
            "stateDirectory": (
                str(_RETENTION_STATE) if _RETENTION_STATE is not None else None
            ),
            "keepStateRequested": _RETENTION_KEEP,
            "automaticDeleteApproved": _RETENTION_DELETE_APPROVED,
            "cleanupDisposition": _RETENTION_CLEANUP_DISPOSITION,
            "symlinkSafeRecursiveDeleteAvailable": bool(
                getattr(shutil.rmtree, "avoids_symlink_attacks", False)
            ),
            "stateExistsAfterCleanup": state_exists,
            "cleanupError": cleanup_error,
            "checks": {
                "stateRetentionPolicySatisfied": cleanup_error is None
                and (retained_safely or removed_safely),
            },
        }
        failed_checks = all_checks(summary)
        summary["failedChecks"] = failed_checks
        if failed_checks:
            summary["result"] = "RED"
        write_json(summary_path, summary)
    except BaseException as error:
        if cleanup_error is None:
            raise RuntimeError(f"could not record state retention result: {error}") from error


@contextlib.contextmanager
def connect_readonly(
    db_path: Path, *, immutable: bool = False
) -> Iterator[sqlite3.Connection]:
    suffix = "?mode=ro"
    if immutable:
        suffix += "&immutable=1"
    connection = sqlite3.connect(f"file:{db_path}{suffix}", uri=True, timeout=30)
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA query_only=ON")
    try:
        yield connection
    finally:
        connection.close()


def scalar(connection: sqlite3.Connection, sql: str, parameters: Iterable[Any] = ()) -> Any:
    row = connection.execute(sql, tuple(parameters)).fetchone()
    return row[0] if row is not None else None


def cluster_bins(sizes: list[int]) -> dict[str, int]:
    result = {
        "1": 0,
        "2": 0,
        "3-5": 0,
        "6-12": 0,
        "13-50": 0,
        "51-200": 0,
        "201-1000": 0,
        ">1000": 0,
    }
    for size in sizes:
        if size == 1:
            key = "1"
        elif size == 2:
            key = "2"
        elif size <= 5:
            key = "3-5"
        elif size <= 12:
            key = "6-12"
        elif size <= 50:
            key = "13-50"
        elif size <= 200:
            key = "51-200"
        elif size <= 1_000:
            key = "201-1000"
        else:
            key = ">1000"
        result[key] += 1
    return result


def partition_digest(connection: sqlite3.Connection) -> str:
    partitions: list[bytes] = []
    current_person: int | None = None
    current_members: list[int] = []
    rows = connection.execute(
        "SELECT person_id, id FROM face_prints "
        "WHERE person_id IS NOT NULL ORDER BY person_id, id"
    )
    for row in rows:
        person = int(row["person_id"])
        if current_person is not None and person != current_person:
            payload = ",".join(map(str, current_members)).encode("ascii")
            partitions.append(hashlib.sha256(payload).digest())
            current_members = []
        current_person = person
        current_members.append(int(row["id"]))
    if current_members:
        payload = ",".join(map(str, current_members)).encode("ascii")
        partitions.append(hashlib.sha256(payload).digest())
    digest = hashlib.sha256()
    for item in sorted(partitions):
        digest.update(item)
    return digest.hexdigest()


def centroid_metrics(rows: list[sqlite3.Row]) -> dict[str, Any]:
    vectors: list[tuple[int, tuple[float, ...]]] = []
    invalid_length = 0
    non_finite = 0
    non_unit = 0
    invalid_radius = 0
    for row in rows:
        blob = row["centroid"]
        if blob is None or len(blob) != 512:
            invalid_length += 1
            continue
        vector = struct.unpack("<128f", blob)
        if not all(math.isfinite(value) for value in vector):
            non_finite += 1
            continue
        norm = math.sqrt(sum(value * value for value in vector))
        if abs(norm - 1.0) > 1e-3:
            non_unit += 1
        radius = row["anchor_radius"]
        if radius is not None and not math.isfinite(float(radius)):
            invalid_radius += 1
        vectors.append((int(row["id"]), vector))

    pair_counts: dict[str, int] | None = None
    top_pairs: list[dict[str, Any]] | None = None
    numpy_error: str | None = None
    pair_backend: str | None = None
    if vectors:
        try:
            import numpy as np

            ids = np.asarray([item[0] for item in vectors], dtype=np.int64)
            matrix = np.asarray([item[1] for item in vectors], dtype=np.float32)
            thresholds = (0.75, 0.80, 0.85, 0.88)
            counts = {f"{threshold:.2f}": 0 for threshold in thresholds}
            best: list[tuple[float, int, int]] = []
            for start in range(0, len(matrix), 256):
                block = matrix[start : start + 256] @ matrix.T
                for local_index in range(block.shape[0]):
                    global_index = start + local_index
                    values = block[local_index, global_index + 1 :]
                    for threshold in thresholds:
                        counts[f"{threshold:.2f}"] += int(np.count_nonzero(values >= threshold))
                    if values.size:
                        take = min(20, int(values.size))
                        candidate_indexes = np.argpartition(values, -take)[-take:]
                        for candidate in candidate_indexes:
                            best.append(
                                (
                                    float(values[candidate]),
                                    int(ids[global_index]),
                                    int(ids[global_index + 1 + int(candidate)]),
                                )
                            )
            pair_counts = counts
            top_pairs = [
                {"similarity": score, "leftPersonID": left, "rightPersonID": right}
                for score, left, right in sorted(best, reverse=True)[:20]
            ]
            pair_backend = "numpy"
        except (ImportError, OSError, ValueError) as error:
            numpy_error = str(error)
            thresholds = (0.75, 0.80, 0.85, 0.88)
            counts = {f"{threshold:.2f}": 0 for threshold in thresholds}
            best: list[tuple[float, int, int]] = []
            for left_index, (left_id, left) in enumerate(vectors):
                for right_id, right in vectors[left_index + 1 :]:
                    score = sum(
                        left_value * right_value
                        for left_value, right_value in zip(left, right, strict=True)
                    )
                    for threshold in thresholds:
                        if score >= threshold:
                            counts[f"{threshold:.2f}"] += 1
                    item = (score, left_id, right_id)
                    if len(best) < 20:
                        heapq.heappush(best, item)
                    elif item > best[0]:
                        heapq.heapreplace(best, item)
            pair_counts = counts
            top_pairs = [
                {"similarity": score, "leftPersonID": left, "rightPersonID": right}
                for score, left, right in sorted(best, reverse=True)
            ]
            pair_backend = "stdlib"

    return {
        "rows": len(rows),
        "validVectors": len(vectors),
        "invalidLength": invalid_length,
        "nonFinite": non_finite,
        "nonUnit": non_unit,
        "invalidAnchorRadius": invalid_radius,
        "crossPersonPairCounts": pair_counts,
        "topCrossPersonPairs": top_pairs,
        "pairCalculationBackend": pair_backend,
        "numpyError": numpy_error,
    }


def centroid_fragment_risk(pair_counts: dict[str, int] | None) -> int | None:
    if pair_counts is None:
        return None
    return (
        pair_counts["0.80"]
        + pair_counts["0.85"] * 2
        + pair_counts["0.88"] * 4
    )


@dataclass(frozen=True)
class FaceCropInventory:
    directory: str
    ids: frozenset[int]
    file_count: int
    total_bytes: int
    invalid_names: int
    duplicate_ids: int
    membership_sha256: str
    extensions: tuple[tuple[str, int], ...]

    def summary(self) -> dict[str, Any]:
        return {
            "directory": self.directory,
            "fileCount": self.file_count,
            "uniqueFaceIDs": len(self.ids),
            "bytes": self.total_bytes,
            "invalidNames": self.invalid_names,
            "duplicateIDs": self.duplicate_ids,
            "membershipSHA256": self.membership_sha256,
            "extensionCounts": dict(self.extensions),
        }


def collect_face_crop_inventory(face_crops: Path) -> FaceCropInventory:
    digest = hashlib.sha256()
    crop_ids: set[int] = set()
    invalid_names = 0
    duplicate_ids = 0
    file_count = 0
    total_bytes = 0
    extensions: collections.Counter[str] = collections.Counter()
    with os.scandir(face_crops) as iterator:
        entries = sorted(iterator, key=lambda entry: os.fsencode(entry.name))
    for entry in entries:
        info = entry.stat(follow_symlinks=False)
        if is_reparse_point(info):
            raise ValueError(f"reparse point is not allowed in face crops: {entry.path}")
        if not stat.S_ISREG(info.st_mode):
            raise ValueError(f"non-regular face-crop entry is not allowed: {entry.path}")
        encoded = json.dumps(
            [entry.name, info.st_size, info.st_mtime_ns],
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8", "surrogatepass")
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
        file_count += 1
        total_bytes += info.st_size
        extensions[Path(entry.name).suffix.casefold()] += 1
        try:
            face_id = int(Path(entry.name).stem)
        except ValueError:
            invalid_names += 1
            continue
        if face_id in crop_ids:
            duplicate_ids += 1
        crop_ids.add(face_id)
    return FaceCropInventory(
        directory=str(face_crops),
        ids=frozenset(crop_ids),
        file_count=file_count,
        total_bytes=total_bytes,
        invalid_names=invalid_names,
        duplicate_ids=duplicate_ids,
        membership_sha256=digest.hexdigest(),
        extensions=tuple(sorted(extensions.items())),
    )


def crop_metrics(
    face_crops: FaceCropInventory | None, db_ids: set[int]
) -> dict[str, Any] | None:
    if face_crops is None:
        return None
    crop_ids = set(face_crops.ids)
    missing = sorted(db_ids - crop_ids)
    extra = sorted(crop_ids - db_ids)
    return {
        "inventory": face_crops.summary(),
        "cropCount": len(crop_ids),
        "invalidNames": face_crops.invalid_names,
        "duplicateIDs": face_crops.duplicate_ids,
        "missingCropCount": len(missing),
        "extraCropCount": len(extra),
        "missingCropIDsPreview": missing[:20],
        "extraCropIDsPreview": extra[:20],
        "exactIDSetMatch": (
            not missing
            and not extra
            and face_crops.invalid_names == 0
            and face_crops.duplicate_ids == 0
        ),
    }


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    position = fraction * (len(ordered) - 1)
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    weight = position - lower
    return ordered[lower] * (1 - weight) + ordered[upper] * weight


def cluster_verdict_counts(connection: sqlite3.Connection) -> dict[int, int]:
    rows = connection.execute(
        "WITH resolved AS ("
        "SELECT "
        "COALESCE(face_a, (SELECT id FROM face_prints "
        "WHERE file_id=file_a AND bbox=bbox_a ORDER BY id LIMIT 1)) AS a, "
        "COALESCE(face_b, (SELECT id FROM face_prints "
        "WHERE file_id=file_b AND bbox=bbox_b ORDER BY id LIMIT 1)) AS b "
        "FROM face_verifications WHERE same_person=0"
        ") "
        "SELECT fa.person_id,COUNT(*) "
        "FROM resolved r "
        "JOIN face_prints fa ON fa.id=r.a "
        "JOIN face_prints fb ON fb.id=r.b "
        "WHERE fa.person_id IS NOT NULL AND fa.person_id=fb.person_id "
        "GROUP BY fa.person_id"
    ).fetchall()
    return {int(row[0]): int(row[1]) for row in rows}


def named_membership_snapshot(db_path: Path) -> dict[str, Any]:
    with connect_readonly(db_path) as connection:
        people = {
            int(row["id"]): (
                row["name"],
                row["title"],
                row["first_name"],
                row["middle_name"],
                row["last_name"],
                row["suffix"],
            )
            for row in connection.execute(
                "SELECT id,name,title,first_name,middle_name,last_name,suffix "
                f"FROM persons p WHERE {PERSON_DISPLAY_NAME_SQL}<>''"
            )
        }
        centroids = {
            int(row["id"]): row["centroid"]
            for row in connection.execute(
                "SELECT id,centroid FROM persons WHERE id IN ("
                + ",".join("?" for _ in people)
                + ")",
                tuple(people),
            )
        } if people else {}
        owner_rows = connection.execute(
            "SELECT f.id AS faceID,f.person_id AS personID,f.arcface_embedding "
            "FROM face_prints f JOIN persons p ON p.id=f.person_id "
            f"WHERE {PERSON_DISPLAY_NAME_SQL}<>''"
        ).fetchall()
        owners = {
            int(row["faceID"]): int(row["personID"]) for row in owner_rows
        }
        embeddings = {
            int(row["faceID"]): row["arcface_embedding"] for row in owner_rows
        }
    digest = hashlib.sha256()
    for face_id, person_id in sorted(owners.items()):
        digest.update(f"{face_id}:{person_id}\n".encode("ascii"))
    return {
        "people": people,
        "owners": owners,
        "centroids": centroids,
        "embeddings": embeddings,
        "personCount": len(people),
        "faceCount": len(owners),
        "ownershipDigest": digest.hexdigest(),
    }


def named_membership_checks(
    baseline: dict[str, Any],
    current: dict[str, Any],
    admission_metrics: dict[str, Any] | None = None,
) -> dict[str, bool]:
    result = {
        "namedPeoplePreserved": all(
            current["people"].get(person_id) == identity
            for person_id, identity in baseline["people"].items()
        ),
        "namedFaceOwnersPreserved": all(
            current["owners"].get(face_id) == person_id
            for face_id, person_id in baseline["owners"].items()
        ),
        "noUnexpectedNamedPeople": set(current["people"])
        == set(baseline["people"]),
    }
    if admission_metrics is not None:
        result.update(admission_metrics["checks"])
    return result


def named_membership_admission_metrics(
    baseline: dict[str, Any], current: dict[str, Any]
) -> dict[str, Any]:
    baseline_people = set(baseline["people"])
    baseline_counts = collections.Counter(baseline["owners"].values())
    admitted = [
        (face_id, person_id)
        for face_id, person_id in current["owners"].items()
        if person_id in baseline_people
        and baseline["owners"].get(face_id) != person_id
    ]
    admissions_by_person = collections.Counter(
        person_id for _face_id, person_id in admitted
    )
    similarity_rows: list[dict[str, Any]] = []
    invalid_evidence: list[int] = []
    for face_id, person_id in admitted:
        centroid_blob = baseline["centroids"].get(person_id)
        embedding_blob = current["embeddings"].get(face_id)
        if (
            centroid_blob is None
            or embedding_blob is None
            or len(centroid_blob) != 512
            or len(embedding_blob) != 512
        ):
            invalid_evidence.append(face_id)
            continue
        centroid = struct.unpack("<128f", centroid_blob)
        embedding = struct.unpack("<128f", embedding_blob)
        similarity = sum(
            left * right
            for left, right in zip(centroid, embedding, strict=True)
        )
        if not math.isfinite(similarity):
            invalid_evidence.append(face_id)
            continue
        similarity_rows.append(
            {
                "faceID": face_id,
                "personID": person_id,
                "similarityToBaselineCentroid": similarity,
            }
        )
    similarities = sorted(
        float(row["similarityToBaselineCentroid"])
        for row in similarity_rows
    )
    total_limit = max(250, int(baseline["faceCount"]))
    per_person_limits = {
        person_id: max(100, int(baseline_counts[person_id]))
        for person_id in baseline_people
    }
    excessive_people = {
        person_id: {
            "admitted": count,
            "limit": per_person_limits[person_id],
        }
        for person_id, count in admissions_by_person.items()
        if count > per_person_limits[person_id]
    }
    return {
        "admittedFaceCount": len(admitted),
        "admissionLimit": total_limit,
        "admissionsByPerson": dict(admissions_by_person),
        "perPersonLimits": per_person_limits,
        "similarityMinimum": min(similarities) if similarities else None,
        "similarityP05": percentile(similarities, 0.05),
        "invalidEvidenceFaceIDs": invalid_evidence[:50],
        "excessivePeople": excessive_people,
        "checks": {
            "namedAdmissionsBounded": len(admitted) <= total_limit,
            "namedAdmissionsPerPersonBounded": not excessive_people,
            "namedAdmissionEvidenceComplete": not invalid_evidence,
            "namedAdmissionSimilarityMinimum": not admitted
            or (bool(similarities) and min(similarities) >= 0.35),
            "namedAdmissionSimilarityP05": not admitted
            or (
                bool(similarities)
                and percentile(similarities, 0.05) is not None
                and float(percentile(similarities, 0.05)) >= 0.55
            ),
        },
    }


def named_membership_summary(snapshot: dict[str, Any]) -> dict[str, Any]:
    return {
        "personCount": snapshot["personCount"],
        "faceCount": snapshot["faceCount"],
        "ownershipDigest": snapshot["ownershipDigest"],
    }


def top_cluster_embedding_integrity(
    connection: sqlite3.Connection,
    capture_profiles: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    verdict_counts = cluster_verdict_counts(connection)
    result: list[dict[str, Any]] = []
    for profile in capture_profiles[:10]:
        person_id = int(profile["personID"])
        centroid_blob = connection.execute(
            "SELECT centroid FROM persons WHERE id=?", (person_id,)
        ).fetchone()[0]
        if centroid_blob is None or len(centroid_blob) != 512:
            result.append(
                {
                    **profile,
                    "error": "centroid missing or not 512 bytes",
                    "manualDifferentPersonVerdictsInsideCluster": verdict_counts.get(
                        person_id, 0
                    ),
                }
            )
            continue
        centroid = struct.unpack("<128f", centroid_blob)
        candidates: list[dict[str, Any]] = []
        for row in connection.execute(
            "SELECT fp.id AS faceID,fp.file_id AS fileID,fp.bbox,"
            "fp.face_quality AS faceQuality,fp.arcface_embedding,"
            "fi.path_text AS path,fi.content_hash,fi.phash "
            "FROM face_prints fp JOIN files fi ON fi.id=fp.file_id "
            "WHERE fp.person_id=? AND fp.excluded=0 AND fi.failed=0 "
            "ORDER BY fp.id",
            (person_id,),
        ):
            embedding = row["arcface_embedding"]
            if embedding is None or len(embedding) != 512:
                continue
            vector = struct.unpack("<128f", embedding)
            similarity = sum(
                left * right for left, right in zip(centroid, vector, strict=True)
            )
            content_hash = row["content_hash"]
            capture_key = (
                f"H{bytes(content_hash).hex().upper()}"
                if content_hash is not None and len(content_hash) > 0
                else f"F{int(row['fileID'])}"
            )
            candidates.append(
                {
                    "faceID": int(row["faceID"]),
                    "fileID": int(row["fileID"]),
                    "path": row["path"],
                    "bbox": row["bbox"],
                    "faceQuality": float(row["faceQuality"]),
                    "similarityToCentroid": similarity,
                    "captureKey": capture_key,
                    "pHash": row["phash"],
                }
            )
        candidates.sort(key=lambda item: (item["similarityToCentroid"], item["faceID"]))
        similarities = [float(item["similarityToCentroid"]) for item in candidates]
        p05_index = round(0.05 * (len(candidates) - 1)) if candidates else 0
        p05_start = max(0, p05_index - 1)
        p05_end = min(len(candidates), p05_index + 2)
        representatives = {
            "farthest": candidates[:1],
            "p05Outliers": candidates[p05_start:p05_end],
            "closest": candidates[-1:] if candidates else [],
        }
        raw_rows = int(profile["rawRows"])
        result.append(
            {
                **profile,
                "centroidCosineP01": percentile(similarities, 0.01),
                "centroidCosineP05": percentile(similarities, 0.05),
                "centroidCosineMedian": percentile(similarities, 0.50),
                "cohesionMeanCosine": (
                    sum(similarities) / len(similarities) if similarities else None
                ),
                "distinctCaptureRatio": (
                    int(profile["distinctExactCaptures"]) / raw_rows
                    if raw_rows
                    else None
                ),
                "exactPHashRatio": (
                    int(profile["distinctExactPHash"]) / raw_rows
                    if raw_rows
                    else None
                ),
                "manualDifferentPersonVerdictsInsideCluster": verdict_counts.get(
                    person_id, 0
                ),
                "representatives": representatives,
            }
        )
    return result


def collect_face_metrics(
    db_path: Path, face_crops: FaceCropInventory | None = None
) -> dict[str, Any]:
    with connect_readonly(db_path) as connection:
        total = int(scalar(connection, "SELECT COUNT(*) FROM face_prints"))
        excluded = int(
            scalar(connection, "SELECT COUNT(*) FROM face_prints WHERE excluded=1")
        )
        low_quality = int(
            scalar(
                connection,
                "SELECT COUNT(*) FROM face_prints "
                "WHERE excluded=0 AND face_quality < ?",
                (QUALITY_FLOOR,),
            )
        )
        eligible = int(
            scalar(
                connection,
                "SELECT COUNT(*) FROM face_prints "
                "WHERE excluded=0 AND face_quality >= ?",
                (QUALITY_FLOOR,),
            )
        )
        assigned = int(
            scalar(
                connection,
                "SELECT COUNT(*) FROM face_prints "
                "WHERE excluded=0 AND face_quality >= ? AND person_id IS NOT NULL",
                (QUALITY_FLOOR,),
            )
        )
        cluster_input_faces = int(
            scalar(
                connection,
                "SELECT COUNT(*) FROM face_prints "
                "WHERE excluded=0 AND LENGTH(arcface_embedding)=512",
            )
        )
        unmatched_cluster_input = int(
            scalar(
                connection,
                "SELECT COUNT(*) FROM face_prints "
                "WHERE excluded=0 AND LENGTH(arcface_embedding)=512 "
                "AND person_id IS NULL",
            )
        )
        persons = int(scalar(connection, "SELECT COUNT(*) FROM persons"))
        named_person_ids = {
            int(row[0])
            for row in connection.execute(
                f"SELECT p.id FROM persons p WHERE {PERSON_DISPLAY_NAME_SQL}<>''"
            )
        }
        named_persons = len(named_person_ids)
        unknown_persons = int(
            scalar(connection, "SELECT COUNT(*) FROM persons WHERE is_unknown=1")
        )
        size_rows = connection.execute(
            "SELECT p.id, COUNT(f.id) AS faces "
            "FROM persons p LEFT JOIN face_prints f ON f.person_id=p.id "
            "GROUP BY p.id ORDER BY faces, p.id"
        ).fetchall()
        sizes = [int(row["faces"]) for row in size_rows]
        displayable_persons = sum(
            1
            for row in size_rows
            if int(row["faces"]) >= PEOPLE_MIN_FACES_PER_CLUSTER
            or int(row["id"]) in named_person_ids
        )
        capture_profiles = [
            dict(row)
            for row in connection.execute(
                "SELECT p.id AS personID, p.name, p.is_unknown AS isUnknown, "
                "p.representative_face_id AS representativeFaceID, "
                "COUNT(fp.id) AS rawRows, "
                "COUNT(DISTINCT fp.file_id) AS distinctFiles, "
                "COUNT(DISTINCT CASE "
                "WHEN fi.content_hash IS NOT NULL AND LENGTH(fi.content_hash)>0 "
                "THEN 'H' || HEX(fi.content_hash) "
                "ELSE 'F' || fp.file_id END) AS distinctExactCaptures, "
                "COUNT(DISTINCT fi.phash) AS distinctExactPHash "
                "FROM persons p "
                "JOIN face_prints fp ON fp.person_id=p.id AND fp.excluded=0 "
                "JOIN files fi ON fi.id=fp.file_id AND fi.failed=0 "
                "GROUP BY p.id ORDER BY rawRows DESC, p.id"
            )
        ]
        largest_cluster_sample: list[dict[str, Any]] = []
        if capture_profiles:
            largest_person_id = int(capture_profiles[0]["personID"])
            largest_cluster_sample = [
                dict(row)
                for row in connection.execute(
                    "SELECT fp.id AS faceID, fp.file_id AS fileID, "
                    "fi.path_text AS path, fp.face_quality AS faceQuality, "
                    "HEX(fi.content_hash) AS contentHash, fi.phash "
                    "FROM face_prints fp JOIN files fi ON fi.id=fp.file_id "
                    "WHERE fp.person_id=? "
                    "AND fp.excluded=0 AND fi.failed=0 "
                    "ORDER BY fp.face_quality DESC, fp.id LIMIT 40",
                    (largest_person_id,),
                )
            ]
        embedding_integrity = top_cluster_embedding_integrity(
            connection, capture_profiles
        )
        cohesion_profiles = [
            profile
            for profile in embedding_integrity
            if "error" not in profile
            and profile.get("centroidCosineP01") is not None
            and profile.get("centroidCosineP05") is not None
            and profile.get("centroidCosineMedian") is not None
        ]
        cohesion_p01 = sorted(
            float(profile["centroidCosineP01"])
            for profile in cohesion_profiles
        )
        cohesion_p05 = sorted(
            float(profile["centroidCosineP05"])
            for profile in cohesion_profiles
        )
        cohesion_medians = sorted(
            float(profile["centroidCosineMedian"])
            for profile in cohesion_profiles
        )
        manual_different_person_violations = sum(
            cluster_verdict_counts(connection).values()
        )
        embedding_lengths = {
            str(row["bytes"]): int(row["count"])
            for row in connection.execute(
                "SELECT LENGTH(print_data) AS bytes, COUNT(*) AS count "
                "FROM face_prints GROUP BY LENGTH(print_data)"
            )
        }
        mirror_lengths = {
            str(row["bytes"]): int(row["count"])
            for row in connection.execute(
                "SELECT LENGTH(arcface_embedding) AS bytes, COUNT(*) AS count "
                "FROM face_prints GROUP BY LENGTH(arcface_embedding)"
            )
        }
        mismatched_embeddings = int(
            scalar(
                connection,
                "SELECT COUNT(*) FROM face_prints "
                "WHERE arcface_embedding IS NULL OR print_data <> arcface_embedding",
            )
        )
        zero_face_persons = sum(1 for size in sizes if size == 0)
        assigned_person_faces = sum(sizes)
        orphan_person_refs = int(
            scalar(
                connection,
                "SELECT COUNT(*) FROM face_prints f "
                "WHERE f.person_id IS NOT NULL "
                "AND NOT EXISTS (SELECT 1 FROM persons p WHERE p.id=f.person_id)",
            )
        )
        file_count_mismatches = int(
            scalar(
                connection,
                "SELECT COUNT(*) FROM ("
                "SELECT p.id FROM persons p "
                "LEFT JOIN face_prints f ON f.person_id=p.id "
                "WHERE COALESCE(p.is_unknown,0)=0 "
                "GROUP BY p.id HAVING p.file_count <> COUNT(DISTINCT f.file_id))",
            )
        )
        bad_representatives = int(
            scalar(
                connection,
                "SELECT COUNT(*) FROM persons p "
                "LEFT JOIN face_prints f ON f.id=p.representative_face_id "
                "WHERE p.representative_face_id IS NULL OR f.id IS NULL OR f.person_id<>p.id",
            )
        )
        same_file_collision_rows = [
            dict(row)
            for row in connection.execute(
                "SELECT fp.file_id AS fileID, fp.person_id AS personID, "
                "COUNT(*) AS faceCount "
                "FROM face_prints fp JOIN persons p ON p.id=fp.person_id "
                "WHERE fp.excluded=0 AND COALESCE(p.is_unknown,0)=0 AND "
                f"{PERSON_DISPLAY_NAME_SQL}='' AND NOT EXISTS ("
                "SELECT 1 FROM face_verifications v "
                "WHERE v.same_person=1 AND v.vlm_model='user-merged' "
                "AND (v.person_a=p.id OR v.person_b=p.id)) "
                "GROUP BY fp.file_id, fp.person_id HAVING COUNT(*)>1 "
                "ORDER BY faceCount DESC, fp.file_id, fp.person_id"
            )
        ]
        same_file_collision_extra_faces = sum(
            int(row["faceCount"]) - 1 for row in same_file_collision_rows
        )
        unknown_bucket_collision_rows = [
            dict(row)
            for row in connection.execute(
                "SELECT fp.file_id AS fileID, fp.person_id AS personID, "
                "COUNT(*) AS faceCount "
                "FROM face_prints fp JOIN persons p ON p.id=fp.person_id "
                "WHERE fp.excluded=0 AND COALESCE(p.is_unknown,0)=1 "
                "GROUP BY fp.file_id, fp.person_id HAVING COUNT(*)>1 "
                "ORDER BY faceCount DESC, fp.file_id, fp.person_id"
            )
        ]
        unknown_bucket_collision_extra_faces = sum(
            int(row["faceCount"]) - 1 for row in unknown_bucket_collision_rows
        )
        protected_collision_rows = [
            dict(row)
            for row in connection.execute(
                "SELECT fp.file_id AS fileID, fp.person_id AS personID, "
                "COUNT(*) AS faceCount "
                "FROM face_prints fp JOIN persons p ON p.id=fp.person_id "
                "WHERE fp.excluded=0 AND COALESCE(p.is_unknown,0)=0 AND ("
                f"{PERSON_DISPLAY_NAME_SQL}<>'' OR EXISTS ("
                "SELECT 1 FROM face_verifications v "
                "WHERE v.same_person=1 AND v.vlm_model='user-merged' "
                "AND (v.person_a=p.id OR v.person_b=p.id))) "
                "GROUP BY fp.file_id, fp.person_id HAVING COUNT(*)>1 "
                "ORDER BY faceCount DESC, fp.file_id, fp.person_id"
            )
        ]
        protected_collision_extra_faces = sum(
            int(row["faceCount"]) - 1 for row in protected_collision_rows
        )
        centroids = centroid_metrics(
            connection.execute(
                "SELECT id, centroid, anchor_radius FROM persons "
                "WHERE COALESCE(is_unknown,0)=0 ORDER BY id"
            ).fetchall()
        )
        db_ids = {
            int(row[0]) for row in connection.execute("SELECT id FROM face_prints")
        }
        digest = partition_digest(connection)

    sorted_sizes = sorted(sizes)
    median = (
        sorted_sizes[len(sorted_sizes) // 2]
        if sorted_sizes
        else 0
    )
    tiny = sum(1 for size in sizes if size <= 12)
    return {
        "totalDetected": total,
        "excluded": excluded,
        "lowQuality": low_quality,
        "qualityEligible": eligible,
        "assignedEligible": assigned,
        "unmatchedEligible": eligible - assigned,
        "clusterInputFaces": cluster_input_faces,
        "unmatchedClusterInput": unmatched_cluster_input,
        "persons": persons,
        "displayablePersons": displayable_persons,
        "namedPersons": named_persons,
        "unknownPersons": unknown_persons,
        "personsAtMost12": tiny,
            "personsAtMost12Fraction": tiny / persons if persons else 0,
            "medianClusterSize": median,
            "maximumClusterSize": max(sizes, default=0),
            "assignedPersonFaces": assigned_person_faces,
            "largestClusterShare": (
                max(sizes, default=0) / assigned_person_faces
                if assigned_person_faces
                else 0
            ),
            "topClusterCohesion": {
                "profileCount": len(cohesion_profiles),
                "p01Minimum": min(cohesion_p01) if cohesion_p01 else None,
                "p05Minimum": min(cohesion_p05) if cohesion_p05 else None,
                "p05Median": percentile(cohesion_p05, 0.50),
                "clusterMedianMinimum": (
                    min(cohesion_medians) if cohesion_medians else None
                ),
            },
        "clusterBins": cluster_bins(sizes),
        "distinctExactContentBins": cluster_bins(
            [int(row["distinctExactCaptures"]) for row in capture_profiles]
        ),
        "distinctExactPHashBins": cluster_bins(
            [int(row["distinctExactPHash"]) for row in capture_profiles]
        ),
        "largestCaptureProfiles": capture_profiles[:20],
        "largestClusterPuritySample": largest_cluster_sample,
        "topClusterEmbeddingIntegrityProxy": embedding_integrity,
        "manualDifferentPersonViolations": manual_different_person_violations,
        "partitionDigest": digest,
        "partitionDigestAlgorithm": (
            "sha256(concat(sort(sha256(csv(sorted(face_ids_per_person))))))"
        ),
        "embeddingLengths": embedding_lengths,
        "mirrorEmbeddingLengths": mirror_lengths,
        "mismatchedEmbeddingColumns": mismatched_embeddings,
        "zeroFacePersons": zero_face_persons,
        "orphanPersonReferences": orphan_person_refs,
        "fileCountMismatches": file_count_mismatches,
        "badRepresentativeFaces": bad_representatives,
        "sameFileIdentityCollisionGroups": len(same_file_collision_rows),
        "sameFileIdentityCollisionExtraFaces": same_file_collision_extra_faces,
        "sameFileIdentityCollisionSample": same_file_collision_rows[:20],
        "sameFileProtectedIdentityCollisionGroups": len(protected_collision_rows),
        "sameFileProtectedIdentityCollisionExtraFaces": (
            protected_collision_extra_faces
        ),
        "sameFileProtectedIdentityCollisionSample": protected_collision_rows[:20],
        "sameFileUnknownBucketCollisionGroups": len(unknown_bucket_collision_rows),
        "sameFileUnknownBucketCollisionExtraFaces": (
            unknown_bucket_collision_extra_faces
        ),
        "centroids": centroids,
        "crops": crop_metrics(face_crops, db_ids),
    }


def strict_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def strict_json_loads(raw: str) -> Any:
    def reject_constant(value: str) -> Any:
        raise ValueError(f"non-finite JSON number: {value}")

    return json.loads(
        raw,
        object_pairs_hook=strict_json_object,
        parse_constant=reject_constant,
    )


def validate_event_envelope(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("event is not a JSON object")
    if set(value) != {"t", "payload"}:
        raise ValueError(f"event envelope keys are invalid: {sorted(value)}")
    timestamp = value["t"]
    if not isinstance(timestamp, str):
        raise ValueError("event timestamp is not a string")
    parsed_timestamp = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
    if parsed_timestamp.tzinfo is None:
        raise ValueError("event timestamp has no timezone")
    payload = value["payload"]
    if not isinstance(payload, dict) or len(payload) != 1:
        raise ValueError("event payload must contain exactly one variant")
    kind = next(iter(payload))
    if kind not in KNOWN_EVENT_KINDS:
        raise ValueError(f"unknown event kind: {kind}")
    return value


def inner_payload(event: dict[str, Any], kind: str) -> Any:
    value = event.get("payload", {}).get(kind)
    if isinstance(value, dict) and set(value) == {"_0"}:
        return value["_0"]
    return value


def event_kind(event: dict[str, Any]) -> str | None:
    payload = event.get("payload")
    if not isinstance(payload, dict) or len(payload) != 1:
        return None
    return next(iter(payload))


class JobBasicLimitInformation(ctypes.Structure):
    _fields_ = [
        ("PerProcessUserTimeLimit", ctypes.c_longlong),
        ("PerJobUserTimeLimit", ctypes.c_longlong),
        ("LimitFlags", ctypes.c_ulong),
        ("MinimumWorkingSetSize", ctypes.c_size_t),
        ("MaximumWorkingSetSize", ctypes.c_size_t),
        ("ActiveProcessLimit", ctypes.c_ulong),
        ("Affinity", ctypes.c_size_t),
        ("PriorityClass", ctypes.c_ulong),
        ("SchedulingClass", ctypes.c_ulong),
    ]


class JobIoCounters(ctypes.Structure):
    _fields_ = [
        ("ReadOperationCount", ctypes.c_ulonglong),
        ("WriteOperationCount", ctypes.c_ulonglong),
        ("OtherOperationCount", ctypes.c_ulonglong),
        ("ReadTransferCount", ctypes.c_ulonglong),
        ("WriteTransferCount", ctypes.c_ulonglong),
        ("OtherTransferCount", ctypes.c_ulonglong),
    ]


class JobExtendedLimitInformation(ctypes.Structure):
    _fields_ = [
        ("BasicLimitInformation", JobBasicLimitInformation),
        ("IoInfo", JobIoCounters),
        ("ProcessMemoryLimit", ctypes.c_size_t),
        ("JobMemoryLimit", ctypes.c_size_t),
        ("PeakProcessMemoryUsed", ctypes.c_size_t),
        ("PeakJobMemoryUsed", ctypes.c_size_t),
    ]


class JobBasicAccountingInformation(ctypes.Structure):
    _fields_ = [
        ("TotalUserTime", ctypes.c_longlong),
        ("TotalKernelTime", ctypes.c_longlong),
        ("ThisPeriodTotalUserTime", ctypes.c_longlong),
        ("ThisPeriodTotalKernelTime", ctypes.c_longlong),
        ("TotalPageFaultCount", ctypes.c_ulong),
        ("TotalProcesses", ctypes.c_ulong),
        ("ActiveProcesses", ctypes.c_ulong),
        ("TotalTerminatedProcesses", ctypes.c_ulong),
    ]


class ThreadEntry32(ctypes.Structure):
    _fields_ = [
        ("dwSize", ctypes.c_ulong),
        ("cntUsage", ctypes.c_ulong),
        ("th32ThreadID", ctypes.c_ulong),
        ("th32OwnerProcessID", ctypes.c_ulong),
        ("tpBasePri", ctypes.c_long),
        ("tpDeltaPri", ctypes.c_long),
        ("dwFlags", ctypes.c_ulong),
    ]


def resume_suspended_process(pid: int) -> None:
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateToolhelp32Snapshot.argtypes = [ctypes.c_ulong, ctypes.c_ulong]
    kernel32.CreateToolhelp32Snapshot.restype = ctypes.c_void_p
    kernel32.Thread32First.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ThreadEntry32),
    ]
    kernel32.Thread32First.restype = ctypes.c_int
    kernel32.Thread32Next.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ThreadEntry32),
    ]
    kernel32.Thread32Next.restype = ctypes.c_int
    kernel32.OpenThread.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    kernel32.OpenThread.restype = ctypes.c_void_p
    kernel32.ResumeThread.argtypes = [ctypes.c_void_p]
    kernel32.ResumeThread.restype = ctypes.c_ulong
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    ctypes.set_last_error(0)
    snapshot = kernel32.CreateToolhelp32Snapshot(0x00000004, 0)
    if not snapshot or snapshot == ctypes.c_void_p(-1).value:
        raise OSError(ctypes.get_last_error(), "thread snapshot failed")
    resumed = 0
    try:
        entry = ThreadEntry32()
        entry.dwSize = ctypes.sizeof(entry)
        ctypes.set_last_error(0)
        if not kernel32.Thread32First(snapshot, ctypes.byref(entry)):
            raise OSError(ctypes.get_last_error(), "Thread32First failed")
        while True:
            if int(entry.th32OwnerProcessID) == pid:
                ctypes.set_last_error(0)
                thread_handle = kernel32.OpenThread(
                    0x0002, False, int(entry.th32ThreadID)
                )
                if not thread_handle:
                    raise OSError(
                        ctypes.get_last_error(),
                        f"OpenThread failed for TID {entry.th32ThreadID}",
                    )
                try:
                    ctypes.set_last_error(0)
                    previous_count = kernel32.ResumeThread(thread_handle)
                    if previous_count == 0xFFFFFFFF:
                        raise OSError(
                            ctypes.get_last_error(),
                            f"ResumeThread failed for TID {entry.th32ThreadID}",
                        )
                    if previous_count != 1:
                        raise RuntimeError(
                            "process thread suspension count was not exactly one: "
                            f"TID {entry.th32ThreadID}, count {previous_count}"
                        )
                    resumed += 1
                finally:
                    ctypes.set_last_error(0)
                    if not kernel32.CloseHandle(thread_handle):
                        raise OSError(
                            ctypes.get_last_error(),
                            f"CloseHandle failed for TID {entry.th32ThreadID}",
                        )
            ctypes.set_last_error(0)
            if kernel32.Thread32Next(snapshot, ctypes.byref(entry)):
                continue
            error = ctypes.get_last_error()
            if error == 18:
                break
            raise OSError(error, "Thread32Next failed before enumeration completed")
    finally:
        ctypes.set_last_error(0)
        if not kernel32.CloseHandle(snapshot):
            raise OSError(
                ctypes.get_last_error(),
                "CloseHandle(thread snapshot) failed",
            )
    if resumed != 1:
        raise RuntimeError(
            f"expected one suspended primary thread for PID {pid}, resumed {resumed}"
        )


class WindowsJobObject:
    def __init__(self) -> None:
        if os.name != "nt":
            raise RuntimeError("the real-data harness requires Windows Job Objects")
        self.kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel32.CreateJobObjectW.argtypes = [ctypes.c_void_p, ctypes.c_wchar_p]
        self.kernel32.CreateJobObjectW.restype = ctypes.c_void_p
        self.kernel32.SetInformationJobObject.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_void_p,
            ctypes.c_ulong,
        ]
        self.kernel32.SetInformationJobObject.restype = ctypes.c_int
        self.kernel32.AssignProcessToJobObject.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        self.kernel32.AssignProcessToJobObject.restype = ctypes.c_int
        self.kernel32.QueryInformationJobObject.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_void_p,
            ctypes.c_ulong,
            ctypes.c_void_p,
        ]
        self.kernel32.QueryInformationJobObject.restype = ctypes.c_int
        self.kernel32.TerminateJobObject.argtypes = [ctypes.c_void_p, ctypes.c_uint]
        self.kernel32.TerminateJobObject.restype = ctypes.c_int
        self.kernel32.OpenProcess.argtypes = [
            ctypes.c_ulong,
            ctypes.c_int,
            ctypes.c_ulong,
        ]
        self.kernel32.OpenProcess.restype = ctypes.c_void_p
        self.kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
        self.kernel32.CloseHandle.restype = ctypes.c_int
        self.handle = self.kernel32.CreateJobObjectW(None, None)
        if not self.handle:
            raise OSError(ctypes.get_last_error(), "CreateJobObjectW failed")
        limits = JobExtendedLimitInformation()
        limits.BasicLimitInformation.LimitFlags = 0x00002000
        if not self.kernel32.SetInformationJobObject(
            self.handle,
            9,
            ctypes.byref(limits),
            ctypes.sizeof(limits),
        ):
            error = ctypes.get_last_error()
            self.close()
            raise OSError(error, "SetInformationJobObject failed")

    def assign(self, pid: int) -> None:
        process_handle = self.kernel32.OpenProcess(0x0001 | 0x0100, False, pid)
        if not process_handle:
            raise OSError(ctypes.get_last_error(), f"OpenProcess failed for PID {pid}")
        try:
            if not self.kernel32.AssignProcessToJobObject(
                self.handle, process_handle
            ):
                raise OSError(
                    ctypes.get_last_error(),
                    f"AssignProcessToJobObject failed for PID {pid}",
                )
        finally:
            self.kernel32.CloseHandle(process_handle)

    def active_process_count(self) -> int:
        accounting = JobBasicAccountingInformation()
        if not self.kernel32.QueryInformationJobObject(
            self.handle,
            1,
            ctypes.byref(accounting),
            ctypes.sizeof(accounting),
            None,
        ):
            raise OSError(
                ctypes.get_last_error(), "QueryInformationJobObject failed"
            )
        return int(accounting.ActiveProcesses)

    def terminate(self, exit_code: int) -> None:
        if self.handle and not self.kernel32.TerminateJobObject(
            self.handle, exit_code
        ):
            raise OSError(ctypes.get_last_error(), "TerminateJobObject failed")

    def close(self) -> None:
        if self.handle:
            if not self.kernel32.CloseHandle(self.handle):
                raise OSError(ctypes.get_last_error(), "CloseHandle(job) failed")
            self.handle = None


@dataclass
class RecordedEvent:
    sequence: int
    elapsed: float
    raw: str
    value: dict[str, Any]


class EngineDriver:
    def __init__(
        self,
        engine: Path,
        environment: dict[str, str],
        artifacts: Path,
        working_directory: Path,
    ) -> None:
        self.engine = engine
        self.environment = environment
        self.artifacts = artifacts
        self.working_directory = working_directory
        self.process: subprocess.Popen[str] | None = None
        self.events: list[RecordedEvent] = []
        self.invalid_stdout: list[str] = []
        self.protocol_errors: list[str] = []
        self.stderr_lines: list[str] = []
        self.stdout_eof = False
        self.stderr_eof = False
        self.stdout_reader_error: str | None = None
        self.stderr_reader_error: str | None = None
        self.command_marks: dict[str, int] = {}
        self.commands: list[dict[str, Any]] = []
        self._condition = threading.Condition()
        self._started = 0.0
        self._stdout_handle: Any = None
        self._stderr_handle: Any = None
        self._threads: list[threading.Thread] = []
        self._job: WindowsJobObject | None = None
        self.job_assigned = False
        self.process_started_suspended = False
        self.process_resumed_after_assignment = False
        self.job_active_after_root_exit: int | None = None
        self.job_errors: list[str] = []

    def start(self) -> None:
        try:
            self._stdout_handle = (self.artifacts / "engine.stdout.jsonl").open(
                "w", encoding="utf-8", buffering=1
            )
            self._stderr_handle = (self.artifacts / "engine.stderr.log").open(
                "w", encoding="utf-8", buffering=1
            )
            self._started = time.monotonic()
            self._job = WindowsJobObject()
            creation_flags = (
                getattr(subprocess, "CREATE_NO_WINDOW", 0) | 0x00000004
            )
            self.process = subprocess.Popen(
                [str(self.engine)],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                errors="strict",
                bufsize=1,
                env=self.environment,
                cwd=self.working_directory,
                creationflags=creation_flags,
            )
            self.process_started_suspended = True
            self._job.assign(self.process.pid)
            self.job_assigned = True
            resume_suspended_process(self.process.pid)
            self.process_resumed_after_assignment = True
            for target, name in (
                (self._read_stdout, "fileid-engine-stdout"),
                (self._read_stderr, "fileid-engine-stderr"),
            ):
                thread = threading.Thread(target=target, name=name, daemon=True)
                thread.start()
                self._threads.append(thread)
        except BaseException:
            self.force_stop()
            raise

    def _read_stdout(self) -> None:
        assert self.process is not None and self.process.stdout is not None
        try:
            for line in self.process.stdout:
                self._stdout_handle.write(line)
                raw = line.rstrip("\r\n")
                try:
                    value = validate_event_envelope(strict_json_loads(raw))
                except (json.JSONDecodeError, TypeError, ValueError) as error:
                    with self._condition:
                        self.invalid_stdout.append(raw)
                        self.protocol_errors.append(str(error))
                        self._condition.notify_all()
                    continue
                with self._condition:
                    item = RecordedEvent(
                        sequence=len(self.events),
                        elapsed=time.monotonic() - self._started,
                        raw=raw,
                        value=value,
                    )
                    self.events.append(item)
                    self._condition.notify_all()
        except BaseException as error:
            with self._condition:
                self.stdout_reader_error = f"{type(error).__name__}: {error}"
        finally:
            with self._condition:
                self.stdout_eof = True
                self._condition.notify_all()

    def _read_stderr(self) -> None:
        assert self.process is not None and self.process.stderr is not None
        try:
            for line in self.process.stderr:
                self._stderr_handle.write(line)
                with self._condition:
                    self.stderr_lines.append(line.rstrip("\r\n"))
                    self._condition.notify_all()
        except BaseException as error:
            with self._condition:
                self.stderr_reader_error = f"{type(error).__name__}: {error}"
        finally:
            with self._condition:
                self.stderr_eof = True
                self._condition.notify_all()

    def mark(self) -> int:
        with self._condition:
            return len(self.events)

    def elapsed(self) -> float:
        return time.monotonic() - self._started

    def send(self, command_id: str, payload: dict[str, Any]) -> int:
        assert self.process is not None and self.process.stdin is not None
        if self.process.poll() is not None:
            raise RuntimeError(
                f"engine exited before {command_id}, code={self.process.returncode}"
            )
        if len(payload) != 1:
            raise ValueError("harness commands must contain exactly one payload variant")
        command_kind = next(iter(payload))
        if command_kind not in HARNESS_COMMAND_KINDS:
            raise ValueError(f"command is not allowed by the read-only harness: {command_kind}")
        with self._condition:
            if command_id in self.command_marks:
                raise ValueError(f"duplicate command id: {command_id}")
            if self.stdout_reader_error is not None:
                raise RuntimeError(f"stdout reader failed: {self.stdout_reader_error}")
            if self.stdout_eof:
                raise RuntimeError("engine stdout reached EOF before command send")
            command_mark = len(self.events)
            self.command_marks[command_id] = command_mark
        encoded = json.dumps(
            {"id": command_id, "payload": payload},
            separators=(",", ":"),
            ensure_ascii=False,
        )
        with (self.artifacts / "commands.jsonl").open(
            "a", encoding="utf-8"
        ) as handle:
            handle.write(encoded + "\n")
        try:
            self.process.stdin.write(encoded + "\n")
            self.process.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise RuntimeError(f"engine stdin failed for {command_id}: {error}") from error
        self.commands.append(
            {
                "id": command_id,
                "kind": command_kind,
                "eventOffset": command_mark,
            }
        )
        return command_mark

    def wait_for(
        self,
        kind: str,
        *,
        after: int | None = None,
        command_id: str | None = None,
        timeout_seconds: float,
        predicate: Any = None,
    ) -> RecordedEvent:
        if command_id is not None:
            if command_id not in self.command_marks:
                raise ValueError(f"unknown command id: {command_id}")
            command_mark = self.command_marks[command_id]
            if after is not None and after != command_mark:
                raise ValueError(
                    f"event offset does not match command {command_id}: "
                    f"{after} != {command_mark}"
                )
            after = command_mark
        if after is None:
            raise ValueError("wait_for requires an event offset or command id")
        deadline = time.monotonic() + timeout_seconds
        scan_index = after
        with self._condition:
            while True:
                while scan_index < len(self.events):
                    item = self.events[scan_index]
                    scan_index += 1
                    observed_kind = event_kind(item.value)
                    if observed_kind == "error" and kind != "error":
                        raise RuntimeError(
                            f"engine emitted an error while waiting for {kind}: "
                            f"{inner_payload(item.value, 'error')!r}"
                        )
                    if observed_kind != kind:
                        continue
                    if predicate is None or predicate(inner_payload(item.value, kind)):
                        return item
                if self.process is not None and self.process.poll() is not None:
                    raise RuntimeError(
                        f"engine exited waiting for {kind}, code={self.process.returncode}"
                    )
                if self.protocol_errors:
                    raise RuntimeError(
                        "engine emitted malformed protocol data: "
                        + "; ".join(self.protocol_errors[:5])
                    )
                if self.stdout_reader_error is not None:
                    raise RuntimeError(f"stdout reader failed: {self.stdout_reader_error}")
                if self.stdout_eof:
                    raise RuntimeError(f"engine stdout reached EOF waiting for {kind}")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"timed out waiting for {kind}")
                self._condition.wait(min(remaining, 1.0))

    def wait_for_quiet(
        self,
        *,
        quiet_seconds: float = 0.5,
        timeout_seconds: float = 10,
    ) -> int:
        deadline = time.monotonic() + timeout_seconds
        with self._condition:
            observed = len(self.events)
            quiet_deadline = time.monotonic() + quiet_seconds
            while True:
                if self.protocol_errors:
                    raise RuntimeError(
                        "engine emitted malformed protocol data: "
                        + "; ".join(self.protocol_errors[:5])
                    )
                if self.stdout_reader_error is not None:
                    raise RuntimeError(f"stdout reader failed: {self.stdout_reader_error}")
                if self.process is not None and self.process.poll() is not None:
                    raise RuntimeError(
                        "engine exited while waiting for a quiet event boundary, "
                        f"code={self.process.returncode}"
                    )
                now = time.monotonic()
                if len(self.events) != observed:
                    observed = len(self.events)
                    quiet_deadline = now + quiet_seconds
                if now >= quiet_deadline:
                    return observed
                remaining = min(quiet_deadline - now, deadline - now)
                if remaining <= 0:
                    raise TimeoutError("timed out waiting for a quiet event boundary")
                self._condition.wait(min(remaining, 0.25))

    def command_mark(self, command_id: str) -> int:
        return self.command_marks[command_id]

    def events_between(self, start: int, end: int | None = None) -> list[RecordedEvent]:
        with self._condition:
            return list(self.events[start:end])

    def stop(self, timeout_seconds: float = 30) -> int:
        assert self.process is not None
        try:
            return_code = self.process.wait(timeout=timeout_seconds)
            self._finalize_job(require_empty=True)
            return return_code
        finally:
            self._close_handles()

    def _finalize_job(self, *, require_empty: bool) -> None:
        if self._job is None:
            if require_empty:
                raise RuntimeError("engine process was not contained in a Job Object")
            return
        job = self._job
        try:
            deadline = time.monotonic() + 5
            active = job.active_process_count()
            while require_empty and active and time.monotonic() < deadline:
                time.sleep(0.05)
                active = job.active_process_count()
            self.job_active_after_root_exit = active
            if require_empty and active:
                raise RuntimeError(
                    f"{active} engine descendant process(es) survived root shutdown"
                )
        except BaseException as error:
            self.job_errors.append(f"{type(error).__name__}: {error}")
            if require_empty:
                raise
        finally:
            try:
                job.close()
            except BaseException as error:
                self.job_errors.append(f"{type(error).__name__}: {error}")
                if require_empty:
                    raise
            else:
                self._job = None

    def _close_handles(self) -> None:
        for thread in self._threads:
            thread.join(timeout=5)
        for handle in (self._stdout_handle, self._stderr_handle):
            if handle is not None and not handle.closed:
                handle.close()

    def force_stop(self) -> int | None:
        process = self.process
        try:
            if self._job is not None:
                try:
                    self._job.terminate(1)
                except BaseException as error:
                    self.job_errors.append(f"{type(error).__name__}: {error}")
            if process is not None and process.poll() is None:
                try:
                    process.kill()
                except BaseException as error:
                    self.job_errors.append(f"{type(error).__name__}: {error}")
                try:
                    process.wait(timeout=10)
                except BaseException as error:
                    self.job_errors.append(f"{type(error).__name__}: {error}")
        finally:
            try:
                if self._job is not None:
                    self._finalize_job(require_empty=False)
            except BaseException as error:
                self.job_errors.append(f"{type(error).__name__}: {error}")
            finally:
                try:
                    self._close_handles()
                except BaseException as error:
                    self.job_errors.append(f"{type(error).__name__}: {error}")

        if process is not None:
            try:
                process_alive = process.poll() is None
            except BaseException as error:
                self.job_errors.append(f"{type(error).__name__}: {error}")
                raise RuntimeError(
                    "could not prove the engine process stopped during forced cleanup"
                ) from error
            if process_alive:
                message = "engine process survived forced cleanup"
                self.job_errors.append(message)
                raise RuntimeError(message)
        return process.returncode if process is not None else None

    def job_summary(self) -> dict[str, Any]:
        return {
            "assigned": self.job_assigned,
            "startedSuspended": self.process_started_suspended,
            "resumedAfterAssignment": self.process_resumed_after_assignment,
            "activeProcessesAfterRootExit": self.job_active_after_root_exit,
            "errors": self.job_errors[:20],
            "checks": {
                "jobObjectAssigned": self.job_assigned,
                "processStartedSuspended": self.process_started_suspended,
                "processResumedAfterAssignment": self.process_resumed_after_assignment,
                "noDescendantsAfterRootExit": self.job_active_after_root_exit == 0,
                "jobObjectOperationsSucceeded": not self.job_errors,
                "jobHandleClosed": self._job is None,
            },
        }


def health_barrier(driver: EngineDriver, label: str) -> dict[str, Any]:
    request_id = f"{label}-{uuid.uuid4()}"
    command_id = f"health-{request_id}"
    driver.send(command_id, {"healthCheck": {"requestID": request_id}})
    event = driver.wait_for(
        "healthCheckResult",
        command_id=command_id,
        timeout_seconds=30,
        predicate=lambda value: (
            isinstance(value, dict) and value.get("requestID") == request_id
        ),
    )
    end = driver.wait_for_quiet()
    window = driver.events_between(driver.command_mark(command_id), end)
    health_results = [
        inner_payload(item.value, "healthCheckResult")
        for item in window
        if event_kind(item.value) == "healthCheckResult"
    ]
    unexpected = [
        event_kind(item.value)
        for item in window
        if event_kind(item.value) not in {"healthCheckResult", "log", "queueState"}
    ]
    if (
        len(health_results) != 1
        or not isinstance(health_results[0], dict)
        or health_results[0].get("requestID") != request_id
    ):
        raise RuntimeError(f"health barrier {label} was not nonce-correlated exactly once")
    if unexpected:
        raise RuntimeError(
            f"health barrier {label} observed unexpected events: {unexpected}"
        )
    assert driver.process is not None
    if int(health_results[0].get("pid", -1)) != driver.process.pid:
        raise RuntimeError(f"health barrier {label} came from a different engine PID")
    return {
        "requestID": request_id,
        "pid": driver.process.pid,
        "eventSequence": event.sequence,
        "windowEnd": end,
    }


def settle_command(
    driver: EngineDriver,
    command_id: str,
    expected_terminals: dict[str, int],
) -> dict[str, Any]:
    end = driver.wait_for_quiet()
    window = driver.events_between(driver.command_mark(command_id), end)
    terminal_counts = collections.Counter(
        kind
        for item in window
        if (kind := event_kind(item.value)) in OPERATION_TERMINAL_KINDS
    )
    expected = collections.Counter(expected_terminals)
    if terminal_counts != expected:
        raise RuntimeError(
            f"command {command_id} terminal mismatch: "
            f"expected {dict(expected)}, observed {dict(terminal_counts)}"
        )
    barrier = health_barrier(driver, f"after-{command_id}")
    return {
        "eventWindowStart": driver.command_mark(command_id),
        "eventWindowEnd": end,
        "terminalCounts": dict(terminal_counts),
        "healthBarrier": barrier,
    }


def process_snapshot() -> dict[int, tuple[int, str]]:
    if os.name != "nt":
        raise RuntimeError("process snapshots require Windows Toolhelp APIs")

    class ProcessEntry32W(ctypes.Structure):
        _fields_ = [
            ("dwSize", ctypes.c_ulong),
            ("cntUsage", ctypes.c_ulong),
            ("th32ProcessID", ctypes.c_ulong),
            ("th32DefaultHeapID", ctypes.c_size_t),
            ("th32ModuleID", ctypes.c_ulong),
            ("cntThreads", ctypes.c_ulong),
            ("th32ParentProcessID", ctypes.c_ulong),
            ("pcPriClassBase", ctypes.c_long),
            ("dwFlags", ctypes.c_ulong),
            ("szExeFile", ctypes.c_wchar * 260),
        ]

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateToolhelp32Snapshot.argtypes = [ctypes.c_ulong, ctypes.c_ulong]
    kernel32.CreateToolhelp32Snapshot.restype = ctypes.c_void_p
    kernel32.Process32FirstW.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ProcessEntry32W),
    ]
    kernel32.Process32FirstW.restype = ctypes.c_int
    kernel32.Process32NextW.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ProcessEntry32W),
    ]
    kernel32.Process32NextW.restype = ctypes.c_int
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    ctypes.set_last_error(0)
    snapshot = kernel32.CreateToolhelp32Snapshot(0x00000002, 0)
    invalid = ctypes.c_void_p(-1).value
    if not snapshot or snapshot == invalid:
        raise OSError(
            ctypes.get_last_error(),
            "CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS) failed",
        )
    entry = ProcessEntry32W()
    entry.dwSize = ctypes.sizeof(entry)
    result: dict[int, tuple[int, str]] = {}
    try:
        ctypes.set_last_error(0)
        if not kernel32.Process32FirstW(snapshot, ctypes.byref(entry)):
            raise OSError(
                ctypes.get_last_error(),
                "Process32FirstW failed",
            )
        while True:
            result[int(entry.th32ProcessID)] = (
                int(entry.th32ParentProcessID),
                entry.szExeFile,
            )
            ctypes.set_last_error(0)
            if kernel32.Process32NextW(snapshot, ctypes.byref(entry)):
                continue
            error = ctypes.get_last_error()
            if error == 18:
                break
            raise OSError(error, "Process32NextW failed before enumeration completed")
    finally:
        ctypes.set_last_error(0)
        if not kernel32.CloseHandle(snapshot):
            raise OSError(
                ctypes.get_last_error(),
                "CloseHandle(process snapshot) failed",
            )
    return result


class FileIDProcessGuard:
    def __init__(self, label: str) -> None:
        self.label = label
        self._stop = threading.Event()
        self._thread = threading.Thread(
            target=self._run,
            name=f"fileid-process-guard-{label}",
            daemon=True,
        )
        self._observed: dict[int, str] = {}
        self._errors: list[str] = []
        self.samples = 0

    @staticmethod
    def _active() -> list[tuple[int, str]]:
        snapshot = process_snapshot()
        if os.name == "nt" and os.getpid() not in snapshot:
            raise RuntimeError("could not obtain a trustworthy Windows process snapshot")
        return sorted(
            (pid, executable)
            for pid, (_, executable) in snapshot.items()
            if Path(executable).stem.casefold().startswith("fileid")
        )

    @staticmethod
    def _describe(processes: Iterable[tuple[int, str]]) -> str:
        return ", ".join(f"{name} (PID {pid})" for pid, name in processes)

    def start(self) -> None:
        active = self._active()
        self.samples += 1
        if active:
            raise RuntimeError(
                f"close FileID and FileIDEngine before {self.label}: "
                + self._describe(active)
            )
        self._thread.start()

    def _run(self) -> None:
        while not self._stop.wait(0.05):
            try:
                active = self._active()
                self.samples += 1
                for pid, name in active:
                    self._observed[pid] = name
            except (OSError, RuntimeError) as error:
                self._errors.append(str(error))

    def stop_and_assert_clean(self) -> None:
        self._stop.set()
        self._thread.join(timeout=10)
        try:
            active = self._active()
            self.samples += 1
            for pid, name in active:
                self._observed[pid] = name
        except (OSError, RuntimeError) as error:
            self._errors.append(str(error))
        if self._thread.is_alive():
            self._errors.append("process guard thread did not stop")
        if self._errors:
            raise RuntimeError(
                f"could not prove FileID stayed closed during {self.label}: "
                + "; ".join(self._errors)
            )
        if self._observed:
            raise RuntimeError(
                f"FileID or FileIDEngine ran during {self.label}: "
                + self._describe(sorted(self._observed.items()))
            )


def working_set_mb(pid: int) -> float | None:
    if os.name != "nt":
        return None

    class ProcessMemoryCounters(ctypes.Structure):
        _fields_ = [
            ("cb", ctypes.c_ulong),
            ("PageFaultCount", ctypes.c_ulong),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    kernel32.OpenProcess.restype = ctypes.c_void_p
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    psapi.GetProcessMemoryInfo.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ProcessMemoryCounters),
        ctypes.c_ulong,
    ]
    handle = kernel32.OpenProcess(0x0400 | 0x0010, False, pid)
    if not handle:
        return None
    counters = ProcessMemoryCounters()
    counters.cb = ctypes.sizeof(counters)
    try:
        if not psapi.GetProcessMemoryInfo(
            handle, ctypes.byref(counters), counters.cb
        ):
            return None
        return float(counters.WorkingSetSize) / (1024 * 1024)
    finally:
        kernel32.CloseHandle(handle)


class ResourceMonitor:
    def __init__(self, process: subprocess.Popen[str]) -> None:
        self.process = process
        self.root_pid = process.pid
        self.stop_event = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.engine_peak_rss_mb = 0.0
        self.children: dict[int, dict[str, Any]] = {}
        self.live_root_samples = 0
        self.errors: list[str] = []
        self.started = False

    def start(self) -> None:
        self.thread.start()
        self.started = True

    def stop(self) -> None:
        self.stop_event.set()
        if self.started:
            self.thread.join(timeout=10)
            if self.thread.is_alive():
                self.errors.append("resource monitor thread did not stop")

    def _descendants(
        self, snapshot: dict[int, tuple[int, str]]
    ) -> dict[int, str]:
        selected = {self.root_pid}
        changed = True
        while changed:
            changed = False
            for pid, (parent, _name) in snapshot.items():
                if parent in selected and pid not in selected:
                    selected.add(pid)
                    changed = True
        return {
            pid: snapshot[pid][1]
            for pid in selected
            if pid != self.root_pid and pid in snapshot
        }

    def _run(self) -> None:
        while not self.stop_event.is_set():
            try:
                if self.process.poll() is not None:
                    break
                snapshot = process_snapshot()
                if self.root_pid not in snapshot:
                    if self.process.poll() is not None:
                        break
                    raise RuntimeError(
                        "live engine root PID was absent from the complete "
                        f"process snapshot: {self.root_pid}"
                    )
                rss = working_set_mb(self.root_pid)
                if rss is None:
                    if self.process.poll() is not None:
                        break
                    raise RuntimeError(
                        f"working-set query failed for live engine PID {self.root_pid}"
                    )
                if self.process.poll() is not None:
                    break
                self.live_root_samples += 1
                self.engine_peak_rss_mb = max(self.engine_peak_rss_mb, rss)
                descendants = self._descendants(snapshot)
                for pid, name in descendants.items():
                    child_rss = working_set_mb(pid)
                    record = self.children.setdefault(
                        pid, {"name": name, "peakRSSMB": 0.0}
                    )
                    if child_rss is not None:
                        record["peakRSSMB"] = max(record["peakRSSMB"], child_rss)
            except BaseException as error:
                self.errors.append(f"{type(error).__name__}: {error}")
                self.stop_event.set()
                break
            if self.stop_event.wait(0.5):
                break

    def summary(self) -> dict[str, Any]:
        return {
            "enginePeakRSSMB": round(self.engine_peak_rss_mb, 2),
            "children": [
                {"pid": pid, **value}
                for pid, value in sorted(self.children.items())
            ],
            "liveRootSamples": self.live_root_samples,
            "monitorErrors": self.errors[:20],
            "checks": {
                "monitorStarted": self.started,
                "monitorThreadStopped": not self.thread.is_alive(),
                "liveRootProcessProven": self.live_root_samples > 0,
                "monitorErrorFree": not self.errors,
            },
        }


def corpus_index(root: Path) -> tuple[set[str], set[str], list[str]]:
    files: set[str] = set()
    directories: set[str] = {normalized(root)}
    errors: list[str] = []
    try:
        root_info, root_identity = path_identity_no_reparse(
            root, expect_directory=True
        )
    except (OSError, ValueError) as error:
        return files, directories, [f"{root}: {error}"]
    seen_directories = {root_identity: normalized(root)}
    stack: list[tuple[Path, tuple[int, int], int, int]] = [
        (root, root_identity, root_info.st_size, root_info.st_mtime_ns)
    ]
    while stack:
        directory, expected_identity, expected_size, expected_mtime_ns = stack.pop()
        try:
            before, before_identity = path_identity_no_reparse(
                directory, expect_directory=True
            )
            if (
                before_identity != expected_identity
                or before.st_size != expected_size
                or before.st_mtime_ns != expected_mtime_ns
            ):
                raise ValueError(f"directory identity changed before traversal: {directory}")
            with os.scandir(directory) as iterator:
                entries = sorted(iterator, key=lambda entry: os.fsencode(entry.name))
            after, after_identity = path_identity_no_reparse(
                directory, expect_directory=True
            )
            if (
                after_identity != expected_identity
                or after.st_size != before.st_size
                or after.st_mtime_ns != before.st_mtime_ns
            ):
                raise ValueError(f"directory changed during traversal: {directory}")
        except (OSError, ValueError) as error:
            errors.append(f"{directory}: {error}")
            continue
        child_directories: list[tuple[Path, tuple[int, int], int, int]] = []
        for entry in entries:
            try:
                info, identity = path_identity_no_reparse(Path(entry.path))
                if stat.S_ISDIR(info.st_mode):
                    if identity in seen_directories:
                        errors.append(
                            "directory identity was visited twice: "
                            f"{entry.path} and {seen_directories[identity]}"
                        )
                        continue
                    directories.add(normalized(entry.path))
                    seen_directories[identity] = normalized(entry.path)
                    child_directories.append(
                        (
                            Path(entry.path),
                            identity,
                            info.st_size,
                            info.st_mtime_ns,
                        )
                    )
                elif stat.S_ISREG(info.st_mode):
                    files.add(normalized(entry.path))
                else:
                    errors.append(f"non-regular corpus entry is not allowed: {entry.path}")
            except (OSError, ValueError) as error:
                errors.append(f"{entry.path}: {error}")
        stack.extend(reversed(child_directories))
    return files, directories, errors


def merge_oracle_embedding(blob: Any) -> tuple[float, ...] | None:
    if not isinstance(blob, (bytes, bytearray, memoryview)) or len(blob) != 512:
        return None
    values = struct.unpack("<128f", bytes(blob))
    if not all(math.isfinite(value) for value in values):
        return None
    norm = math.sqrt(sum(value * value for value in values))
    if not math.isfinite(norm) or norm <= 1e-12:
        return None
    return tuple(value / norm for value in values)


def merge_oracle_identity(row: sqlite3.Row) -> dict[str, Any]:
    def clean(value: Any) -> str:
        return " ".join(str(value or "").split()).casefold()

    name = clean(row["name"])
    title = clean(row["title"])
    first = clean(row["first_name"])
    middle = clean(row["middle_name"])
    last = clean(row["last_name"])
    suffix = clean(row["suffix"])
    structured = clean(" ".join((first, middle, last)))
    decorated = clean(" ".join((title, first, middle, last, suffix)))
    legacy_full_first = bool(name and first == name and not middle and not last)
    return {
        "first": "" if legacy_full_first else first,
        "last": last,
        "fullNames": frozenset(
            value for value in (name, structured, decorated) if value
        ),
    }


def merge_oracle_identities_conflict(
    source: dict[str, Any], destination: dict[str, Any]
) -> bool:
    if (
        source["first"]
        and destination["first"]
        and source["first"] != destination["first"]
    ):
        return True
    if (
        source["last"]
        and destination["last"]
        and source["last"] != destination["last"]
    ):
        return True
    return bool(
        source["fullNames"]
        and destination["fullNames"]
        and source["fullNames"].isdisjoint(destination["fullNames"])
    )


def merge_suggestion_db_oracle(
    db_path: Path, reported_pairs: list[Any]
) -> dict[str, Any]:
    def pair_key(left: int, right: int) -> tuple[int, int]:
        return (left, right) if left < right else (right, left)

    with connect_readonly(db_path) as connection:
        rows = connection.execute(
            "SELECT p.id,p.name,p.title,p.first_name,p.middle_name,"
            "p.last_name,p.suffix,p.centroid,rep.id AS anchor_face_id,"
            "rep.arcface_embedding AS representative,COUNT(member.id) AS members "
            "FROM persons p "
            "JOIN face_prints rep ON rep.id=p.representative_face_id "
            "AND rep.arcface_embedding IS NOT NULL "
            "AND COALESCE(rep.excluded,0)=0 "
            "JOIN face_prints member ON member.person_id=p.id "
            "AND COALESCE(member.excluded,0)=0 "
            "WHERE COALESCE(p.is_unknown,0)=0 GROUP BY p.id ORDER BY p.id"
        ).fetchall()
        people: dict[int, dict[str, Any]] = {}
        invalid_embeddings: list[int] = []
        for row in rows:
            embedding = merge_oracle_embedding(row["centroid"])
            if embedding is None:
                embedding = merge_oracle_embedding(row["representative"])
            if embedding is None:
                invalid_embeddings.append(int(row["id"]))
                continue
            people[int(row["id"])] = {
                "anchorFaceID": int(row["anchor_face_id"]),
                "embedding": embedding,
                "identity": merge_oracle_identity(row),
            }

        blocked_person_pairs: set[tuple[int, int]] = set()
        blocked_face_pairs: set[tuple[int, int]] = set()
        blocked_owner_pairs: set[tuple[int, int]] = set()
        verdict_rows = connection.execute(
            "SELECT person_a,person_b,face_a,face_b,file_a,bbox_a,file_b,bbox_b "
            "FROM face_verifications WHERE same_person=0 "
            "ORDER BY person_a,person_b"
        ).fetchall()
        files_by_person: dict[int, set[int]] = collections.defaultdict(set)
        for membership in connection.execute(
            "SELECT person_id,file_id FROM face_prints "
            "WHERE person_id IS NOT NULL AND COALESCE(excluded,0)=0 "
            "ORDER BY person_id,file_id"
        ):
            files_by_person[int(membership["person_id"])].add(
                int(membership["file_id"])
            )

        def resolve_face(
            legacy_id: Any, file_id: Any, bbox: Any
        ) -> tuple[int, int | None] | None:
            if file_id is not None and bbox is not None:
                current = connection.execute(
                    "SELECT id,person_id FROM face_prints "
                    "WHERE file_id=? AND bbox=? ORDER BY id LIMIT 1",
                    (file_id, bbox),
                ).fetchone()
                if current is not None:
                    return (
                        int(current["id"]),
                        int(current["person_id"])
                        if current["person_id"] is not None
                        else None,
                    )
            if legacy_id is None:
                return None
            current = connection.execute(
                "SELECT id,person_id FROM face_prints WHERE id=?",
                (legacy_id,),
            ).fetchone()
            if current is None:
                return None
            return (
                int(current["id"]),
                int(current["person_id"])
                if current["person_id"] is not None
                else None,
            )

        resolved_verdicts = 0
        for verdict in verdict_rows:
            person_a = int(verdict["person_a"])
            person_b = int(verdict["person_b"])
            if person_a != person_b:
                blocked_person_pairs.add(pair_key(person_a, person_b))
            face_a = resolve_face(
                verdict["face_a"], verdict["file_a"], verdict["bbox_a"]
            )
            face_b = resolve_face(
                verdict["face_b"], verdict["file_b"], verdict["bbox_b"]
            )
            if face_a is None or face_b is None or face_a[0] == face_b[0]:
                continue
            resolved_verdicts += 1
            blocked_face_pairs.add(pair_key(face_a[0], face_b[0]))
            if (
                face_a[1] is not None
                and face_b[1] is not None
                and face_a[1] != face_b[1]
            ):
                blocked_owner_pairs.add(pair_key(face_a[1], face_b[1]))

    person_ids = sorted(people)
    exact = len(person_ids) <= 2_000
    comparison_pairs: Iterable[tuple[int, int]]
    if exact:
        comparison_pairs = (
            (person_ids[left], person_ids[right])
            for left in range(len(person_ids))
            for right in range(left + 1, len(person_ids))
        )
    else:
        comparison_pairs = (
            (person_ids[left], person_ids[right])
            for left in range(len(person_ids))
            for right in range(left + 1, min(len(person_ids), left + 65))
        )

    digest = hashlib.sha256(b"fileid-merge-suggestion-oracle-v1\0")
    eligible_pairs: set[tuple[int, int]] = set()
    evidence: list[dict[str, Any]] = []
    comparisons = 0
    named_conflicts = 0
    verdict_suppressed = 0
    same_file_suppressed = 0
    dimension_mismatches = 0
    for person_a, person_b in comparison_pairs:
        comparisons += 1
        left = people[person_a]
        right = people[person_b]
        left_embedding = left["embedding"]
        right_embedding = right["embedding"]
        if len(left_embedding) != len(right_embedding):
            dimension_mismatches += 1
            continue
        similarity = sum(
            a * b for a, b in zip(left_embedding, right_embedding, strict=True)
        )
        if not 0.55 <= similarity < 0.97:
            continue
        person_pair = pair_key(person_a, person_b)
        left_files = files_by_person.get(person_a)
        right_files = files_by_person.get(person_b)
        if (
            left_files is None
            or right_files is None
            or not left_files.isdisjoint(right_files)
        ):
            same_file_suppressed += 1
            continue
        if merge_oracle_identities_conflict(
            left["identity"], right["identity"]
        ):
            named_conflicts += 1
            continue
        face_pair = pair_key(left["anchorFaceID"], right["anchorFaceID"])
        if (
            person_pair in blocked_person_pairs
            or person_pair in blocked_owner_pairs
            or face_pair in blocked_face_pairs
        ):
            verdict_suppressed += 1
            continue
        eligible_pairs.add(person_pair)
        encoded = json.dumps(
            [person_a, person_b, format(similarity, ".9f")],
            separators=(",", ":"),
        ).encode("ascii")
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
        if len(evidence) < 20:
            evidence.append(
                {
                    "leftPersonID": person_a,
                    "rightPersonID": person_b,
                    "similarity": similarity,
                }
            )

    reported_keys = {
        pair_key(int(pair["sourcePersonID"]), int(pair["destinationPersonID"]))
        for pair in reported_pairs
        if isinstance(pair, dict)
        and isinstance(pair.get("sourcePersonID"), int)
        and not isinstance(pair.get("sourcePersonID"), bool)
        and isinstance(pair.get("destinationPersonID"), int)
        and not isinstance(pair.get("destinationPersonID"), bool)
        and pair.get("sourcePersonID") != pair.get("destinationPersonID")
    }
    witness_surfaced = not eligible_pairs or bool(eligible_pairs & reported_keys)
    return {
        "algorithm": "face-merge-db-oracle-v1",
        "mode": "exact" if exact else "conservative-window-witness",
        "candidatePersonRows": len(rows),
        "decodedPeople": len(people),
        "invalidEmbeddingPersonIDs": invalid_embeddings[:50],
        "comparisonCount": comparisons,
        "eligiblePairCount": len(eligible_pairs),
        "eligibleCountIsLowerBound": not exact,
        "namedConflictCount": named_conflicts,
        "verdictSuppressedCount": verdict_suppressed,
        "sameFileSuppressedCount": same_file_suppressed,
        "differentPersonVerdictCount": len(verdict_rows),
        "resolvedDifferentPersonVerdictCount": resolved_verdicts,
        "dimensionMismatchCount": dimension_mismatches,
        "eligibleDigest": digest.hexdigest(),
        "eligibleEvidence": evidence,
        "checks": {
            "candidateEmbeddingsDecoded": not invalid_embeddings,
            "eligibleOracleWitnessSurfaced": witness_surfaced,
        },
    }


def validate_merge_suggestions(
    payload: dict[str, Any], db_path: Path
) -> dict[str, Any]:
    pairs = payload.get("pairs")
    if set(payload) != {"pairs"} or not isinstance(pairs, list):
        raise ValueError("mergeSuggestions payload must contain only a pairs array")
    required = {
        "sourcePersonID",
        "destinationPersonID",
        "similarity",
        "sourceAnchorFaceID",
        "destinationAnchorFaceID",
        "sourceMemberCount",
        "destinationMemberCount",
    }

    def normalize_name(value: Any) -> str:
        return " ".join(str(value or "").split()).casefold()

    def person_identity(row: sqlite3.Row) -> dict[str, Any]:
        name = normalize_name(row["name"])
        title = normalize_name(row["title"])
        first = normalize_name(row["first_name"])
        middle = normalize_name(row["middle_name"])
        last = normalize_name(row["last_name"])
        suffix = normalize_name(row["suffix"])
        structured = normalize_name(" ".join((first, middle, last)))
        decorated = normalize_name(" ".join((title, first, middle, last, suffix)))
        full_names = frozenset(
            value for value in (name, structured, decorated) if value
        )
        legacy_full_first = bool(name and first == name and not middle and not last)
        return {
            "first": "" if legacy_full_first else first,
            "last": last,
            "fullNames": full_names,
        }

    def identities_conflict(
        source: dict[str, Any], destination: dict[str, Any]
    ) -> bool:
        if (
            source["first"]
            and destination["first"]
            and source["first"] != destination["first"]
        ):
            return True
        if (
            source["last"]
            and destination["last"]
            and source["last"] != destination["last"]
        ):
            return True
        return bool(
            source["fullNames"]
            and destination["fullNames"]
            and source["fullNames"].isdisjoint(destination["fullNames"])
        )

    with connect_readonly(db_path) as connection:
        people = {
            int(row["id"]): {
                "identity": person_identity(row),
                "members": int(row["members"]),
            }
            for row in connection.execute(
                "SELECT p.id,p.name,p.title,p.first_name,p.middle_name,"
                "p.last_name,p.suffix,COUNT(f.id) AS members "
                "FROM persons p LEFT JOIN face_prints f ON f.person_id=p.id "
                "AND COALESCE(f.excluded,0)=0 "
                "GROUP BY p.id"
            )
        }
        face_owners = {
            int(row["id"]): (
                int(row["person_id"]) if row["person_id"] is not None else None
            )
            for row in connection.execute("SELECT id,person_id FROM face_prints")
        }
        files_by_person: dict[int, set[int]] = collections.defaultdict(set)
        for row in connection.execute(
            "SELECT person_id,file_id FROM face_prints "
            "WHERE person_id IS NOT NULL AND COALESCE(excluded,0)=0"
        ):
            files_by_person[int(row["person_id"])].add(int(row["file_id"]))
    malformed: list[int] = []
    missing_people: list[int] = []
    bad_anchors: list[int] = []
    bad_member_counts: list[int] = []
    self_pairs: list[int] = []
    named_conflicts: list[int] = []
    cooccurring_pairs: list[int] = []
    duplicate_pairs: list[int] = []
    non_finite_or_out_of_range: list[int] = []
    seen: set[tuple[int, int]] = set()
    similarities: list[float] = []
    for index, pair in enumerate(pairs):
        if not isinstance(pair, dict) or set(pair) != required:
            malformed.append(index)
            continue
        integer_fields = required - {"similarity"}
        if any(
            isinstance(pair[field], bool) or not isinstance(pair[field], int)
            for field in integer_fields
        ):
            malformed.append(index)
            continue
        source = int(pair["sourcePersonID"])
        destination = int(pair["destinationPersonID"])
        similarity = float(pair["similarity"])
        similarities.append(similarity)
        if not math.isfinite(similarity) or not 0 <= similarity <= 1:
            non_finite_or_out_of_range.append(index)
        if source == destination:
            self_pairs.append(index)
        canonical = tuple(sorted((source, destination)))
        if canonical in seen:
            duplicate_pairs.append(index)
        seen.add(canonical)
        if source not in people or destination not in people:
            missing_people.append(index)
            continue
        if (
            face_owners.get(int(pair["sourceAnchorFaceID"])) != source
            or face_owners.get(int(pair["destinationAnchorFaceID"])) != destination
        ):
            bad_anchors.append(index)
        if (
            int(pair["sourceMemberCount"]) != people[source]["members"]
            or int(pair["destinationMemberCount"]) != people[destination]["members"]
        ):
            bad_member_counts.append(index)
        if identities_conflict(
            people[source]["identity"], people[destination]["identity"]
        ):
            named_conflicts.append(index)
        source_files = files_by_person.get(source)
        destination_files = files_by_person.get(destination)
        if (
            source_files is None
            or destination_files is None
            or not source_files.isdisjoint(destination_files)
        ):
            cooccurring_pairs.append(index)
    oracle = merge_suggestion_db_oracle(db_path, pairs)
    thresholds = (0.45, 0.50, 0.60, 0.70, 0.75, 0.80, 0.85, 0.88)
    return {
        "count": len(pairs),
        "similarityAtLeast": {
            f"{threshold:.2f}": sum(
                1 for similarity in similarities if similarity >= threshold
            )
            for threshold in thresholds
        },
        "topPairs": pairs[:20],
        "independentDBOracle": oracle,
        "checks": {
            "payloadShapeValid": not malformed,
            "similaritiesFiniteAndInRange": not non_finite_or_out_of_range,
            "noSelfPairs": not self_pairs,
            "noDuplicatePairs": not duplicate_pairs,
            "allPeopleExist": not missing_people,
            "anchorsBelongToPeople": not bad_anchors,
            "memberCountsMatchDB": not bad_member_counts,
            "noConflictingNamedPair": not named_conflicts,
            "noSameFilePair": not cooccurring_pairs,
            "boundedResult": len(pairs) <= 50,
            "independentDBOracleEligibleWitnessSurfaced": oracle["checks"][
                "eligibleOracleWitnessSurfaced"
            ],
        },
        "violations": {
            "malformedIndexes": malformed[:50],
            "nonFiniteOrOutOfRangeIndexes": non_finite_or_out_of_range[:50],
            "selfPairIndexes": self_pairs[:50],
            "duplicatePairIndexes": duplicate_pairs[:50],
            "missingPeopleIndexes": missing_people[:50],
            "badAnchorIndexes": bad_anchors[:50],
            "badMemberCountIndexes": bad_member_counts[:50],
            "namedConflictIndexes": named_conflicts[:50],
            "cooccurringPairIndexes": cooccurring_pairs[:50],
        },
    }


def plan_event_counts(payload: dict[str, Any]) -> tuple[dict[str, int], dict[str, int]]:
    raw_categories = payload.get("categoryCounts")
    if not isinstance(raw_categories, list):
        raise ValueError("restructurePlan categoryCounts must be an array")
    categories: dict[str, int] = {}
    for index, row in enumerate(raw_categories):
        if (
            not isinstance(row, dict)
            or set(row) != {"category", "count"}
            or not isinstance(row.get("category"), str)
            or not str(row["category"]).strip()
            or isinstance(row.get("count"), bool)
            or not isinstance(row.get("count"), int)
            or int(row["count"]) < 0
        ):
            raise ValueError(
                f"restructurePlan categoryCounts row {index} is invalid"
            )
        category = str(row["category"])
        if category in categories:
            raise ValueError(
                f"restructurePlan categoryCounts duplicates {category!r}"
            )
        categories[category] = int(row["count"])
    raw_confidence = payload.get("confidenceCounts")
    if (
        not isinstance(raw_confidence, dict)
        or set(raw_confidence) != {"auto", "review", "ask", "unknown"}
        or any(
            isinstance(value, bool)
            or not isinstance(value, int)
            or value < 0
            for value in raw_confidence.values()
        )
    ):
        raise ValueError("restructurePlan confidenceCounts is invalid")
    confidence = {
        str(key): int(value) for key, value in raw_confidence.items()
    }
    return categories, confidence


def plan_digest(moves: list[dict[str, Any]], *, ordered: bool) -> str:
    rows = [
        (
            int(move.get("fileID", -1)),
            str(move.get("source", "")),
            str(move.get("destination", "")),
            str(move.get("category", "")),
            str(move.get("tier", "")),
            str(move.get("confidence", "")),
            str(move.get("reason", "")),
        )
        for move in moves
    ]
    if not ordered:
        rows.sort()
    digest = hashlib.sha256()
    for row in rows:
        encoded = json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode(
            "utf-8", "surrogatepass"
        )
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
    return digest.hexdigest()


@contextlib.contextmanager
def read_plan_spool_pages(
    spool: Path, page_size: int = RESTRUCTURE_PAGE_SIZE
) -> Iterator[tuple[dict[str, Any], Iterator[list[dict[str, Any]]]]]:
    if page_size <= 0:
        raise ValueError("plan page size must be positive")
    spool_info = spool.lstat()
    if is_reparse_point(spool_info) or not stat.S_ISREG(spool_info.st_mode):
        raise ValueError(f"plan spool is not a regular non-reparse file: {spool}")
    with spool.open("r", encoding="utf-8") as handle:
        header_line = handle.readline()
        if not header_line:
            raise ValueError(f"empty plan spool: {spool}")
        header = strict_json_loads(header_line)
        if not isinstance(header, dict) or set(header) != {
            "version",
            "libraryRoot",
            "totalMoves",
        }:
            raise ValueError(f"invalid stored plan header: {spool}")
        if (
            isinstance(header["version"], bool)
            or not isinstance(header["version"], int)
            or not isinstance(header["libraryRoot"], str)
            or isinstance(header["totalMoves"], bool)
            or not isinstance(header["totalMoves"], int)
            or header["totalMoves"] < 0
        ):
            raise ValueError(f"invalid stored plan header value types: {spool}")

        def iter_pages() -> Iterator[list[dict[str, Any]]]:
            page: list[dict[str, Any]] = []
            for line_number, line in enumerate(handle, 2):
                if not line.strip():
                    raise ValueError(f"blank stored plan row at line {line_number}")
                move = strict_json_loads(line)
                if not isinstance(move, dict):
                    raise ValueError(
                        f"stored plan row {line_number} is not an object"
                    )
                page.append(move)
                if len(page) == page_size:
                    yield page
                    page = []
            if page:
                yield page

        yield header, iter_pages()


def plan_digest_row(move: dict[str, Any]) -> bytes:
    row = (
        int(move.get("fileID", -1)),
        str(move.get("source", "")),
        str(move.get("destination", "")),
        str(move.get("category", "")),
        str(move.get("tier", "")),
        str(move.get("confidence", "")),
        str(move.get("reason", "")),
    )
    return json.dumps(
        row, ensure_ascii=False, separators=(",", ":")
    ).encode("utf-8", "surrogatepass")


class StreamingPlanDigests:
    def __init__(self) -> None:
        self._ordered = hashlib.sha256()
        self._sum = 0
        self._xor = 0
        self._count = 0

    def add(self, move: dict[str, Any]) -> None:
        encoded = plan_digest_row(move)
        framed = len(encoded).to_bytes(8, "big") + encoded
        self._ordered.update(framed)
        leaf = int.from_bytes(hashlib.sha256(framed).digest(), "big")
        self._sum = (self._sum + leaf) % (1 << 256)
        self._xor ^= leaf
        self._count += 1

    def ordered(self) -> str:
        return self._ordered.hexdigest()

    def canonical(self) -> str:
        digest = hashlib.sha256(b"fileid-plan-multiset-v1\0")
        digest.update(self._count.to_bytes(16, "big"))
        digest.update(self._sum.to_bytes(32, "big"))
        digest.update(self._xor.to_bytes(32, "big"))
        return digest.hexdigest()


def windows_path_key(path: str | Path) -> tuple[str, ...]:
    parsed = PureWindowsPath(path)
    result: list[str] = []
    for index, component in enumerate(parsed.parts):
        if index == 0 and component == parsed.anchor:
            result.append(component.rstrip("\\/").casefold())
        else:
            result.append(component.rstrip(" .").casefold())
    return tuple(result)


def windows_path_violations(path: str, corpus_root: Path) -> list[str]:
    violations: list[str] = []
    raw_components = path.replace("/", "\\").split("\\")
    if "." in raw_components:
        violations.append("dotSegment")
    if ".." in raw_components:
        violations.append("parentSegment")
    parsed = PureWindowsPath(path)
    if not parsed.is_absolute():
        violations.append("notAbsolute")
        return violations
    try:
        relative_path = parsed.relative_to(PureWindowsPath(corpus_root))
    except ValueError:
        violations.append("notInsideRoot")
        return violations
    if relative_path == PureWindowsPath(".") or any(
        component == ".." for component in relative_path.parts
    ):
        violations.append("notStrictlyInsideRoot")
    if not parsed.name:
        violations.append("doesNotNameFile")
    try:
        if len(path.encode("utf-16-le")) // 2 >= 32_767:
            violations.append("pathExceedsNtLimit")
    except UnicodeEncodeError:
        violations.append("invalidUnicode")
    for component in relative_path.parts:
        if component in {".", ".."}:
            continue
        if not component:
            violations.append("emptyComponent")
            continue
        if component.endswith((".", " ")):
            violations.append(f"trailingDotOrSpace:{component}")
        if any(
            character in WINDOWS_INVALID_COMPONENT_CHARACTERS
            or ord(character) < 0x20
            for character in component
        ):
            violations.append(f"invalidCharacter:{component}")
        reserved_stem = component.rstrip(" .").split(".", 1)[0].upper()
        if reserved_stem in WINDOWS_RESERVED_COMPONENTS or reserved_stem in {
            "CONIN$",
            "CONOUT$",
        }:
            violations.append(f"reservedDeviceName:{component}")
        try:
            if len(component.encode("utf-16-le")) // 2 > 255:
                violations.append(f"componentTooLong:{component[:80]}")
        except UnicodeEncodeError:
            violations.append(f"invalidComponentUnicode:{component[:80]}")
    return violations


RESTRUCTURE_GENERIC_FOLDERS = frozenset(
    {
        "downloads",
        "downloaded",
        "desktop",
        "unsorted",
        "inbox",
        "new folder",
        "untitled",
        "temp",
        "tmp",
        "misc",
        "other",
        "stuff",
        "things",
        "files",
    }
)
FOLDER_CLASSIFICATION_ORACLE = (
    "windows-large-plan-v1:failed0-root-bounds-person-gps-doc-kind"
)


def restructure_root_bounds(root: str) -> tuple[str, str, str]:
    separator = "\\" if "\\" in root else "/"
    trimmed = root.rstrip("/\\")
    prefix = f"{trimmed}{separator}"
    upper_bytes = bytearray(prefix.encode("utf-8"))
    for index in range(len(upper_bytes) - 1, -1, -1):
        if upper_bytes[index] != 0xFF:
            upper_bytes[index] += 1
            del upper_bytes[index + 1 :]
            return trimmed, prefix, upper_bytes.decode("utf-8", "replace")
    return trimmed, prefix, "\U0010ffff"


def safe_restructure_component(raw: str) -> str:
    illegal = frozenset('<>:"/\\|?*')
    value = "".join(
        "_" if ord(character) < 0x20 or character in illegal else character
        for character in raw
    )[:200].rstrip(". ")
    if not value:
        return "_"
    basename = "".join(
        chr(ord(character) + 32)
        if "A" <= character <= "Z"
        else character
        for character in value.split(".", 1)[0]
    )
    if basename in {
        "con",
        "prn",
        "aux",
        "nul",
        *(f"com{index}" for index in range(10)),
        *(f"lpt{index}" for index in range(10)),
    }:
        value = f"_{value}"
    return value


def restructure_oracle_category(row: sqlite3.Row | tuple[Any, ...]) -> str:
    if not isinstance(row[2], str):
        raise ValueError("restructure oracle kind is not text")
    kind = row[2]
    latitude = row[3]
    longitude = row[4]
    if row[5] is not None and not isinstance(row[5], int):
        raise ValueError("restructure oracle has_text is not an integer")
    has_text = int(row[5] or 0) != 0
    names = row[6]
    if names is not None and not isinstance(names, str):
        raise ValueError("restructure oracle person names are not text")
    face_count = int(row[7])
    named_face_count = int(row[8])
    named_person_count = int(row[9])
    person_name: str | None = None
    if (
        face_count > 0
        and face_count == named_face_count
        and named_person_count == 1
        and isinstance(names, str)
    ):
        candidates = [
            name.strip() for name in names.split("\x1f") if name.strip()
        ]
        if len(candidates) == 1:
            person_name = candidates[0]
    if person_name is not None:
        return f"People/{safe_restructure_component(person_name)}"
    if any(
        coordinate is not None
        and (
            isinstance(coordinate, bool)
            or not isinstance(coordinate, (int, float))
        )
        for coordinate in (latitude, longitude)
    ):
        raise ValueError("restructure oracle coordinates are not numeric")
    if latitude is not None and longitude is not None:
        latitude_value = float(latitude)
        longitude_value = float(longitude)
        if (
            math.isfinite(latitude_value)
            and math.isfinite(longitude_value)
            and -90.0 <= latitude_value <= 90.0
            and -180.0 <= longitude_value <= 180.0
        ):
            latitude_bucket = (
                math.copysign(
                    math.floor(abs(latitude_value * 2.0) + 0.5),
                    latitude_value,
                )
                / 2.0
            )
            longitude_bucket = (
                math.copysign(
                    math.floor(abs(longitude_value * 2.0) + 0.5),
                    longitude_value,
                )
                / 2.0
            )
            return f"Places/{latitude_bucket:.1f}_{longitude_bucket:.1f}"
    if has_text or kind in {"pdf", "doc"}:
        return "document"
    if kind == "image":
        return "photo"
    if kind == "video":
        return "video"
    if kind == "audio":
        return "audio"
    if kind == "model":
        return "model"
    return "misc"


def restructure_oracle_year_month(
    modified_at: Any, created_at: Any
) -> tuple[int, int] | None:
    timestamp = created_at if created_at is not None else modified_at
    if (
        isinstance(timestamp, bool)
        or not isinstance(timestamp, (int, float))
        or not math.isfinite(float(timestamp))
        or float(timestamp) <= 86_400.0
    ):
        return None
    try:
        value = datetime.fromtimestamp(float(timestamp), timezone.utc)
    except (OSError, OverflowError, ValueError):
        return None
    return value.year, value.month


def restructure_oracle_person_name(row: sqlite3.Row) -> str | None:
    names = row["names"]
    if names is not None and not isinstance(names, str):
        raise ValueError("restructure expected-move oracle person names are not text")
    face_count = int(row["face_count"])
    named_face_count = int(row["named_face_count"])
    named_person_count = int(row["named_person_count"])
    if (
        face_count <= 0
        or face_count != named_face_count
        or named_person_count != 1
        or not isinstance(names, str)
    ):
        return None
    candidates = [name.strip() for name in names.split("\x1f") if name.strip()]
    return candidates[0] if len(candidates) == 1 else None


def restructure_oracle_move(row: sqlite3.Row, corpus_root: Path) -> dict[str, Any]:
    source = row["path_text"]
    kind = row["kind"]
    if not isinstance(source, str) or not isinstance(kind, str):
        raise ValueError("restructure expected-move oracle path or kind is not text")
    has_text_raw = row["has_text"]
    if has_text_raw is not None and (
        isinstance(has_text_raw, bool) or not isinstance(has_text_raw, int)
    ):
        raise ValueError("restructure expected-move oracle has_text is not an integer")
    latitude = row["location_lat"]
    longitude = row["location_lon"]
    if any(
        coordinate is not None
        and (
            isinstance(coordinate, bool)
            or not isinstance(coordinate, (int, float))
        )
        for coordinate in (latitude, longitude)
    ):
        raise ValueError("restructure expected-move oracle coordinates are not numeric")
    date = restructure_oracle_year_month(row["modified_at"], row["created_at"])
    root = PureWindowsPath(corpus_root)
    person_name = restructure_oracle_person_name(row)
    category: str
    destination: PureWindowsPath
    if person_name is not None:
        safe_name = safe_restructure_component(person_name)
        category = f"People/{safe_name}"
        destination = root / "People" / safe_name
        if date is not None:
            destination /= str(date[0])
    elif latitude is not None and longitude is not None:
        latitude_value = float(latitude)
        longitude_value = float(longitude)
        if (
            math.isfinite(latitude_value)
            and math.isfinite(longitude_value)
            and -90.0 <= latitude_value <= 90.0
            and -180.0 <= longitude_value <= 180.0
        ):
            latitude_bucket = (
                math.copysign(
                    math.floor(abs(latitude_value * 2.0) + 0.5),
                    latitude_value,
                )
                / 2.0
            )
            longitude_bucket = (
                math.copysign(
                    math.floor(abs(longitude_value * 2.0) + 0.5),
                    longitude_value,
                )
                / 2.0
            )
            bucket = f"{latitude_bucket:.1f}_{longitude_bucket:.1f}"
            category = f"Places/{bucket}"
            destination = root / "Places" / bucket
            if date is not None:
                destination /= str(date[0])
        else:
            latitude = None
            longitude = None
    if person_name is None and (latitude is None or longitude is None):
        has_text = int(has_text_raw or 0) != 0
        year = str(date[0]) if date is not None else None
        month_names = (
            "",
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        )
        if has_text or kind in {"pdf", "doc"}:
            category = "document"
            destination = root / "Documents"
            if year is not None:
                destination /= year
        elif kind == "image":
            category = "photo"
            destination = root / "Photos"
            if date is not None:
                destination = destination / year / month_names[date[1]]
        elif kind == "video":
            category = "video"
            destination = root / "Videos"
            if year is not None:
                destination /= year
        elif kind == "audio":
            category = "audio"
            destination = root / "Audio"
            if year is not None:
                destination /= year
        elif kind == "model":
            category = "model"
            destination = root / "3D Models"
        else:
            category = "misc"
            destination = root / "Misc"
    filename = PureWindowsPath(source).name
    if not filename:
        raise ValueError(
            f"restructure expected-move oracle source does not name a file: {source}"
        )
    return {
        "fileID": int(row["id"]),
        "source": source,
        "sourceFolder": str(PureWindowsPath(source).parent),
        "destination": str(destination / filename),
        "category": category,
    }


def restructure_oracle_digest_update(
    digest: Any, row: dict[str, Any]
) -> None:
    encoded = json.dumps(
        [
            int(row["fileID"]),
            str(row["category"]),
            str(row["destination"]),
        ],
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8", "surrogatepass")
    digest.update(len(encoded).to_bytes(8, "big"))
    digest.update(encoded)


def restructure_oracle_digest(rows: Iterable[dict[str, Any]]) -> str:
    digest = hashlib.sha256()
    for row in rows:
        restructure_oracle_digest_update(digest, row)
    return digest.hexdigest()


def restructure_expected_moves_oracle(
    db_path: Path,
    corpus_root: Path,
    corpus_files: set[str],
    corpus_directories: set[str],
) -> dict[str, Any]:
    root, prefix, upper = restructure_root_bounds(str(corpus_root))
    query = """
        SELECT
            f.id,
            f.path_text,
            f.kind,
            f.modified_at,
            f.created_at,
            f.location_lat,
            f.location_lon,
            f.has_text,
            (SELECT GROUP_CONCAT(name, char(31))
               FROM (SELECT DISTINCT p.name
                       FROM persons p
                       JOIN face_prints fp ON fp.person_id = p.id
                      WHERE fp.file_id = f.id
                        AND p.name IS NOT NULL
                        AND TRIM(p.name) <> ''
                      ORDER BY p.name)) AS names,
            (SELECT COUNT(*) FROM face_prints fp
              WHERE fp.file_id = f.id) AS face_count,
            (SELECT COUNT(*) FROM face_prints fp
               JOIN persons p ON p.id = fp.person_id
              WHERE fp.file_id = f.id
                AND p.name IS NOT NULL
                AND TRIM(p.name) <> '') AS named_face_count,
            (SELECT COUNT(DISTINCT fp.person_id) FROM face_prints fp
               JOIN persons p ON p.id = fp.person_id
              WHERE fp.file_id = f.id
                AND p.name IS NOT NULL
                AND TRIM(p.name) <> '') AS named_person_count
        FROM files f
        WHERE f.failed = 0
          AND (?1 = '' OR f.path_text COLLATE NOCASE = ?1
               OR (f.path_text COLLATE NOCASE >= ?2
                   AND f.path_text COLLATE NOCASE < ?3))
        ORDER BY f.id
    """
    with connect_readonly(db_path, immutable=False) as connection:
        proposed = [
            restructure_oracle_move(row, corpus_root)
            for row in connection.execute(query, (root, prefix, upper))
        ]
    folder_categories: dict[str, collections.Counter[str]] = {}
    for row in proposed:
        folder_categories.setdefault(
            str(row["sourceFolder"]), collections.Counter()
        )[str(row["category"])] += 1
    root_key = windows_path_key(corpus_root)
    tiers: dict[str, str] = {}
    for folder, categories in folder_categories.items():
        total = sum(categories.values())
        top = max(categories.values(), default=0)
        folder_name = PureWindowsPath(folder).name.lower()
        if folder_name in RESTRUCTURE_GENERIC_FOLDERS or total <= 2:
            tier = "Junk"
        elif windows_path_key(folder) == root_key:
            tier = "Mixed"
        elif top * 100 >= total * 80:
            tier = "Anchor"
        else:
            tier = "Mixed"
        tiers[folder] = tier

    occupied = {
        windows_path_key(path) for path in corpus_files | corpus_directories
    }
    claimed: set[tuple[str, ...]] = set()
    next_suffix: dict[tuple[str, ...], int] = {}
    expected: list[dict[str, Any]] = []
    for row in proposed:
        if tiers[str(row["sourceFolder"])] == "Anchor":
            continue
        destination = str(row["destination"])
        if windows_path_key(row["source"]) == windows_path_key(destination):
            continue
        family = windows_path_key(destination)
        candidate = destination
        candidate_key = family
        if candidate_key in occupied or candidate_key in claimed:
            path = PureWindowsPath(destination)
            suffix = path.suffix
            stem = path.stem
            number = next_suffix.get(family, 2)
            while True:
                filename = (
                    f"{stem} ({number}){suffix}"
                    if suffix
                    else f"{stem} ({number})"
                )
                candidate = str(path.parent / filename)
                candidate_key = windows_path_key(candidate)
                number += 1
                if candidate_key not in occupied and candidate_key not in claimed:
                    next_suffix[family] = number
                    break
        claimed.add(candidate_key)
        expected.append(
            {
                "fileID": int(row["fileID"]),
                "category": str(row["category"]),
                "destination": candidate,
            }
        )
    return {
        "algorithm": "windows-restructure-exact-moves-v1",
        "moves": expected,
        "moveCount": len(expected),
        "digest": restructure_oracle_digest(expected),
        "categoryCounts": dict(
            collections.Counter(str(row["category"]) for row in expected)
        ),
    }


def folder_classification_oracle(
    db_path: Path, corpus_root: Path
) -> dict[str, Any]:
    root, prefix, upper = restructure_root_bounds(str(corpus_root))
    folder_categories: dict[str, collections.Counter[str]] = {}
    folder_windows_keys: dict[tuple[str, ...], set[str]] = {}
    excluded_file_ids: set[int] = set()
    query = """
        SELECT
            f.id,
            f.path_text,
            f.kind,
            f.location_lat,
            f.location_lon,
            f.has_text,
            (SELECT GROUP_CONCAT(name, char(31))
               FROM (SELECT DISTINCT p.name
                       FROM persons p
                       JOIN face_prints fp ON fp.person_id = p.id
                      WHERE fp.file_id = f.id
                        AND p.name IS NOT NULL
                        AND TRIM(p.name) <> ''
                      ORDER BY p.name)) AS names,
            (SELECT COUNT(*) FROM face_prints fp
              WHERE fp.file_id = f.id) AS face_count,
            (SELECT COUNT(*) FROM face_prints fp
               JOIN persons p ON p.id = fp.person_id
              WHERE fp.file_id = f.id
                AND p.name IS NOT NULL
                AND TRIM(p.name) <> '') AS named_face_count,
            (SELECT COUNT(DISTINCT fp.person_id) FROM face_prints fp
               JOIN persons p ON p.id = fp.person_id
              WHERE fp.file_id = f.id
                AND p.name IS NOT NULL
                AND TRIM(p.name) <> '') AS named_person_count
        FROM files f
        WHERE f.failed = 0
          AND (?1 = '' OR f.path_text COLLATE NOCASE = ?1
               OR (f.path_text COLLATE NOCASE >= ?2
                   AND f.path_text COLLATE NOCASE < ?3))
        ORDER BY f.id
    """
    with connect_readonly(db_path, immutable=False) as connection:
        excluded_file_ids.update(
            int(row[0])
            for row in connection.execute(
                "SELECT id FROM files WHERE failed IS NULL OR failed <> 0"
            )
        )
        for row in connection.execute(query, (root, prefix, upper)):
            if not isinstance(row[1], str):
                raise ValueError("restructure oracle path is not text")
            source_folder = str(PureWindowsPath(row[1]).parent)
            categories = folder_categories.setdefault(
                source_folder, collections.Counter()
            )
            categories[restructure_oracle_category(row)] += 1
            folder_windows_keys.setdefault(
                windows_path_key(source_folder), set()
            ).add(source_folder)
    root_key = windows_path_key(corpus_root)
    tiers_by_folder: dict[str, str] = {}
    expected = collections.Counter()
    for folder, categories in folder_categories.items():
        total = sum(categories.values())
        top = max(categories.values(), default=0)
        folder_name = PureWindowsPath(folder).name.lower()
        if folder_name in RESTRUCTURE_GENERIC_FOLDERS or total <= 2:
            tier = "Junk"
        elif windows_path_key(folder) == root_key:
            tier = "Mixed"
        elif top * 100 >= total * 80:
            tier = "Anchor"
        else:
            tier = "Mixed"
        tiers_by_folder[folder] = tier
        expected[tier] += 1
    windows_aliases = {
        "\\".join(key): sorted(spellings)
        for key, spellings in folder_windows_keys.items()
        if len(spellings) > 1
    }
    digest = hashlib.sha256()
    for folder, tier in sorted(tiers_by_folder.items()):
        row = json.dumps(
            [
                folder,
                tier,
                dict(sorted(folder_categories[folder].items())),
            ],
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8", "surrogatepass")
        digest.update(len(row).to_bytes(8, "big"))
        digest.update(row)
    return {
        "algorithm": FOLDER_CLASSIFICATION_ORACLE,
        "counts": {
            "Anchor": expected["Anchor"],
            "Mixed": expected["Mixed"],
            "Junk": expected["Junk"],
        },
        "folderCount": len(tiers_by_folder),
        "tiersByFolder": tiers_by_folder,
        "windowsAliases": windows_aliases,
        "excludedFileIDs": excluded_file_ids,
        "digest": digest.hexdigest(),
    }


def validate_folder_classifications(
    value: Any,
    tier_by_source_folder: dict[str, str],
    conflicting_folders: list[str],
    db_path: Path,
    corpus_root: Path,
) -> dict[str, Any]:
    keys = {"anchorFolders", "mixedFolders", "junkFolders"}
    if not isinstance(value, dict) or set(value) != keys:
        raise ValueError(
            "restructurePlan folderClassifications must contain exactly "
            "anchorFolders, mixedFolders, and junkFolders"
        )
    if any(
        isinstance(value[key], bool)
        or not isinstance(value[key], int)
        or value[key] < 0
        for key in keys
    ):
        raise ValueError(
            "restructurePlan folderClassifications values must be non-negative integers"
        )
    counts = {
        "Anchor": int(value["anchorFolders"]),
        "Mixed": int(value["mixedFolders"]),
        "Junk": int(value["junkFolders"]),
    }
    observed = collections.Counter(tier_by_source_folder.values())
    oracle_error: str | None = None
    try:
        oracle = folder_classification_oracle(db_path, corpus_root)
    except (OSError, sqlite3.Error, TypeError, ValueError) as error:
        oracle_error = f"{type(error).__name__}: {error}"
        oracle = {
            "algorithm": FOLDER_CLASSIFICATION_ORACLE,
            "counts": {},
            "folderCount": 0,
            "tiersByFolder": {},
            "windowsAliases": {},
            "excludedFileIDs": set(),
            "digest": None,
        }
    expected = oracle["counts"]
    wrong_tiers = [
        {
            "folder": folder,
            "spoolTier": tier,
            "oracleTier": oracle["tiersByFolder"].get(folder),
        }
        for folder, tier in tier_by_source_folder.items()
        if oracle["tiersByFolder"].get(folder) != tier
    ]
    unknown_source_folders = [
        folder
        for folder in tier_by_source_folder
        if folder not in oracle["tiersByFolder"]
    ]
    oracle_available = oracle_error is None and not oracle["windowsAliases"]
    checks = {
        "folderClassificationsPresent": True,
        "folderClassificationTotalPositive": sum(counts.values()) > 0,
        "folderClassificationExactOracleAvailable": oracle_available,
        "folderClassificationTotalMatchesOracle": oracle_available
        and sum(counts.values()) == int(oracle["folderCount"]),
        "folderClassificationCountsMatchExactOracle": oracle_available
        and counts == expected,
        "sourceFolderTiersConsistent": not conflicting_folders,
        "anchorRowsExcludedFromActionablePlan": observed.get("Anchor", 0) == 0,
        "spoolFolderTiersMatchExactOracle": oracle_available
        and not wrong_tiers
        and not unknown_source_folders,
        "folderOracleWindowsCanonical": not oracle["windowsAliases"],
    }
    return {
        "counts": {
            "anchorFolders": counts["Anchor"],
            "mixedFolders": counts["Mixed"],
            "junkFolders": counts["Junk"],
        },
        "observedActionableSourceFoldersByTier": dict(observed),
        "oracleAlgorithm": oracle["algorithm"],
        "oracleCounts": expected,
        "oracleFolderCount": oracle["folderCount"],
        "oracleDigest": oracle["digest"],
        "excludedFileIDs": oracle["excludedFileIDs"],
        "checks": checks,
        "violations": {
            "conflictingSourceFolders": conflicting_folders[:50],
            "wrongSourceFolderTiers": wrong_tiers[:50],
            "unknownSourceFolders": unknown_source_folders[:50],
            "windowsCanonicalFolderAliases": dict(
                list(oracle["windowsAliases"].items())[:50]
            ),
            "oracleError": oracle_error,
        },
    }


def validate_plan_spool(
    spool: Path | None,
    event_payload_value: dict[str, Any],
    corpus_root: Path,
    corpus_files: set[str],
    corpus_directories: set[str],
    db_paths_by_id: dict[int, str],
    db_path: Path,
) -> dict[str, Any]:
    allowed_event_keys = {
        "libraryRoot",
        "planID",
        "totalMoves",
        "truncated",
        "moves",
        "categoryCounts",
        "confidenceCounts",
        "folderClassifications",
    }
    required_event_keys = {
        "libraryRoot",
        "moves",
        "categoryCounts",
        "confidenceCounts",
        "folderClassifications",
    }
    if (
        not isinstance(event_payload_value, dict)
        or not required_event_keys <= set(event_payload_value)
        or not set(event_payload_value) <= allowed_event_keys
    ):
        raise ValueError("restructurePlan payload shape is invalid")
    plan_id = event_payload_value.get("planID")
    truncated = event_payload_value.get("truncated", False)
    if not isinstance(event_payload_value["libraryRoot"], str) or not isinstance(
        truncated, bool
    ):
        raise ValueError("restructurePlan payload field types are invalid")
    if spool is None:
        if plan_id is not None or truncated:
            raise ValueError("inline restructurePlan payload has paged fields")
    elif (
        not isinstance(plan_id, str)
        or not plan_id
        or "totalMoves" not in event_payload_value
        or "truncated" not in event_payload_value
    ):
        raise ValueError("paged restructurePlan payload fields are invalid")
    preview = event_payload_value["moves"]
    if not isinstance(preview, list):
        raise ValueError("restructurePlan moves preview is not an array")
    move_keys = {
        "fileID",
        "source",
        "destination",
        "category",
        "tier",
        "confidence",
        "reason",
    }
    event_total = event_payload_value.get("totalMoves")
    if event_total is not None and (
        isinstance(event_total, bool)
        or not isinstance(event_total, int)
        or event_total < 0
    ):
        raise ValueError("restructurePlan totalMoves must be a non-negative integer")
    expected_oracle = restructure_expected_moves_oracle(
        db_path,
        corpus_root,
        corpus_files,
        corpus_directories,
    )
    expected_moves = expected_oracle.pop("moves")
    expected_move_mismatches: list[dict[str, Any]] = []
    actual_oracle_digest = hashlib.sha256()
    row_count = 0
    seen_file_ids: set[int] = set()
    source_order: dict[str, int] = {}
    destination_records: dict[
        tuple[str, ...], dict[str, Any]
    ] = {}
    categories: collections.Counter[str] = collections.Counter()
    confidence: collections.Counter[str] = collections.Counter()
    tiers: collections.Counter[str] = collections.Counter()
    tier_by_source_folder: dict[str, str] = {}
    conflicting_source_folders: list[str] = []
    duplicate_file_ids: list[int] = []
    duplicate_sources: list[int] = []
    duplicate_destinations: list[dict[str, Any]] = []
    missing_file_ids: list[int] = []
    wrong_source_for_file_id: list[int] = []
    source_missing: list[int] = []
    out_of_root: list[int] = []
    no_ops: list[int] = []
    destination_path_violations: list[dict[str, Any]] = []
    empty_reasons: list[int] = []
    page_sizes: list[int] = []
    page_digests: list[str] = []
    preview_matches = True
    digests = StreamingPlanDigests()
    header: dict[str, Any]

    def add_sample(target: list[Any], value: Any) -> None:
        if len(target) < 50:
            target.append(value)

    if spool is None:
        inline_header = {
            "version": 2,
            "libraryRoot": event_payload_value["libraryRoot"],
            "totalMoves": len(preview),
        }
        plan_source = contextlib.nullcontext((inline_header, iter((preview,))))
    else:
        plan_source = read_plan_spool_pages(spool)
    with plan_source as (stored_header, page_iterator):
        header = stored_header
        for page in page_iterator:
            page_sizes.append(len(page))
            page_digests.append(plan_digest(page, ordered=True))
            for move in page:
                index = row_count
                if (
                    set(move) != move_keys
                    or isinstance(move.get("fileID"), bool)
                    or not isinstance(move.get("fileID"), int)
                    or not all(
                        isinstance(move.get(key), str)
                        for key in (
                            "source",
                            "destination",
                            "category",
                            "confidence",
                        )
                    )
                    or (
                        move.get("tier") is not None
                        and not isinstance(move.get("tier"), str)
                    )
                    or (
                        move.get("reason") is not None
                        and not isinstance(move.get("reason"), str)
                    )
                ):
                    raise ValueError(
                        f"restructure plan contains malformed move row: {index}"
                    )
                file_id = int(move["fileID"])
                raw_source = str(move["source"])
                source = normalized(raw_source)
                raw_destination = str(move["destination"])
                destination = normalized(raw_destination)
                destination_key = windows_path_key(raw_destination)
                oracle_row = {
                    "fileID": file_id,
                    "category": str(move["category"]),
                    "destination": raw_destination,
                }
                restructure_oracle_digest_update(
                    actual_oracle_digest, oracle_row
                )
                expected_row = (
                    expected_moves[index]
                    if index < len(expected_moves)
                    else None
                )
                if expected_row != oracle_row:
                    add_sample(
                        expected_move_mismatches,
                        {
                            "index": index,
                            "expected": expected_row,
                            "observed": oracle_row,
                        },
                    )
                if index < len(preview) and preview[index] != move:
                    preview_matches = False
                if file_id in seen_file_ids:
                    add_sample(duplicate_file_ids, file_id)
                seen_file_ids.add(file_id)
                if source in source_order:
                    add_sample(duplicate_sources, file_id)
                else:
                    source_order[source] = index
                if destination_key in destination_records:
                    add_sample(
                        duplicate_destinations,
                        {
                            "fileID": file_id,
                            "destination": raw_destination,
                            "collidesWithFileID": destination_records[
                                destination_key
                            ]["fileID"],
                        },
                    )
                else:
                    destination_records[destination_key] = {
                        "fileID": file_id,
                        "destination": raw_destination,
                        "normalized": destination,
                        "index": index,
                    }
                if file_id not in db_paths_by_id:
                    add_sample(missing_file_ids, file_id)
                elif db_paths_by_id[file_id] != raw_source:
                    add_sample(wrong_source_for_file_id, file_id)
                if source not in corpus_files:
                    add_sample(source_missing, file_id)
                if not under_root(raw_source, corpus_root) or not under_root(
                    raw_destination, corpus_root
                ):
                    add_sample(out_of_root, file_id)
                if source == destination:
                    add_sample(no_ops, file_id)
                violations = windows_path_violations(
                    raw_destination, corpus_root
                )
                if violations:
                    add_sample(
                        destination_path_violations,
                        {
                            "fileID": file_id,
                            "destination": raw_destination,
                            "violations": violations,
                        },
                    )
                category = str(move["category"])
                confidence_value = str(move["confidence"])
                tier = str(move.get("tier") or "")
                categories[category] += 1
                confidence[confidence_value] += 1
                tiers[tier] += 1
                source_folder = str(PureWindowsPath(raw_source).parent)
                previous_tier = tier_by_source_folder.setdefault(
                    source_folder, tier
                )
                if (
                    previous_tier != tier
                    and source_folder not in conflicting_source_folders
                ):
                    add_sample(conflicting_source_folders, source_folder)
                if not str(move.get("reason") or "").strip():
                    add_sample(empty_reasons, file_id)
                digests.add(move)
                row_count += 1
    if len(preview) > row_count:
        preview_matches = False
    if row_count < len(expected_moves):
        for index in range(row_count, min(len(expected_moves), row_count + 50)):
            add_sample(
                expected_move_mismatches,
                {
                    "index": index,
                    "expected": expected_moves[index],
                    "observed": None,
                },
            )
    total = int(event_total if event_total is not None else header["totalMoves"])
    destination_ancestor_conflicts: list[dict[str, Any]] = []
    existing_file_ancestor_conflicts: list[dict[str, Any]] = []
    for destination_key, record in destination_records.items():
        for component_count in range(1, len(destination_key)):
            ancestor = destination_records.get(
                destination_key[:component_count]
            )
            if ancestor is not None:
                add_sample(
                    destination_ancestor_conflicts,
                    {
                        "ancestorFileID": ancestor["fileID"],
                        "ancestorDestination": ancestor["destination"],
                        "descendantFileID": record["fileID"],
                        "descendantDestination": record["destination"],
                    },
                )
                break
        for parent in PureWindowsPath(record["destination"]).parents:
            normalized_parent = normalized(str(parent))
            if (
                normalized_parent in corpus_files
                and (
                    normalized_parent not in source_order
                    or source_order[normalized_parent] >= int(record["index"])
                )
            ):
                add_sample(
                    existing_file_ancestor_conflicts,
                    {
                        "fileID": record["fileID"],
                        "destination": record["destination"],
                        "blockingFile": str(parent),
                    },
                )
                break
    occupied_not_vacated = [
        int(record["fileID"])
        for record in destination_records.values()
        if (
            record["normalized"] in corpus_files
            or record["normalized"] in corpus_directories
        )
        and (
            record["normalized"] not in source_order
            or source_order[record["normalized"]] >= int(record["index"])
        )
    ][:50]
    event_categories, event_confidence_raw = plan_event_counts(event_payload_value)
    event_confidence = event_confidence_raw
    computed_confidence = {
        "auto": confidence.get("auto", 0),
        "review": confidence.get("review", 0),
        "ask": confidence.get("ask", 0),
        "unknown": sum(
            count for key, count in confidence.items() if key not in ALLOWED_CONFIDENCE
        ),
    }
    folder_classifications = validate_folder_classifications(
        event_payload_value["folderClassifications"],
        tier_by_source_folder,
        conflicting_source_folders,
        db_path,
        corpus_root,
    )
    excluded_file_ids = folder_classifications.pop("excludedFileIDs")
    ineligible_file_ids = [
        file_id
        for file_id in seen_file_ids
        if file_id in excluded_file_ids
    ][:50]
    actual_expected_digest = actual_oracle_digest.hexdigest()
    checks = {
        "spoolVersionCurrent": spool is None or int(header["version"]) == 2,
        "headerRootMatchesRequest": normalized(str(header["libraryRoot"]))
        == normalized(corpus_root),
        "eventRootMatchesRequest": normalized(str(event_payload_value["libraryRoot"]))
        == normalized(corpus_root),
        "planIDMatchesSpool": (spool is None and plan_id is None)
        or (spool is not None and isinstance(plan_id, str) and plan_id == spool.stem),
        "headerTotalMatchesRows": int(header["totalMoves"]) == row_count,
        "eventTotalMatchesRows": total == row_count,
        "expectedMoveCountMatches": row_count
        == int(expected_oracle["moveCount"]),
        "expectedMoveDigestMatches": actual_expected_digest
        == expected_oracle["digest"],
        "expectedMoveRowsExact": not expected_move_mismatches
        and row_count == len(expected_moves),
        "planNonempty": row_count > 0,
        "previewBounded": len(preview) <= RESTRUCTURE_PREVIEW_CAP,
        "previewMatchesSpoolPrefix": preview_matches,
        "truncatedTruthful": bool(event_payload_value.get("truncated", False))
        == (row_count > len(preview)),
        "pagedRowsCoverPlan": sum(page_sizes) == row_count,
        "pagesBounded": all(
            0 < page_size <= RESTRUCTURE_PAGE_SIZE
            for page_size in page_sizes
        ),
        "nonFinalPagesFull": all(
            page_size == RESTRUCTURE_PAGE_SIZE
            for page_size in page_sizes[:-1]
        ),
        "moveShapesValid": True,
        "uniqueFileIDs": not duplicate_file_ids,
        "uniqueSources": not duplicate_sources,
        "uniqueDestinations": not duplicate_destinations,
        "allFileIDsExist": not missing_file_ids,
        "allFileIDsEligible": not ineligible_file_ids,
        "fileIDSourcesMatchDBExactly": not wrong_source_for_file_id,
        "allSourcesExist": not source_missing,
        "allPathsInsideRoot": not out_of_root,
        "noCaseInsensitiveNoOps": not no_ops,
        "destinationsCollisionSafe": not occupied_not_vacated,
        "destinationsHaveNoAncestorConflicts": not destination_ancestor_conflicts,
        "destinationParentsContainNoUnvacatedFiles": not (
            existing_file_ancestor_conflicts
        ),
        "destinationsWindowsLegal": not destination_path_violations,
        "categoriesNonempty": all(key.strip() for key in categories),
        "reasonsNonempty": not empty_reasons,
        "confidenceAllowed": all(key in ALLOWED_CONFIDENCE for key in confidence),
        "tiersAllowed": all(key in ALLOWED_TIERS for key in tiers),
        "categoryCountSum": sum(categories.values()) == total,
        "confidenceCountSum": sum(confidence.values()) == total,
        "eventCategoryCountsMatch": dict(categories) == event_categories,
        "eventConfidenceCountsMatch": computed_confidence == event_confidence,
        **folder_classifications["checks"],
    }
    return {
        "storageMode": "inline" if spool is None else "paged",
        "spool": str(spool) if spool is not None else None,
        "header": header,
        "totalMoves": total,
        "previewMoves": len(preview),
        "truncated": bool(event_payload_value.get("truncated", False)),
        "planID": plan_id,
        "pageSize": RESTRUCTURE_PAGE_SIZE,
        "pageCount": len(page_sizes),
        "pageDigests": page_digests,
        "categoryCounts": dict(categories),
        "confidenceCounts": dict(confidence),
        "tierCounts": dict(tiers),
        "folderClassifications": folder_classifications["counts"],
        "folderClassificationEvidence": folder_classifications,
        "folderClassificationOracleDigest": folder_classifications[
            "oracleDigest"
        ],
        "expectedMoveOracle": expected_oracle,
        "observedExpectedMoveDigest": actual_expected_digest,
        "orderedDigest": digests.ordered(),
        "canonicalDigest": digests.canonical(),
        "canonicalDigestAlgorithm": "sha256-multiset-v1",
        "checks": checks,
        "violations": {
            "malformedRowIndexes": [],
            "duplicateFileIDs": duplicate_file_ids,
            "duplicateSourceIDs": duplicate_sources,
            "duplicateDestinations": duplicate_destinations,
            "missingFileIDs": missing_file_ids[:50],
            "ineligibleFileIDs": ineligible_file_ids,
            "wrongSourceForFileIDs": wrong_source_for_file_id[:50],
            "missingSourceIDs": source_missing[:50],
            "outOfRootIDs": out_of_root[:50],
            "noOpIDs": no_ops[:50],
            "occupiedDestinationIDs": occupied_not_vacated[:50],
            "destinationAncestorConflicts": destination_ancestor_conflicts,
            "existingFileAncestorConflicts": existing_file_ancestor_conflicts,
            "illegalDestinations": destination_path_violations[:50],
            "emptyReasonIDs": empty_reasons,
            "expectedMoveMismatches": expected_move_mismatches,
        },
    }


DEEP_SELECTION_TARGETS = (
    ("imageWithFaces", "image", "has_faces=1"),
    (
        "heicWithoutFaces",
        "image",
        "has_faces=0 AND LOWER(extension) IN ('heic','heif')",
    ),
    ("video", "video", "1=1"),
    ("pdf", "pdf", "1=1"),
    ("document", "doc", "1=1"),
    ("audio", "audio", "1=1"),
    ("modelObj", "model", "LOWER(extension)='obj'"),
)


def required_deep_labels(
    model_kind: str, limit: int, available_labels: tuple[str, ...]
) -> tuple[str, ...]:
    canonical_model = VLM_CANONICAL_IDS.get(model_kind)
    if canonical_model not in VLM_ALIASES or limit <= 0:
        return ()
    return tuple(
        label
        for label, _kind, _extra in DEEP_SELECTION_TARGETS
        if label in available_labels
    )[:limit]


def available_deep_labels(
    db_path: Path, corpus_files: set[str]
) -> tuple[str, ...]:
    available: list[str] = []
    with connect_readonly(db_path) as connection:
        for label, kind, extra in DEEP_SELECTION_TARGETS:
            rows = connection.execute(
                f"SELECT path_text FROM files WHERE failed=0 "
                f"AND vlm_model IS NULL AND vlm_full_model IS NULL "
                f"AND vlm_description IS NULL AND vlm_proposed_name IS NULL "
                f"AND vlm_analyzed_at IS NULL AND size_bytes>0 "
                f"AND kind=? AND {extra} ORDER BY id",
                (kind,),
            )
            if any(normalized(row["path_text"]) in corpus_files for row in rows):
                available.append(label)
    return tuple(available)


def deep_selection_label(item: dict[str, Any]) -> str | None:
    kind = str(item.get("kind") or "").casefold()
    extension = str(item.get("extension") or "").casefold().lstrip(".")
    has_faces = item.get("hasFaces")
    if kind == "image" and has_faces is True:
        return "imageWithFaces"
    if (
        kind == "image"
        and has_faces is False
        and extension in {"heic", "heif"}
    ):
        return "heicWithoutFaces"
    if kind == "doc":
        return "document"
    if kind in {"image", "video", "pdf", "audio"}:
        return kind
    if kind == "model" and extension == "obj":
        return "modelObj"
    return None


def deep_selection_matches_label(item: dict[str, Any]) -> bool:
    label = str(item.get("label") or "")
    inferred = deep_selection_label(item)
    if label == "document":
        return str(item.get("kind") or "").casefold() == "doc" and inferred == label
    if label in {"image", "video", "pdf", "audio"}:
        return label == str(item.get("kind") or "").casefold() and inferred is not None
    return label == inferred


DEEP_GENERIC_OUTPUT_TOKENS = frozenset(
    {
        "a",
        "an",
        "and",
        "background",
        "content",
        "copy",
        "dcim",
        "dsc",
        "document",
        "edited",
        "file",
        "final",
        "generic",
        "human",
        "image",
        "img",
        "item",
        "landscape",
        "media",
        "misc",
        "model",
        "mov",
        "new",
        "object",
        "of",
        "old",
        "person",
        "photo",
        "picture",
        "portrait",
        "pxl",
        "scan",
        "scene",
        "screenshot",
        "someone",
        "something",
        "square",
        "stuff",
        "text",
        "the",
        "thing",
        "this",
        "tall",
        "unknown",
        "untitled",
        "vid",
        "video",
        "view",
        "wide",
        "year",
    }
)


def deep_word_tokens(value: Any) -> tuple[str, ...]:
    raw = re.findall(r"[^\W_]+", str(value or "").casefold(), flags=re.UNICODE)
    result: list[str] = []
    for token in raw:
        if token.isdecimal():
            continue
        if len(token) > 5 and token.endswith("ies"):
            token = f"{token[:-3]}y"
        elif len(token) > 4 and token.endswith("s") and not token.endswith("ss"):
            token = token[:-1]
        if len(token) >= 2:
            result.append(token)
    return tuple(result)


def deep_specific_tokens(value: Any) -> frozenset[str]:
    return frozenset(
        token
        for token in deep_word_tokens(value)
        if token not in DEEP_GENERIC_OUTPUT_TOKENS
    )


def deep_normalized_text(value: Any) -> str:
    return " ".join(deep_word_tokens(value))


def deep_semantic_output_quality(
    outputs: list[dict[str, Any]],
) -> dict[str, Any]:
    generic_description_ids: list[int] = []
    generic_name_ids: list[int] = []
    generic_tag_ids: list[int] = []
    description_groups: dict[str, list[int]] = collections.defaultdict(list)
    name_groups: dict[str, list[int]] = collections.defaultdict(list)
    signature_groups: dict[tuple[str, str, tuple[str, ...]], list[int]] = (
        collections.defaultdict(list)
    )
    for output in outputs:
        file_id = int(output.get("fileID", -1))
        description = str(output.get("description") or "")
        proposed_name = str(output.get("proposedName") or "")
        tags = [
            str(tag)
            for tag in output.get("tags", [])
            if isinstance(tag, str)
        ]
        if not deep_specific_tokens(description):
            generic_description_ids.append(file_id)
        if not deep_specific_tokens(PureWindowsPath(proposed_name).stem):
            generic_name_ids.append(file_id)
        if tags and any(not deep_specific_tokens(tag) for tag in tags):
            generic_tag_ids.append(file_id)
        normalized_description = deep_normalized_text(description)
        normalized_name = deep_normalized_text(
            PureWindowsPath(proposed_name).stem
        )
        normalized_tags = tuple(
            sorted(
                normalized
                for tag in tags
                if (normalized := deep_normalized_text(tag))
            )
        )
        description_groups[normalized_description].append(file_id)
        name_groups[normalized_name].append(file_id)
        signature_groups[
            (normalized_description, normalized_name, normalized_tags)
        ].append(file_id)

    duplicate_descriptions = [
        {"value": value, "fileIDs": file_ids}
        for value, file_ids in description_groups.items()
        if value and len(file_ids) > 1
    ]
    duplicate_names = [
        {"value": value, "fileIDs": file_ids}
        for value, file_ids in name_groups.items()
        if value and len(file_ids) > 1
    ]
    duplicate_signatures = [
        {
            "description": signature[0],
            "proposedName": signature[1],
            "tags": list(signature[2]),
            "fileIDs": file_ids,
        }
        for signature, file_ids in signature_groups.items()
        if any(signature) and len(file_ids) > 1
    ]
    return {
        "genericDescriptionFileIDs": generic_description_ids[:50],
        "genericProposedNameFileIDs": generic_name_ids[:50],
        "genericTagFileIDs": generic_tag_ids[:50],
        "duplicateDescriptions": duplicate_descriptions[:50],
        "descriptionsExactlyDistinct": not duplicate_descriptions,
        "duplicateProposedNames": duplicate_names[:50],
        "duplicateSemanticSignatures": duplicate_signatures[:50],
        "checks": {
            "descriptionsContainSpecificContent": bool(outputs)
            and not generic_description_ids,
            "proposedNamesContainSpecificContent": bool(outputs)
            and not generic_name_ids,
            "tagsContainSpecificContent": not generic_tag_ids,
            "proposedNamesDistinctAcrossSelection": not duplicate_names,
            "semanticSignaturesDistinctAcrossSelection": not duplicate_signatures,
        },
    }


def deep_gold_content_oracle(
    connection: sqlite3.Connection,
    selected: list[dict[str, Any]],
    outputs_by_id: dict[int, dict[str, Any]],
) -> dict[str, Any]:
    selected_ids = [int(item["fileID"]) for item in selected]
    terms: dict[int, set[str]] = {
        file_id: set() for file_id in selected_ids
    }
    sources: dict[int, set[str]] = {
        file_id: set() for file_id in selected_ids
    }
    strengths: dict[int, int] = {
        file_id: 0 for file_id in selected_ids
    }
    if selected_ids:
        placeholders = ",".join("?" for _ in selected_ids)
        for row in connection.execute(
            f"SELECT file_id,tag,source FROM tags "
            f"WHERE file_id IN ({placeholders}) "
            "AND source IN ('user','auto') ORDER BY file_id,source,tag",
            selected_ids,
        ):
            file_id = int(row["file_id"])
            tokens = deep_specific_tokens(row["tag"])
            if not tokens:
                continue
            terms[file_id].update(tokens)
            source = str(row["source"])
            sources[file_id].add(f"{source}Tag")
            strengths[file_id] = max(
                strengths[file_id], 3 if source == "user" else 2
            )
        try:
            name_rows = connection.execute(
                f"SELECT DISTINCT fp.file_id,{PERSON_DISPLAY_NAME_SQL} AS personName "
                "FROM face_prints fp "
                "JOIN persons p ON p.id=fp.person_id "
                f"WHERE fp.file_id IN ({placeholders}) "
                f"AND {PERSON_DISPLAY_NAME_SQL}<>'' "
                "ORDER BY fp.file_id,personName",
                selected_ids,
            )
            for row in name_rows:
                file_id = int(row["file_id"])
                tokens = deep_specific_tokens(row["personName"])
                if not tokens:
                    continue
                terms[file_id].update(tokens)
                sources[file_id].add("namedFace")
                strengths[file_id] = max(strengths[file_id], 3)
        except sqlite3.Error:
            pass

    for item in selected:
        file_id = int(item["fileID"])
        path_tokens = frozenset(
            token
            for token in deep_specific_tokens(
                PureWindowsPath(str(item["path"])).stem
            )
            if token.isalpha()
        )
        if path_tokens:
            terms[file_id].update(path_tokens)
            sources[file_id].add("filename")
            strengths[file_id] = max(strengths[file_id], 1)

    ranked = sorted(
        (
            (
                -strengths[file_id],
                -len(terms[file_id]),
                selected_ids.index(file_id),
                file_id,
            )
            for file_id in selected_ids
            if terms[file_id]
        )
    )
    target_count = min(2, len(selected_ids))
    gold_ids = [item[3] for item in ranked[:target_count]]
    cases: list[dict[str, Any]] = []
    matched = 0
    for file_id in gold_ids:
        output = outputs_by_id.get(file_id, {})
        output_terms = set()
        output_terms.update(deep_specific_tokens(output.get("description")))
        output_terms.update(deep_specific_tokens(output.get("proposedName")))
        for tag in output.get("tags", []):
            output_terms.update(deep_specific_tokens(tag))
        overlap = sorted(terms[file_id] & output_terms)
        if overlap:
            matched += 1
        cases.append(
            {
                "fileID": file_id,
                "sources": sorted(sources[file_id]),
                "expectedTerms": sorted(terms[file_id])[:50],
                "observedSpecificTerms": sorted(output_terms)[:50],
                "matchedTerms": overlap[:50],
            }
        )
    return {
        "targetCount": target_count,
        "coveredCount": len(gold_ids),
        "matchedCount": matched,
        "cases": cases,
        "checks": {
            "goldOracleCoverageAvailable": target_count > 0
            and len(gold_ids) == target_count,
            "goldOracleTermsMatched": target_count > 0
            and matched == target_count,
        },
    }


def select_deep_files(
    db_path: Path,
    corpus_files: set[str],
    limit: int,
    exclude_ids: set[int] | None = None,
) -> list[dict[str, Any]]:
    if limit <= 0:
        return []
    selected: list[dict[str, Any]] = []
    seen: set[int] = set(exclude_ids or ())
    with connect_readonly(db_path) as connection:
        for label, kind, extra in DEEP_SELECTION_TARGETS:
            rows = connection.execute(
                f"SELECT id,path_text,kind,extension,size_bytes,has_faces "
                f"FROM files WHERE failed=0 AND vlm_model IS NULL "
                f"AND vlm_full_model IS NULL "
                f"AND vlm_description IS NULL AND vlm_proposed_name IS NULL "
                f"AND vlm_analyzed_at IS NULL "
                f"AND NOT EXISTS (SELECT 1 FROM tags t "
                f"WHERE t.file_id=files.id AND t.source='vlm') "
                f"AND kind=? AND size_bytes>0 AND {extra} "
                "ORDER BY CASE WHEN "
                "EXISTS (SELECT 1 FROM tags evidence "
                "WHERE evidence.file_id=files.id "
                "AND evidence.source IN ('user','auto')) "
                "OR EXISTS (SELECT 1 FROM face_prints fp "
                "JOIN persons p ON p.id=fp.person_id "
                "WHERE fp.file_id=files.id "
                f"AND {PERSON_DISPLAY_NAME_SQL}<>'') "
                "THEN 0 ELSE 1 END,id",
                (kind,),
            )
            for row in rows:
                if normalized(row["path_text"]) not in corpus_files:
                    continue
                file_id = int(row["id"])
                if file_id in seen:
                    continue
                selected.append(
                    {
                        "label": label,
                        "fileID": file_id,
                        "path": row["path_text"],
                        "kind": row["kind"],
                        "extension": row["extension"],
                        "sizeBytes": int(row["size_bytes"]),
                        "hasFaces": bool(row["has_faces"]),
                    }
                )
                seen.add(file_id)
                break
            if len(selected) >= limit:
                return selected[:limit]

        if len(selected) < limit:
            rows = connection.execute(
                "SELECT id,path_text,kind,extension,size_bytes,has_faces "
                "FROM files WHERE failed=0 AND vlm_model IS NULL "
                "AND vlm_full_model IS NULL "
                "AND vlm_description IS NULL AND vlm_proposed_name IS NULL "
                "AND vlm_analyzed_at IS NULL "
                "AND NOT EXISTS (SELECT 1 FROM tags t "
                "WHERE t.file_id=files.id AND t.source='vlm') "
                "AND size_bytes>0 "
                "AND (kind IN ('image','video','pdf','doc','audio') "
                "OR (kind='model' AND LOWER(extension)='obj')) "
                "ORDER BY CASE WHEN "
                "EXISTS (SELECT 1 FROM tags evidence "
                "WHERE evidence.file_id=files.id "
                "AND evidence.source IN ('user','auto')) "
                "OR EXISTS (SELECT 1 FROM face_prints fp "
                "JOIN persons p ON p.id=fp.person_id "
                "WHERE fp.file_id=files.id "
                f"AND {PERSON_DISPLAY_NAME_SQL}<>'') "
                "THEN 0 ELSE 1 END,id"
            )
            for row in rows:
                file_id = int(row["id"])
                if file_id in seen or normalized(row["path_text"]) not in corpus_files:
                    continue
                item = {
                    "fileID": file_id,
                    "path": row["path_text"],
                    "kind": row["kind"],
                    "extension": row["extension"],
                    "sizeBytes": int(row["size_bytes"]),
                    "hasFaces": bool(row["has_faces"]),
                }
                label = deep_selection_label(item)
                if label is None:
                    continue
                selected.append(
                    {
                        "label": label,
                        **item,
                    }
                )
                seen.add(file_id)
                if len(selected) >= limit:
                    break
    return selected


def select_unsupported_stl(
    db_path: Path, corpus_files: set[str], exclude_ids: set[int]
) -> dict[str, Any]:
    with connect_readonly(db_path) as connection:
        rows = connection.execute(
            "SELECT id,path_text,kind,extension,size_bytes,has_faces "
            "FROM files WHERE failed=0 AND kind='model' "
            "AND LOWER(extension)='stl' AND size_bytes>0 ORDER BY id"
        )
        for row in rows:
            file_id = int(row["id"])
            if file_id in exclude_ids or normalized(row["path_text"]) not in corpus_files:
                continue
            return {
                "label": "unsupportedStl",
                "fileID": file_id,
                "path": row["path_text"],
                "kind": row["kind"],
                "extension": row["extension"],
                "sizeBytes": int(row["size_bytes"]),
                "hasFaces": bool(row["has_faces"]),
            }
    return {
        "label": "unsupportedStl",
        "fileID": -1,
        "path": "",
        "kind": "model",
        "extension": "stl",
        "sizeBytes": 0,
        "hasFaces": False,
        "syntheticMissingIDFallback": True,
    }


def deep_event_metrics(
    events: list[RecordedEvent],
    selected: list[dict[str, Any]],
    complete: dict[str, Any],
    command_elapsed: float,
    model_kind: str,
    configured_limit: int,
    required_labels: tuple[str, ...],
) -> dict[str, Any]:
    kinds = [event_kind(item.value) for item in events]
    starting = [
        inner_payload(item.value, "deepAnalyzeStarting")
        for item in events
        if event_kind(item.value) == "deepAnalyzeStarting"
    ]
    progress = [
        inner_payload(item.value, "deepAnalyzeProgress")
        for item in events
        if event_kind(item.value) == "deepAnalyzeProgress"
    ]
    done = [
        inner_payload(item.value, "deepAnalyzeFileDone")
        for item in events
        if event_kind(item.value) == "deepAnalyzeFileDone"
    ]
    errors = [
        inner_payload(item.value, "error")
        for item in events
        if event_kind(item.value) == "error"
    ]
    processed_values = [int(item.get("processed", 0)) for item in progress]
    selected_ids = {int(item["fileID"]) for item in selected}
    done_ids = [int(item.get("fileID", -1)) for item in done]
    selected_paths = {normalized(str(item["path"])) for item in selected}
    selected_labels = [str(item.get("label") or "") for item in selected]
    canonical_model = VLM_CANONICAL_IDS.get(model_kind)
    complete_event = next(
        (
            item
            for item in events
            if event_kind(item.value) == "deepAnalyzeComplete"
        ),
        None,
    )
    observed_wall_seconds = (
        max(0.0, complete_event.elapsed - command_elapsed)
        if complete_event is not None
        else math.inf
    )
    reported_seconds = float(complete.get("totalSeconds", math.nan))
    semantic_quality = deep_semantic_output_quality(
        [
            {
                "fileID": item.get("fileID", -1),
                "description": item.get("description"),
                "proposedName": item.get("proposedName"),
                "tags": [],
            }
            for item in done
        ]
    )

    def first_latency(kind: str) -> float | None:
        for item in events:
            if event_kind(item.value) == kind:
                return max(0.0, item.elapsed - command_elapsed)
        return None

    return {
        "complete": complete,
        "starting": starting,
        "progressEvents": len(progress),
        "fileDone": done,
        "errors": errors,
        "timeToStartingSeconds": first_latency("deepAnalyzeStarting"),
        "timeToFirstProgressSeconds": first_latency("deepAnalyzeProgress"),
        "timeToFirstFileDoneSeconds": first_latency("deepAnalyzeFileDone"),
        "configuredLimit": configured_limit,
        "selectedLabels": selected_labels,
        "requiredLabels": list(required_labels),
        "semanticQuality": semantic_quality,
        "filesPerSecond": (
            float(complete.get("processed", 0)) / float(complete.get("totalSeconds", 0))
            if float(complete.get("totalSeconds", 0)) > 0
            else 0.0
        ),
        "checks": {
            "knownCanonicalModel": canonical_model == model_kind
            and canonical_model in VLM_ALIASES,
            "configuredSelectionLimitHonored": len(selected) == configured_limit,
            "selectedMediaKindsValid": all(
                deep_selection_matches_label(item) for item in selected
            ),
            "startingLifecyclePresent": bool(starting),
            "startingShapesValid": bool(starting)
            and all(set(item) == {"modelKind", "phase", "message"} for item in starting),
            "startingModelMatches": bool(starting)
            and all(item.get("modelKind") == model_kind for item in starting),
            "startingPhasesValid": bool(starting)
            and all(
                item.get("phase") in {"queued", "loadingModel", "resolvingTargets"}
                and bool(str(item.get("message", "")).strip())
                for item in starting
            ),
            "startingBeforeTerminal": not starting
            or kinds.index("deepAnalyzeStarting") < kinds.index("deepAnalyzeComplete"),
            "completeModelMatches": complete.get("modelKind") == model_kind,
            "completeShapeValid": set(complete)
            == {"processed", "failed", "totalSeconds", "modelKind", "cancelled"},
            "reportedDurationFinite": math.isfinite(reported_seconds)
            and reported_seconds >= 0,
            "reportedDurationWithinObservedWall": math.isfinite(reported_seconds)
            and reported_seconds <= observed_wall_seconds + 5,
            "progressPresent": bool(progress),
            "progressShapesValid": bool(progress)
            and all(
                {"processed", "total", "modelKind"} <= set(item)
                and set(item)
                <= {
                    "processed",
                    "total",
                    "etaSeconds",
                    "currentPath",
                    "currentCaption",
                    "modelKind",
                }
                for item in progress
            ),
            "progressModelMatches": bool(progress)
            and all(item.get("modelKind") == model_kind for item in progress),
            "progressTotalsMatchSelection": bool(progress)
            and all(int(item.get("total", -1)) == len(selected) for item in progress),
            "progressMonotonic": processed_values == sorted(processed_values),
            "progressBoundsValid": all(
                0 <= int(item.get("processed", -1)) <= len(selected)
                for item in progress
            ),
            "progressPathsSelected": all(
                item.get("currentPath") is None
                or normalized(str(item.get("currentPath"))) in selected_paths
                for item in progress
            ),
            "processedPlusFailedEqualsSelected": int(complete.get("processed", -1))
            + int(complete.get("failed", -1))
            == len(selected),
            "zeroFailures": int(complete.get("failed", -1)) == 0,
            "notCancelled": not bool(complete.get("cancelled", True)),
            "oneDonePerProcessed": len(done_ids)
            == int(complete.get("processed", -1)),
            "doneShapesValid": all(
                {"fileID", "description", "modelKind"} <= set(item)
                and set(item)
                <= {
                    "fileID",
                    "description",
                    "proposedName",
                    "modelKind",
                }
                for item in done
            ),
            "doneIDsUnique": len(done_ids) == len(set(done_ids)),
            "doneIDsSelected": set(done_ids).issubset(selected_ids),
            "doneIDsExactlySelected": set(done_ids) == selected_ids,
            "doneModelsMatch": all(
                item.get("modelKind") == model_kind for item in done
            ),
            "doneDescriptionsNonempty": all(
                bool(str(item.get("description", "")).strip()) for item in done
            ),
            "doneProposedNamesNonempty": all(
                bool(str(item.get("proposedName", "")).strip()) for item in done
            ),
            **semantic_quality["checks"],
            "noErrorEvents": not errors,
            "requiredKindsSelected": set(required_labels).issubset(selected_labels),
        },
    }


def run_deep_command(
    driver: EngineDriver,
    *,
    command_id: str,
    wire_model_kind: str,
    expected_model_kind: str,
    file_ids: list[int],
    skip_existing: bool,
    tags_only: bool,
    propose_renames: bool,
    timeout_seconds: float,
) -> dict[str, Any]:
    started = time.monotonic()
    driver.send(
        command_id,
        {
            "deepAnalyzeAll": {
                "modelKind": wire_model_kind,
                "skipExisting": skip_existing,
                "fileIDs": file_ids,
                "tagsOnly": tags_only,
                "proposeRenames": propose_renames,
            }
        },
    )
    event = driver.wait_for(
        "deepAnalyzeComplete",
        command_id=command_id,
        timeout_seconds=timeout_seconds,
        predicate=lambda value: (
            isinstance(value, dict)
            and value.get("modelKind") == expected_model_kind
            and not bool(value.get("cancelled", True))
        ),
    )
    fence = settle_command(driver, command_id, {"deepAnalyzeComplete": 1})
    end = int(fence["eventWindowEnd"])
    events = driver.events_between(driver.command_mark(command_id), end)
    return {
        "wireModelKind": wire_model_kind,
        "expectedModelKind": expected_model_kind,
        "complete": inner_payload(event.value, "deepAnalyzeComplete"),
        "events": events,
        "commandFence": fence,
        "wallSeconds": time.monotonic() - started,
    }


def deep_db_metrics(
    db_path: Path, selected: list[dict[str, Any]], model_kind: str
) -> dict[str, Any]:
    ids = [int(item["fileID"]) for item in selected]
    if not ids:
        return {
            "rows": [],
            "perFile": [],
            "vlmTagCount": 0,
            "vlmTagsByFile": {},
            "checks": {},
        }
    placeholders = ",".join("?" for _ in ids)
    with connect_readonly(db_path) as connection:
        rows = [
            dict(row)
            for row in connection.execute(
                f"SELECT id,path_text,kind,extension,has_faces,"
                f"vlm_description,vlm_proposed_name,"
                f"vlm_model,vlm_full_model,vlm_analyzed_at "
                f"FROM files WHERE id IN ({placeholders}) "
                "ORDER BY id",
                ids,
            )
        ]
        tag_rows = connection.execute(
            f"SELECT file_id,tag FROM tags WHERE source='vlm' "
            f"AND file_id IN ({placeholders}) ORDER BY file_id,tag",
            ids,
        ).fetchall()
    rows_by_id = {int(row["id"]): row for row in rows}
    tags_by_id: dict[int, list[str]] = collections.defaultdict(list)
    for row in tag_rows:
        tags_by_id[int(row["file_id"])].append(str(row["tag"]))
    expected_marker = model_kind
    per_file: list[dict[str, Any]] = []
    for item in selected:
        file_id = int(item["fileID"])
        row = rows_by_id.get(file_id)
        database_selection = (
            {
                "label": item.get("label"),
                "kind": row["kind"],
                "extension": row["extension"],
                "hasFaces": bool(row["has_faces"]),
            }
            if row is not None
            else {}
        )
        checks = {
            "rowPersisted": row is not None,
            "pathMatchesSelection": row is not None
            and normalized(str(row["path_text"])) == normalized(str(item["path"])),
            "kindMatchesSelection": row is not None
            and str(row["kind"]).casefold()
            == str(item.get("kind") or "").casefold(),
            "extensionMatchesSelection": row is not None
            and str(row["extension"] or "").casefold().lstrip(".")
            == str(item.get("extension") or "").casefold().lstrip("."),
            "labelMatchesDatabaseMedia": row is not None
            and deep_selection_matches_label(database_selection),
            "rawModelPersisted": row is not None
            and row["vlm_model"] == expected_marker,
            "fullModelPersisted": row is not None
            and row["vlm_full_model"] == expected_marker,
            "analyzedAtPersisted": row is not None
            and row["vlm_analyzed_at"] is not None,
            "descriptionPersisted": row is not None
            and bool(str(row["vlm_description"] or "").strip()),
            "proposedNamePersisted": row is not None
            and bool(str(row["vlm_proposed_name"] or "").strip()),
            "vlmTagsPersisted": bool(tags_by_id.get(file_id)),
        }
        per_file.append(
            {
                "fileID": file_id,
                "label": item.get("label"),
                "kind": item.get("kind"),
                "extension": item.get("extension"),
                "vlmTagCount": len(tags_by_id.get(file_id, [])),
                "vlmTags": tags_by_id.get(file_id, []),
                "row": row,
                "checks": checks,
            }
        )

    def every(check: str) -> bool:
        return bool(per_file) and all(
            file_result["checks"][check] is True for file_result in per_file
        )

    outputs_by_id = {
        file_id: {
            "fileID": file_id,
            "description": row["vlm_description"],
            "proposedName": row["vlm_proposed_name"],
            "tags": tags_by_id.get(file_id, []),
        }
        for file_id, row in rows_by_id.items()
    }
    semantic_quality = deep_semantic_output_quality(
        [outputs_by_id[file_id] for file_id in ids if file_id in outputs_by_id]
    )
    with connect_readonly(db_path) as connection:
        gold_oracle = deep_gold_content_oracle(
            connection, selected, outputs_by_id
        )
        existing_paths = [
            (int(row["id"]), str(row["path_text"]))
            for row in connection.execute(
                "SELECT id,path_text FROM files ORDER BY id"
            )
        ]
    existing_by_key: dict[tuple[str, ...], list[int]] = (
        collections.defaultdict(list)
    )
    for existing_id, existing_path in existing_paths:
        existing_by_key[windows_path_key(existing_path)].append(existing_id)
    proposed_name_collisions: list[dict[str, Any]] = []
    for item in selected:
        file_id = int(item["fileID"])
        row = rows_by_id.get(file_id)
        if row is None:
            continue
        proposed_name = str(row["vlm_proposed_name"] or "").strip()
        if not proposed_name:
            continue
        extension = str(row["extension"] or "").strip().lstrip(".")
        candidate_name = PureWindowsPath(proposed_name).stem
        if extension:
            candidate_name = f"{candidate_name}.{extension}"
        candidate = str(
            PureWindowsPath(str(row["path_text"])).parent / candidate_name
        )
        colliding_ids = [
            existing_id
            for existing_id in existing_by_key.get(
                windows_path_key(candidate), []
            )
            if existing_id != file_id
        ]
        if colliding_ids:
            proposed_name_collisions.append(
                {
                    "fileID": file_id,
                    "candidate": candidate,
                    "collidingFileIDs": colliding_ids[:50],
                }
            )

    return {
        "rows": rows,
        "perFile": per_file,
        "vlmTagCount": sum(len(tags) for tags in tags_by_id.values()),
        "vlmTagsByFile": {
            str(file_id): len(tags_by_id.get(file_id, []))
            for file_id in ids
        },
        "vlmTagValuesByFile": {
            str(file_id): tags_by_id.get(file_id, []) for file_id in ids
        },
        "expectedModelMarker": expected_marker,
        "semanticQuality": semantic_quality,
        "goldContentOracle": gold_oracle,
        "proposedNameCollisions": proposed_name_collisions,
        "checks": {
            "selectedFileIDsUnique": len(ids) == len(set(ids)),
            "allRowsPersisted": len(rows_by_id) == len(ids),
            "selectionMatchesDatabase": every("pathMatchesSelection")
            and every("kindMatchesSelection")
            and every("extensionMatchesSelection")
            and every("labelMatchesDatabaseMedia"),
            "rawModelPersistedForEveryFile": every("rawModelPersisted"),
            "fullModelPersistedForEveryFile": every("fullModelPersisted"),
            "analyzedAtPersistedForEveryFile": every("analyzedAtPersisted"),
            "descriptionsPersistedForEveryFile": every("descriptionPersisted"),
            "proposedNamesPersistedForEveryFile": every("proposedNamePersisted"),
            "vlmTagsPersistedForEveryFile": every("vlmTagsPersisted"),
            **semantic_quality["checks"],
            **gold_oracle["checks"],
            "proposedNamesDoNotCollideWithExistingSiblings": not (
                proposed_name_collisions
            ),
        },
    }


def deep_db_snapshot(db_path: Path, file_ids: list[int]) -> dict[str, Any]:
    if not file_ids:
        return {"rows": [], "vlmTagsByFile": {}}
    placeholders = ",".join("?" for _ in file_ids)
    with connect_readonly(db_path) as connection:
        rows = [
            dict(row)
            for row in connection.execute(
                f"SELECT id,vlm_description,vlm_proposed_name,vlm_model,"
                f"vlm_full_model,vlm_analyzed_at "
                f"FROM files WHERE id IN ({placeholders}) ORDER BY id",
                file_ids,
            )
        ]
        tag_rows = connection.execute(
            f"SELECT file_id,COUNT(*) AS count FROM tags WHERE source='vlm' "
            f"AND file_id IN ({placeholders}) GROUP BY file_id ORDER BY file_id",
            file_ids,
        ).fetchall()
    return {
        "rows": rows,
        "vlmTagsByFile": {str(row["file_id"]): int(row["count"]) for row in tag_rows},
    }


def deep_snapshot_is_unprocessed(
    snapshot: dict[str, Any], file_ids: list[int]
) -> bool:
    expected_ids = set(file_ids)
    rows_by_id = {int(row["id"]): row for row in snapshot["rows"]}
    return set(rows_by_id) == expected_ids and all(
        row.get("vlm_description") is None
        and row.get("vlm_proposed_name") is None
        and row.get("vlm_model") is None
        and row.get("vlm_full_model") is None
        and row.get("vlm_analyzed_at") is None
        and int(snapshot["vlmTagsByFile"].get(str(file_id), 0)) == 0
        for file_id, row in rows_by_id.items()
    )


def deep_partial_full_content_checks(
    before: dict[str, Any],
    partial: dict[str, Any],
    full: dict[str, Any],
    file_id: int,
    model_kind: str,
) -> dict[str, bool]:
    partial_rows = {int(row["id"]): row for row in partial["rows"]}
    full_rows = {int(row["id"]): row for row in full["rows"]}
    partial_row = partial_rows.get(file_id)
    full_row = full_rows.get(file_id)
    return {
        "initialStateUnprocessed": deep_snapshot_is_unprocessed(before, [file_id]),
        "partialMarkersRemainNull": partial_row is not None
        and partial_row.get("vlm_model") is None
        and partial_row.get("vlm_full_model") is None
        and partial_row.get("vlm_analyzed_at") is None,
        "partialDescriptionRemainsNull": partial_row is not None
        and partial_row.get("vlm_description") is None,
        "partialProposedNameRemainsNull": partial_row is not None
        and partial_row.get("vlm_proposed_name") is None,
        "partialTagsPersisted": int(
            partial["vlmTagsByFile"].get(str(file_id), 0)
        )
        > 0,
        "rawModelCanonical": full_row is not None
        and full_row.get("vlm_model") == model_kind,
        "fullMarkerCanonical": full_row is not None
        and full_row.get("vlm_full_model") == model_kind,
        "fullDescriptionPersisted": full_row is not None
        and bool(str(full_row.get("vlm_description") or "").strip()),
        "fullProposedNamePersisted": full_row is not None
        and bool(str(full_row.get("vlm_proposed_name") or "").strip()),
        "fullTagsPersisted": int(full["vlmTagsByFile"].get(str(file_id), 0)) > 0,
    }


def deep_snapshot_delta(
    before: dict[str, Any], after: dict[str, Any]
) -> dict[str, Any]:
    before_rows = {int(row["id"]): row for row in before["rows"]}
    after_rows = {int(row["id"]): row for row in after["rows"]}
    all_ids = sorted(set(before_rows) | set(after_rows))
    changed_ids = [
        file_id
        for file_id in all_ids
        if before_rows.get(file_id) != after_rows.get(file_id)
        or before["vlmTagsByFile"].get(str(file_id), 0)
        != after["vlmTagsByFile"].get(str(file_id), 0)
    ]

    def analyzed_at_advanced(file_id: int) -> bool:
        old = before_rows.get(file_id, {}).get("vlm_analyzed_at")
        new = after_rows.get(file_id, {}).get("vlm_analyzed_at")
        if new is None:
            return False
        if old is None:
            return True
        try:
            return float(new) > float(old)
        except (TypeError, ValueError):
            return new != old

    return {
        "changedFileIDs": changed_ids,
        "unchangedFileIDs": [
            file_id for file_id in all_ids if file_id not in changed_ids
        ],
        "analyzedAtAdvancedFileIDs": [
            file_id for file_id in all_ids if analyzed_at_advanced(file_id)
        ],
        "beforeRowCount": len(before_rows),
        "afterRowCount": len(after_rows),
    }


def bad_tuning_environment(allow: set[str]) -> list[str]:
    result: list[str] = []
    for name in os.environ:
        normalized_name = name.upper()
        if normalized_name in CONTROLLED_FILEID_ENV or normalized_name in allow:
            continue
        if normalized_name.startswith("FILEID_"):
            result.append(name)
    return sorted(result)


def isolated_environment(
    allow: set[str],
    *,
    state: Path,
    db_path: Path,
    models: Path,
    runtime_temp: Path,
    ort_dylib_path: Path | None,
) -> tuple[dict[str, str], list[str]]:
    stripped: list[str] = []
    environment: dict[str, str] = {}
    for name, value in os.environ.items():
        normalized_name = name.upper()
        if normalized_name.startswith("FILEID_") and normalized_name not in allow:
            stripped.append(name)
            continue
        if normalized_name == "ORT_DYLIB_PATH":
            stripped.append(name)
            continue
        environment[name] = value
    if ort_dylib_path is not None:
        environment["ORT_DYLIB_PATH"] = str(ort_dylib_path)
    environment["LOCALAPPDATA"] = str(state)
    environment["FILEID_DB"] = str(db_path)
    environment["FILEID_MODELS_DIR"] = str(models)
    environment["FILEID_LOG"] = "info"
    environment["FILEID_RESTRUCTURE_LARGE_PLAN_THRESHOLD"] = "0"
    environment["TEMP"] = str(runtime_temp)
    environment["TMP"] = str(runtime_temp)
    environment["TMPDIR"] = str(runtime_temp)
    return environment, sorted(stripped)


def validate_face_invariants(metrics: dict[str, Any]) -> dict[str, bool]:
    crops = metrics.get("crops")
    centroids = metrics["centroids"]
    cohesion = metrics["topClusterCohesion"]
    cohesion_values = [
        cohesion.get("p01Minimum"),
        cohesion.get("p05Minimum"),
        cohesion.get("p05Median"),
        cohesion.get("clusterMedianMinimum"),
    ]
    return {
        "faceAccounting": metrics["totalDetected"]
        == metrics["excluded"]
        + metrics["lowQuality"]
        + metrics["qualityEligible"],
        "assignmentAccounting": metrics["qualityEligible"]
        == metrics["assignedEligible"] + metrics["unmatchedEligible"],
        "clusterInputAccounting": 0
        <= metrics["unmatchedClusterInput"]
        <= metrics["clusterInputFaces"],
        "printDataIs512Bytes": metrics["embeddingLengths"] == {"512": metrics["totalDetected"]},
        "mirrorEmbeddingIs512Bytes": metrics["mirrorEmbeddingLengths"]
        == {"512": metrics["totalDetected"]},
        "embeddingColumnsEqual": metrics["mismatchedEmbeddingColumns"] == 0,
        "noZeroFacePersons": metrics["zeroFacePersons"] == 0,
        "noOrphanPersonReferences": metrics["orphanPersonReferences"] == 0,
        "fileCountsMatch": metrics["fileCountMismatches"] == 0,
        "representativesValid": metrics["badRepresentativeFaces"] == 0,
        "noSameFileIdentityCollisions": metrics[
            "sameFileIdentityCollisionGroups"
        ]
        == 0
        and metrics["sameFileIdentityCollisionExtraFaces"] == 0,
        "noTotalClusterCollapse": metrics["persons"] == 0
        or metrics["largestClusterShare"] <= 0.90,
        "topClusterCohesionAvailable": metrics["persons"] == 0
        or int(cohesion["profileCount"]) > 0,
        "topClusterCohesionFinite": metrics["persons"] == 0
        or all(
            value is not None and math.isfinite(float(value))
            for value in cohesion_values
        ),
        "centroidsValid": centroids["invalidLength"] == 0
        and centroids["nonFinite"] == 0
        and centroids["nonUnit"] == 0
        and centroids["invalidAnchorRadius"] == 0,
        "cropIDsMatch": crops is None or bool(crops["exactIDSetMatch"]),
    }


def all_checks(value: Any) -> list[str]:
    failed: list[str] = []

    def walk_check_leaves(item: Any, path: str) -> None:
        if isinstance(item, dict):
            if not item:
                failed.append(path)
            for key, child in item.items():
                walk_check_leaves(child, f"{path}.{key}")
        elif isinstance(item, list):
            if not item:
                failed.append(path)
            for index, child in enumerate(item):
                walk_check_leaves(child, f"{path}[{index}]")
        elif item is not True:
            failed.append(path)

    def walk(item: Any, path: str) -> None:
        if isinstance(item, dict):
            for key, child in item.items():
                child_path = f"{path}.{key}" if path else str(key)
                if key == "checks":
                    if isinstance(child, (dict, list)):
                        walk_check_leaves(child, child_path)
                    else:
                        failed.append(child_path)
                else:
                    walk(child, child_path)
        elif isinstance(item, list):
            for index, child in enumerate(item):
                walk(child, f"{path}[{index}]")

    walk(value, "")
    return sorted(set(failed))


def scan_state_logs(log_directory: Path) -> dict[str, Any]:
    ort_pattern = re.compile(r"Loaded ONNX Runtime dylib with version '([^']+)'")
    versions: list[str] = []
    bad_markers: list[str] = []
    files: list[str] = []
    if log_directory.is_dir():
        for path in sorted(log_directory.iterdir()):
            if not path.is_file():
                continue
            files.append(str(path))
            with path.open("r", encoding="utf-8", errors="strict") as handle:
                for line_number, line in enumerate(handle, 1):
                    if match := ort_pattern.search(line):
                        versions.append(match.group(1))
                    if BAD_LOG_MARKER.search(line) and len(bad_markers) < 100:
                        bad_markers.append(f"{path.name}:{line_number}: {line.rstrip()}")
    return {
        "files": files,
        "ortVersions": versions,
        "badMarkers": bad_markers,
        "checks": {
            "ortRuntimeObserved": bool(versions),
            "ortRuntimeIs122": bool(versions)
            and all(version.startswith("1.22.") for version in versions),
            "noErrorOrPanicMarkers": not bad_markers,
        },
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Clone a checkpointed FileID DB, exercise face clustering, merge "
            "suggestions, paged Restructure planning, and bounded Deep Analyze "
            "without mutating the corpus."
        )
    )
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--seed-db", type=Path, required=True)
    parser.add_argument("--face-crops", type=Path)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--models", type=Path, required=True)
    parser.add_argument(
        "--reuse-models-in-place",
        action="store_true",
        help=(
            "reuse and fingerprint an already-isolated model directory instead "
            "of copying it into the disposable state directory"
        ),
    )
    parser.add_argument("--ort-dylib-path", type=Path)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--state-directory", type=Path)
    parser.add_argument(
        "--keep-state",
        action="store_true",
        help="retain the isolated engine, model, and database clones after validation",
    )
    parser.add_argument("--model-kind", default="mistral_small_3_2")
    parser.add_argument("--model-alias")
    parser.add_argument("--deep-limit", type=int, default=6)
    parser.add_argument("--deep-cancel-limit", type=int, default=2)
    parser.add_argument("--skip-faces", action="store_true")
    parser.add_argument("--skip-restructure", action="store_true")
    parser.add_argument("--skip-deep-analyze", action="store_true")
    parser.add_argument("--fingerprint-samples", type=int, default=1_000)
    parser.add_argument("--max-sample-bytes", type=int, default=8 * 1024 * 1024)
    parser.add_argument("--face-max-persons", type=int, default=2_300)
    parser.add_argument(
        "--face-max-persons-per-1000-eligible", type=float, default=12.0
    )
    parser.add_argument("--face-max-tiny-cluster-ratio", type=float, default=0.95)
    parser.add_argument("--face-max-largest-cluster-share", type=float, default=0.35)
    parser.add_argument("--face-max-person-reduction-fraction", type=float, default=0.75)
    parser.add_argument("--face-min-assigned-retention", type=float, default=0.75)
    parser.add_argument("--face-min-top-cluster-p05", type=float, default=0.30)
    parser.add_argument(
        "--face-min-top-cluster-median-p05", type=float, default=0.40
    )
    parser.add_argument("--cluster-timeout-minutes", type=int, default=30)
    parser.add_argument("--plan-timeout-minutes", type=int, default=30)
    parser.add_argument("--deep-timeout-minutes", type=int, default=120)
    parser.add_argument("--allow-tuning-env", action="append", default=[])
    return parser.parse_args()


def run_validation() -> int:
    global _RETENTION_ARTIFACTS
    global _RETENTION_CLEANUP_DISPOSITION
    global _RETENTION_DELETE_APPROVED
    global _RETENTION_KEEP
    global _RETENTION_MARKER
    global _RETENTION_STATE
    global _RETENTION_STATE_IDENTITY

    args = parse_args()
    _RETENTION_KEEP = bool(args.keep_state)
    _RETENTION_DELETE_APPROVED = False
    _RETENTION_CLEANUP_DISPOSITION = "not-created"
    _RETENTION_STATE_IDENTITY = None
    requested_model_kind = args.model_kind
    canonical_model_kind = VLM_CANONICAL_IDS.get(
        requested_model_kind, requested_model_kind
    )
    model_alias = args.model_alias or VLM_ALIASES.get(canonical_model_kind)
    if (
        model_alias is not None
        and VLM_CANONICAL_IDS.get(model_alias, model_alias) != canonical_model_kind
    ):
        raise ValueError(
            f"--model-alias {model_alias!r} does not resolve to {canonical_model_kind!r}"
        )
    args.model_kind = canonical_model_kind
    repo_root = Path(__file__).resolve().parents[3]
    corpus = args.corpus.resolve(strict=True)
    seed_db = args.seed_db.resolve(strict=True)
    engine = args.engine.resolve(strict=True)
    models = args.models.resolve(strict=True)
    ort_dylib_path = (
        args.ort_dylib_path.resolve(strict=True) if args.ort_dylib_path else None
    )
    source_face_crops = (
        args.face_crops.resolve(strict=True) if args.face_crops else None
    )
    if not corpus.is_dir():
        raise ValueError(f"corpus is not a directory: {corpus}")
    if not seed_db.is_file():
        raise ValueError(f"seed DB is not a file: {seed_db}")
    if not engine.is_file():
        raise ValueError(f"engine is not a file: {engine}")
    if not models.is_dir():
        raise ValueError(f"models directory is not a directory: {models}")
    if ort_dylib_path is not None and not ort_dylib_path.is_file():
        raise ValueError(f"ORT dylib is not a file: {ort_dylib_path}")
    if source_face_crops is not None and not source_face_crops.is_dir():
        raise ValueError(
            f"face-crops directory is not a directory: {source_face_crops}"
        )
    if source_face_crops is not None:
        require_outside(source_face_crops, corpus, "face-crops directory")
    if args.deep_limit < 0:
        raise ValueError("--deep-limit must be non-negative")
    if args.deep_cancel_limit < 0:
        raise ValueError("--deep-cancel-limit must be non-negative")
    if args.fingerprint_samples <= 0:
        raise ValueError("--fingerprint-samples must be positive")
    if args.max_sample_bytes <= 0:
        raise ValueError("--max-sample-bytes must be positive")
    if args.face_max_persons <= 0:
        raise ValueError("--face-max-persons must be positive")
    if (
        not math.isfinite(args.face_max_persons_per_1000_eligible)
        or args.face_max_persons_per_1000_eligible <= 0
    ):
        raise ValueError("--face-max-persons-per-1000-eligible must be positive")
    if (
        not math.isfinite(args.face_max_tiny_cluster_ratio)
        or not 0 <= args.face_max_tiny_cluster_ratio <= 1
    ):
        raise ValueError("--face-max-tiny-cluster-ratio must be between 0 and 1")
    if (
        not math.isfinite(args.face_max_largest_cluster_share)
        or not 0 < args.face_max_largest_cluster_share < 1
    ):
        raise ValueError(
            "--face-max-largest-cluster-share must be between 0 and 1"
        )
    if (
        not math.isfinite(args.face_max_person_reduction_fraction)
        or not 0 < args.face_max_person_reduction_fraction < 1
    ):
        raise ValueError(
            "--face-max-person-reduction-fraction must be between 0 and 1"
        )
    if (
        not math.isfinite(args.face_min_assigned_retention)
        or not 0 < args.face_min_assigned_retention <= 1
    ):
        raise ValueError("--face-min-assigned-retention must be between 0 and 1")
    if (
        not math.isfinite(args.face_min_top_cluster_p05)
        or not -1 <= args.face_min_top_cluster_p05 <= 1
        or not math.isfinite(args.face_min_top_cluster_median_p05)
        or not -1 <= args.face_min_top_cluster_median_p05 <= 1
    ):
        raise ValueError("face cohesion floors must be between -1 and 1")
    if min(
        args.cluster_timeout_minutes,
        args.plan_timeout_minutes,
        args.deep_timeout_minutes,
    ) <= 0:
        raise ValueError("operation timeouts must be positive")

    allowed_fileid_environment = {name.upper() for name in args.allow_tuning_env}
    invalid_allowed_environment = sorted(
        name
        for name in allowed_fileid_environment
        if not name.startswith("FILEID_") or name in CONTROLLED_FILEID_ENV
    )
    if invalid_allowed_environment:
        raise ValueError(
            "--allow-tuning-env only accepts non-harness FILEID_* variables: "
            + ", ".join(invalid_allowed_environment)
        )
    bad_environment = bad_tuning_environment(allowed_fileid_environment)
    if bad_environment:
        raise RuntimeError(
            "inherited FILEID_* variables require explicit --allow-tuning-env: "
            + ", ".join(bad_environment)
        )

    source_models = models
    source_engine = engine
    source_engine_runtime_files = sorted(
        source_engine.parent.glob("*.dll"), key=lambda path: path.name.lower()
    )
    source_ort_dylib_path = ort_dylib_path
    colocated_ort = source_engine.parent / "onnxruntime.dll"
    if source_ort_dylib_path is None and colocated_ort.is_file():
        source_ort_dylib_path = colocated_ort.resolve(strict=True)
    if source_ort_dylib_path is not None and not (
        under_root(source_ort_dylib_path, source_models)
        or source_ort_dylib_path.parent == source_engine.parent
    ):
        raise ValueError(
            "ORT dylib must be inside the source models directory or colocated with the source engine"
        )
    protected_inputs = [
        corpus,
        seed_db,
        source_engine,
        source_models,
        *source_engine_runtime_files,
    ]
    if source_face_crops is not None:
        protected_inputs.append(source_face_crops)

    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    artifacts = (
        args.artifacts.resolve()
        if args.artifacts
        else repo_root / ".ralph" / f"real-data-{timestamp}-{uuid.uuid4().hex[:8]}"
    )
    if args.state_directory:
        state = args.state_directory.resolve()
    else:
        temp_parent = Path(tempfile.gettempdir()).resolve(strict=True)
        state = temp_parent / f"fileid-real-data-{uuid.uuid4().hex}"
    require_disjoint(artifacts, protected_inputs, "artifacts directory")
    require_disjoint(state, protected_inputs, "state directory")
    require_disjoint(artifacts, [state], "artifacts directory")
    if artifacts.exists() and not artifacts.is_dir():
        raise ValueError(f"artifacts path is not a directory: {artifacts}")
    if state.exists() and not state.is_dir():
        raise ValueError(f"state path is not a directory: {state}")
    if artifacts.exists() and any(artifacts.iterdir()):
        raise FileExistsError(f"artifacts directory is not empty: {artifacts}")
    if state.exists() and any(state.iterdir()):
        raise FileExistsError(f"state directory is not empty: {state}")
    artifacts.mkdir(parents=True, exist_ok=True)
    if args.state_directory:
        state.mkdir(parents=True, exist_ok=True)
    else:
        state.mkdir(parents=False, exist_ok=False)
    _state_info, _RETENTION_STATE_IDENTITY = path_identity_no_reparse(
        state, expect_directory=True
    )
    _RETENTION_MARKER = f"{uuid.uuid4()}\n"
    (state / ".fileid-real-data-validation-state").write_text(
        _RETENTION_MARKER, encoding="utf-8"
    )
    _RETENTION_ARTIFACTS = artifacts
    _RETENTION_STATE = state

    model_process_guard = FileIDProcessGuard(
        "model and face-crop input isolation"
    )
    model_process_guard.start()
    try:
        source_models_before = full_tree_manifest(source_models)
        source_face_crops_before = (
            collect_face_crop_inventory(source_face_crops)
            if source_face_crops is not None
            else None
        )
        required_free_bytes = seed_db.stat().st_size + 5 * 1024 * 1024 * 1024
        if not args.reuse_models_in_place:
            required_free_bytes += source_models_before["bytes"]
        available_free_bytes = shutil.disk_usage(state).free
        if available_free_bytes < required_free_bytes:
            raise RuntimeError(
                "insufficient free space for isolated models/catalog: "
                f"need {required_free_bytes}, available {available_free_bytes}"
            )
        if args.reuse_models_in_place:
            models = source_models
        else:
            models = state / "Models"
            shutil.copytree(source_models, models, copy_function=shutil.copy2)
        isolated_models_manifest = full_tree_manifest(models)
        source_models_after_copy = full_tree_manifest(source_models)
        source_face_crops_after_copy = (
            collect_face_crop_inventory(source_face_crops)
            if source_face_crops is not None
            else None
        )
    finally:
        model_process_guard.stop_and_assert_clean()
    if source_models_after_copy != source_models_before:
        raise RuntimeError("live models directory changed while it was being cloned")
    if isolated_models_manifest != source_models_before:
        raise RuntimeError("isolated models clone does not match live model source")
    if source_face_crops_after_copy != source_face_crops_before:
        raise RuntimeError("live face-crop membership changed during input isolation")
    face_crops = source_face_crops_before

    engine_directory = state / "engine"
    engine_directory.mkdir(parents=False, exist_ok=False)
    engine = engine_directory / source_engine.name
    source_engine_before = file_snapshot(source_engine)
    source_engine_runtime_before = {
        path.name: file_snapshot(path) for path in source_engine_runtime_files
    }
    shutil.copy2(source_engine, engine)
    for runtime_file in source_engine_runtime_files:
        shutil.copy2(runtime_file, engine_directory / runtime_file.name)
    isolated_engine_snapshot = file_snapshot(engine)
    isolated_engine_runtime = {
        path.name: file_snapshot(engine_directory / path.name)
        for path in source_engine_runtime_files
    }
    source_engine_after_copy = file_snapshot(source_engine)
    source_engine_runtime_after_copy = {
        path.name: file_snapshot(path) for path in source_engine_runtime_files
    }
    if source_engine_after_copy != source_engine_before:
        raise RuntimeError("source engine changed while it was being cloned")
    if source_engine_runtime_after_copy != source_engine_runtime_before:
        raise RuntimeError("source engine runtime changed while it was being cloned")
    if isolated_engine_snapshot["sha256"] != source_engine_before["sha256"]:
        raise RuntimeError("isolated engine clone hash does not match source engine")
    if any(
        isolated_engine_runtime[name]["sha256"] != source["sha256"]
        for name, source in source_engine_runtime_before.items()
    ):
        raise RuntimeError("isolated engine runtime clone does not match source runtime")

    if source_ort_dylib_path is not None:
        if under_root(source_ort_dylib_path, source_models):
            ort_relative = source_ort_dylib_path.relative_to(source_models)
            ort_dylib_path = (models / ort_relative).resolve(strict=True)
        else:
            ort_dylib_path = (engine_directory / source_ort_dylib_path.name).resolve(
                strict=True
            )

    model_copy = {
        "mode": (
            "reused-read-only" if args.reuse_models_in_place else "isolated-copy"
        ),
        "source": str(source_models),
        "sourceBefore": source_models_before,
        "sourceAfterCopy": source_models_after_copy,
        "isolated": str(models),
        "isolatedManifest": isolated_models_manifest,
        "fileIDProcessSnapshotSamples": model_process_guard.samples,
        "requiredFreeBytes": required_free_bytes,
        "availableFreeBytesBeforeCopy": available_free_bytes,
    }
    face_crop_input = {
        "sourceBefore": (
            source_face_crops_before.summary()
            if source_face_crops_before is not None
            else None
        ),
        "sourceAfterIsolation": (
            source_face_crops_after_copy.summary()
            if source_face_crops_after_copy is not None
            else None
        ),
        "frozenForRun": source_face_crops_before is not None,
        "fileIDProcessSnapshotSamples": model_process_guard.samples,
    }
    engine_copy = {
        "sourceBefore": source_engine_before,
        "sourceAfterCopy": source_engine_after_copy,
        "isolated": isolated_engine_snapshot,
        "runtimeSourceBefore": source_engine_runtime_before,
        "runtimeSourceAfterCopy": source_engine_runtime_after_copy,
        "isolatedRuntime": isolated_engine_runtime,
    }

    app_root = state / "FileID"
    app_root.mkdir(parents=True, exist_ok=False)
    db_path = app_root / "fileid.sqlite"

    wal_path = seed_db.with_name(seed_db.name + "-wal")
    shm_path = seed_db.with_name(seed_db.name + "-shm")
    journal_path = seed_db.with_name(seed_db.name + "-journal")
    process_guard = FileIDProcessGuard("seed catalog copy")
    process_guard.start()
    try:
        wal_before = file_snapshot(wal_path)
        shm_before = file_snapshot(shm_path)
        journal_before = file_snapshot(journal_path)
        if wal_before["size"]:
            raise RuntimeError(
                f"seed WAL is not checkpointed ({wal_before['size']} bytes): "
                f"{wal_path}"
            )
        if journal_before["size"]:
            raise RuntimeError(
                f"seed rollback journal is not empty ({journal_before['size']} bytes): "
                f"{journal_path}"
            )
        seed_before = {
            "path": str(seed_db),
            "size": seed_db.stat().st_size,
            "modifiedNS": seed_db.stat().st_mtime_ns,
            "sha256": sha256_file(seed_db),
        }
        shutil.copy2(seed_db, db_path)
        clone_sha256 = sha256_file(db_path)
        seed_after_copy = {
            "path": str(seed_db),
            "size": seed_db.stat().st_size,
            "modifiedNS": seed_db.stat().st_mtime_ns,
            "sha256": sha256_file(seed_db),
        }
        wal_after = file_snapshot(wal_path)
        shm_after = file_snapshot(shm_path)
        journal_after = file_snapshot(journal_path)
    finally:
        process_guard.stop_and_assert_clean()
    if wal_after["size"]:
        raise RuntimeError(
            f"seed WAL became non-empty during copy ({wal_after['size']} bytes): "
            f"{wal_path}"
        )
    if journal_after["size"]:
        raise RuntimeError(
            "seed rollback journal became non-empty during copy "
            f"({journal_after['size']} bytes): {journal_path}"
        )
    if wal_after != wal_before or shm_after != shm_before or journal_after != journal_before:
        raise RuntimeError("SQLite sidecar state changed while the live seed was cloned")
    if seed_after_copy != seed_before:
        raise RuntimeError("live seed database changed while it was being cloned")
    if clone_sha256 != seed_before["sha256"]:
        raise RuntimeError("isolated DB clone hash does not match stable live seed")
    if os.path.samefile(seed_db, db_path):
        raise RuntimeError("isolated DB clone resolves to the live seed file")
    clone_integrity = sqlite_integrity_snapshot(db_path, immutable=True)
    if not all(clone_integrity["checks"].values()):
        raise RuntimeError(
            "isolated DB clone failed SQLite integrity or foreign-key validation"
        )
    seed_copy = {
        "seedAfterCopy": seed_after_copy,
        "clone": {
            "path": str(db_path),
            "size": db_path.stat().st_size,
            "sha256": clone_sha256,
        },
        "walBefore": wal_before,
        "walAfter": wal_after,
        "shmBefore": shm_before,
        "shmAfter": shm_after,
        "journalBefore": journal_before,
        "journalAfter": journal_after,
        "cloneIntegrity": clone_integrity,
        "fileIDProcessSnapshotSamples": process_guard.samples,
    }

    corpus_files, corpus_directories, corpus_index_errors = corpus_index(corpus)
    if corpus_index_errors:
        raise RuntimeError(
            "corpus traversal is not safe: " + "; ".join(corpus_index_errors[:20])
        )
    before_fingerprint = safe_tree_fingerprint(
        corpus, args.fingerprint_samples, args.max_sample_bytes
    )
    if before_fingerprint.get("errors"):
        raise RuntimeError(
            f"corpus fingerprint reported errors: {before_fingerprint['errors']}"
        )
    if not before_fingerprint.get("contentSamples"):
        raise RuntimeError("corpus fingerprint produced no content samples")
    write_json(artifacts / "corpus-before.json", before_fingerprint)

    with connect_readonly(db_path, immutable=True) as connection:
        db_paths_by_id = {
            int(row[0]): str(row[1])
            for row in connection.execute("SELECT id,path_text FROM files")
        }
        db_paths = set(db_paths_by_id.values())
        file_count = int(scalar(connection, "SELECT COUNT(*) FROM files"))
        failed_file_count = int(
            scalar(connection, "SELECT COUNT(*) FROM files WHERE failed=1")
        )
        case_variant_count = sum(
            1
            for path in db_paths
            if under_root(path, corpus)
            and not (
                os.path.normpath(path) == os.path.normpath(str(corpus))
                or os.path.normpath(path).startswith(
                    os.path.normpath(str(corpus)) + os.sep
                )
            )
        )

    summary: dict[str, Any] = {
        "startedAt": utc_now(),
        "sourceRoot": str(corpus),
        "seed": seed_before,
        "seedCopy": seed_copy,
        "modelCopy": model_copy,
        "engineCopy": engine_copy,
        "faceCropInput": face_crop_input,
        "stateDirectory": str(state),
        "localAppData": str(state),
        "fileIDDB": str(db_path),
        "modelsDirectory": str(models),
        "sourceModelsDirectory": str(source_models),
        "deepAnalyzeModel": {
            "requested": requested_model_kind,
            "canonical": canonical_model_kind,
            "aliasProbe": model_alias,
        },
        "engine": {
            "path": str(engine),
            "size": engine.stat().st_size,
            "modifiedNS": engine.stat().st_mtime_ns,
            "sha256": sha256_file(engine),
        },
        "catalog": {
            "files": file_count,
            "failedFiles": failed_file_count,
            "dbPaths": len(db_paths),
            "corpusFiles": len(corpus_files),
            "corpusDirectories": len(corpus_directories),
            "corpusIndexErrors": corpus_index_errors[:100],
            "caseVariantDBPaths": case_variant_count,
        },
        "environment": {
            "ortDylibPath": str(ort_dylib_path) if ort_dylib_path else None,
            "ortDylibPathRemoved": ort_dylib_path is None,
            "allowedTuningVariables": sorted(allowed_fileid_environment),
        },
    }

    runtime_temp = state / "temp"
    runtime_temp.mkdir(parents=False, exist_ok=False)
    environment, stripped_environment = isolated_environment(
        allowed_fileid_environment,
        state=state,
        db_path=db_path,
        models=models,
        runtime_temp=runtime_temp,
        ort_dylib_path=ort_dylib_path,
    )
    summary["environment"]["strippedInheritedVariables"] = stripped_environment

    driver = EngineDriver(engine, environment, artifacts, state)
    monitor: ResourceMonitor | None = None
    exit_code: int | None = None
    run_error: str | None = None
    try:
        driver.start()
        assert driver.process is not None
        monitor = ResourceMonitor(driver.process)
        monitor.start()
        ready_event = driver.wait_for(
            "ready", after=0, timeout_seconds=90
        )
        summary["ready"] = inner_payload(ready_event.value, "ready")
        ready_end = driver.wait_for_quiet()
        ready_events = [
            item
            for item in driver.events_between(0, ready_end)
            if event_kind(item.value) == "ready"
        ]
        if len(ready_events) != 1:
            raise RuntimeError(f"expected one ready event, observed {len(ready_events)}")
        summary["readyBarrier"] = health_barrier(driver, "initial-ready")

        face_runs = []
        baseline_face = collect_face_metrics(db_path, face_crops)
        baseline_face["checks"] = validate_face_invariants(baseline_face)
        baseline_face["checks"].pop("noSameFileIdentityCollisions", None)
        baseline_named_membership = named_membership_snapshot(db_path)
        summary["faces"] = {
            "baseline": baseline_face,
            "namedMembershipBaseline": named_membership_summary(
                baseline_named_membership
            ),
            "runs": face_runs,
        }
        if not args.skip_faces:
            for run_index in range(1, 3):
                command_id = f"cluster-{run_index}"
                started = time.monotonic()
                driver.send(command_id, {"runFaceClustering": {}})
                event = driver.wait_for(
                    "faceClusteringComplete",
                    command_id=command_id,
                    timeout_seconds=args.cluster_timeout_minutes * 60,
                    predicate=lambda value: isinstance(value, dict),
                )
                command_fence = settle_command(
                    driver, command_id, {"faceClusteringComplete": 1}
                )
                metrics = collect_face_metrics(db_path, face_crops)
                metrics["checks"] = validate_face_invariants(metrics)
                current_named_membership = named_membership_snapshot(db_path)
                admission_metrics = named_membership_admission_metrics(
                    baseline_named_membership,
                    current_named_membership,
                )
                face_runs.append(
                    {
                        "run": run_index,
                        "wallSeconds": time.monotonic() - started,
                        "event": inner_payload(event.value, "faceClusteringComplete"),
                        "commandFence": command_fence,
                        "namedMembership": named_membership_summary(
                            current_named_membership
                        ),
                        "namedMembershipAdmissions": admission_metrics,
                        "namedMembershipChecks": {
                            "checks": named_membership_checks(
                                baseline_named_membership,
                                current_named_membership,
                                admission_metrics,
                            )
                        },
                        "metrics": metrics,
                    }
                )
            final_face = face_runs[-1]["metrics"]
            final_face_event = face_runs[-1]["event"]
            displayable_persons_per_1000_eligible = (
                final_face["displayablePersons"]
                * 1000
                / final_face["qualityEligible"]
                if final_face["qualityEligible"]
                else math.inf
            )
            baseline_cluster_share = float(baseline_face["largestClusterShare"])
            calibrated_cluster_share_ceiling = min(
                args.face_max_largest_cluster_share,
                max(0.10, baseline_cluster_share * 1.25),
            )
            calibrated_cluster_size_ceiling = max(
                100,
                math.ceil(float(baseline_face["maximumClusterSize"]) * 1.25),
                math.ceil(
                    float(final_face["assignedPersonFaces"])
                    * calibrated_cluster_share_ceiling
                ),
            )
            minimum_person_count = max(
                1,
                math.floor(
                    float(baseline_face["persons"])
                    * (1.0 - args.face_max_person_reduction_fraction)
                ),
            )
            baseline_cohesion = baseline_face["topClusterCohesion"]
            final_cohesion = final_face["topClusterCohesion"]

            def calibrated_cohesion_floor(
                key: str, configured_floor: float, allowed_drop: float
            ) -> float:
                baseline_value = baseline_cohesion.get(key)
                if baseline_value is None or not math.isfinite(
                    float(baseline_value)
                ):
                    return configured_floor
                return max(
                    configured_floor,
                    float(baseline_value) - allowed_drop,
                )

            p01_floor = calibrated_cohesion_floor("p01Minimum", 0.15, 0.05)
            p05_floor = calibrated_cohesion_floor(
                "p05Minimum",
                args.face_min_top_cluster_p05,
                0.05,
            )
            median_p05_floor = calibrated_cohesion_floor(
                "p05Median",
                args.face_min_top_cluster_median_p05,
                0.03,
            )
            cluster_median_floor = calibrated_cohesion_floor(
                "clusterMedianMinimum",
                0.60,
                0.03,
            )
            baseline_pair_counts = baseline_face["centroids"][
                "crossPersonPairCounts"
            ]
            observed_pair_counts = face_runs[0]["metrics"]["centroids"][
                "crossPersonPairCounts"
            ]
            baseline_fragment_risk = centroid_fragment_risk(baseline_pair_counts)
            observed_fragment_risk = centroid_fragment_risk(observed_pair_counts)
            summary["faces"]["acceptance"] = {
                "maxPersons": args.face_max_persons,
                "absoluteCeilingKind": "raw-cluster non-regression guard",
                "absoluteCeilingRationale": {
                    "reason": (
                        "The raw 2,300 ceiling bounds retained search and merge "
                        "state. People-grid overload is measured at the actual "
                        "six-face presentation boundary, while named clusters "
                        "remain visible regardless of size."
                    ),
                },
                "maxPersonsPer1000Eligible": (
                    args.face_max_persons_per_1000_eligible
                ),
                "maxTinyClusterRatio": args.face_max_tiny_cluster_ratio,
                "maxPersonReductionFraction": (
                    args.face_max_person_reduction_fraction
                ),
                "minAssignedRetention": args.face_min_assigned_retention,
                "minimumPersonCount": minimum_person_count,
                "configuredMaxLargestClusterShare": (
                    args.face_max_largest_cluster_share
                ),
                "calibratedLargestClusterShareCeiling": (
                    calibrated_cluster_share_ceiling
                ),
                "calibratedLargestClusterSizeCeiling": (
                    calibrated_cluster_size_ceiling
                ),
                "cohesionFloors": {
                    "p01Minimum": p01_floor,
                    "p05Minimum": p05_floor,
                    "p05Median": median_p05_floor,
                    "clusterMedianMinimum": cluster_median_floor,
                },
                "observedPersons": final_face["persons"],
                "observedDisplayablePersons": final_face["displayablePersons"],
                "observedUnknownBuckets": final_face["unknownPersons"],
                "observedDisplayablePersonsPer1000Eligible": (
                    displayable_persons_per_1000_eligible
                ),
                "observedLargestClusterSize": final_face["maximumClusterSize"],
                "observedLargestClusterShare": final_face[
                    "largestClusterShare"
                ],
                "observedTopClusterCohesion": final_cohesion,
                "highSimilarityFragmentRisk": {
                    "weights": {"0.80": 1, "0.85": 2, "0.88": 4},
                    "baselinePairCounts": baseline_pair_counts,
                    "observedPairCounts": observed_pair_counts,
                    "baselineScore": baseline_fragment_risk,
                    "observedScore": observed_fragment_risk,
                },
                "legacy999Diagnostic": {
                    "ceiling": 999,
                    "wouldPass": final_face["persons"] <= 999,
                    "blocking": False,
                },
            }
            summary["faces"]["checks"] = {
                "partitionStableAcrossRerun": face_runs[0]["metrics"][
                    "partitionDigest"
                ]
                == face_runs[1]["metrics"]["partitionDigest"],
                "assignedEligibleStableAcrossRerun": face_runs[0]["metrics"][
                    "assignedEligible"
                ]
                == face_runs[1]["metrics"]["assignedEligible"],
                "displayablePersonsNonIncreasing": face_runs[0]["metrics"][
                    "displayablePersons"
                ]
                <= baseline_face["displayablePersons"],
                "personReductionBounded": final_face["persons"]
                >= minimum_person_count,
                "absolutePersonCeiling": final_face["persons"]
                <= args.face_max_persons,
                "displayablePersonRatioCeiling": displayable_persons_per_1000_eligible
                <= args.face_max_persons_per_1000_eligible,
                "tinyClusterRatioCeiling": final_face["personsAtMost12Fraction"]
                <= args.face_max_tiny_cluster_ratio,
                "largestClusterShareBounded": final_face[
                    "largestClusterShare"
                ]
                <= args.face_max_largest_cluster_share,
                "largestClusterSizeBounded": final_face["maximumClusterSize"]
                <= calibrated_cluster_size_ceiling,
                "topClusterP01Cohesive": final_cohesion["p01Minimum"]
                is not None
                and float(final_cohesion["p01Minimum"]) >= p01_floor,
                "topClusterP05Cohesive": final_cohesion["p05Minimum"]
                is not None
                and float(final_cohesion["p05Minimum"]) >= p05_floor,
                "topClusterMedianP05Cohesive": final_cohesion["p05Median"]
                is not None
                and float(final_cohesion["p05Median"])
                >= median_p05_floor,
                "topClusterMedianCohesive": final_cohesion[
                    "clusterMedianMinimum"
                ]
                is not None
                and float(final_cohesion["clusterMedianMinimum"])
                >= cluster_median_floor,
                "assignedEligibleRetained": face_runs[0]["metrics"][
                    "assignedEligible"
                ]
                >= baseline_face["assignedEligible"]
                * args.face_min_assigned_retention,
                "centroidPairMetricsAvailable": observed_pair_counts is not None
                and baseline_pair_counts is not None,
                "highSimilarityFragmentRiskNonIncreasing": (
                    observed_pair_counts is not None
                    and baseline_pair_counts is not None
                    and observed_fragment_risk is not None
                    and baseline_fragment_risk is not None
                    and observed_pair_counts["0.75"]
                    <= baseline_pair_counts["0.75"]
                    and observed_pair_counts["0.85"]
                    <= baseline_pair_counts["0.85"]
                    and observed_pair_counts["0.88"]
                    <= baseline_pair_counts["0.88"]
                    and observed_fragment_risk <= baseline_fragment_risk
                ),
                "manualDifferentPersonConstraintsPreserved": final_face[
                    "manualDifferentPersonViolations"
                ]
                == 0,
                "sameFileIdentityCollisionsEliminated": final_face[
                    "sameFileIdentityCollisionGroups"
                ]
                == 0
                and final_face["sameFileIdentityCollisionExtraFaces"] == 0,
                "eventPersonCountMatchesDB": int(
                    final_face_event.get("personCount", -1)
                )
                == final_face["persons"],
                "eventFaceCountMatchesDB": int(final_face_event.get("faceCount", -1))
                == final_face["qualityEligible"],
                "eventUnmatchedMatchesDB": int(
                    final_face_event.get("unmatchedFaces", -1)
                )
                == final_face["unmatchedEligible"],
            }

            merge_started = time.monotonic()
            driver.send("merge-1", {"findMergeSuggestions": {}})
            merge_event = driver.wait_for(
                "mergeSuggestions",
                command_id="merge-1",
                timeout_seconds=args.cluster_timeout_minutes * 60,
                predicate=lambda value: (
                    isinstance(value, dict) and isinstance(value.get("pairs"), list)
                ),
            )
            merge_fence = settle_command(
                driver, "merge-1", {"mergeSuggestions": 1}
            )
            summary["mergeSuggestions"] = {
                "wallSeconds": time.monotonic() - merge_started,
                "commandFence": merge_fence,
                **validate_merge_suggestions(
                    inner_payload(merge_event.value, "mergeSuggestions"),
                    db_path,
                ),
            }

        if not args.skip_restructure:
            plan_directory = app_root / "restructure_plans"
            cancel_plan_command = f"plan-cancel-{uuid.uuid4()}"
            cancel_request_command = f"cancel-restructure-{uuid.uuid4()}"
            cancel_plan_started = time.monotonic()
            driver.send(
                cancel_plan_command,
                {
                    "planRestructure": {
                        "libraryRoot": str(corpus),
                        "supportsPagedPlans": True,
                    }
                },
            )
            driver.send(cancel_request_command, {"cancelRestructure": {}})
            cancel_plan_error_event = driver.wait_for(
                "error",
                command_id=cancel_plan_command,
                timeout_seconds=60,
                predicate=lambda value: (
                    isinstance(value, dict)
                    and value.get("kind") == "plan_restructure_cancelled"
                ),
            )
            cancel_plan_fence = settle_command(
                driver, cancel_plan_command, {"error": 1}
            )
            residual_cancel_spools = (
                [
                    str(path)
                    for path in plan_directory.iterdir()
                    if path.is_file()
                ]
                if plan_directory.is_dir()
                else []
            )
            restructure_cancellation = {
                "wallSeconds": time.monotonic() - cancel_plan_started,
                "error": inner_payload(cancel_plan_error_event.value, "error"),
                "commandFence": cancel_plan_fence,
                "residualSpools": residual_cancel_spools,
                "checks": {
                    "typedCancellation": inner_payload(
                        cancel_plan_error_event.value, "error"
                    ).get("kind")
                    == "plan_restructure_cancelled",
                    "noPartialOrActionableSpool": not residual_cancel_spools,
                },
            }
            plan_runs = []
            for run_index in range(1, 3):
                command_id = f"plan-{run_index}"
                started = time.monotonic()
                driver.send(
                    command_id,
                    {
                        "planRestructure": {
                            "libraryRoot": str(corpus),
                            "supportsPagedPlans": True,
                        }
                    },
                )
                event = driver.wait_for(
                    "restructurePlan",
                    command_id=command_id,
                    timeout_seconds=args.plan_timeout_minutes * 60,
                    predicate=lambda value: (
                        isinstance(value, dict)
                        and normalized(str(value.get("libraryRoot", "")))
                        == normalized(corpus)
                        and isinstance(value.get("moves"), list)
                        and (
                            isinstance(value.get("planID"), str)
                            or (
                                value.get("planID") is None
                                and not bool(value.get("truncated", False))
                            )
                        )
                    ),
                )
                command_fence = settle_command(
                    driver, command_id, {"restructurePlan": 1}
                )
                payload = inner_payload(event.value, "restructurePlan")
                plan_id = payload.get("planID")
                spool = None
                if isinstance(plan_id, str) and plan_id:
                    spool = app_root / "restructure_plans" / f"{plan_id}.ndjson"
                    deadline = time.monotonic() + 30
                    while not spool.is_file() and time.monotonic() < deadline:
                        time.sleep(0.1)
                    if not spool.is_file():
                        raise FileNotFoundError(f"plan spool not found: {spool}")
                parsed = validate_plan_spool(
                    spool,
                    payload,
                    corpus,
                    corpus_files,
                    corpus_directories,
                    db_paths_by_id,
                    db_path,
                )
                parsed["run"] = run_index
                parsed["wallSeconds"] = time.monotonic() - started
                parsed["commandFence"] = command_fence
                plan_runs.append(parsed)
            summary["restructure"] = {
                "cancellation": restructure_cancellation,
                "runs": plan_runs,
                "checks": {
                    "orderedPlanStable": plan_runs[0]["orderedDigest"]
                    == plan_runs[1]["orderedDigest"],
                    "canonicalPlanStable": plan_runs[0]["canonicalDigest"]
                    == plan_runs[1]["canonicalDigest"],
                    "totalsStable": plan_runs[0]["totalMoves"]
                    == plan_runs[1]["totalMoves"],
                    "planStorageModeStable": plan_runs[0]["storageMode"]
                    == plan_runs[1]["storageMode"],
                    "planHandlesValid": (
                        plan_runs[0]["planID"] is None
                        and plan_runs[1]["planID"] is None
                    )
                    or (
                        isinstance(plan_runs[0]["planID"], str)
                        and isinstance(plan_runs[1]["planID"], str)
                        and plan_runs[0]["planID"] != plan_runs[1]["planID"]
                    ),
                    "pagedRetrievalStable": plan_runs[0]["pageDigests"]
                        == plan_runs[1]["pageDigests"],
                "folderClassificationsStable": plan_runs[0][
                    "folderClassifications"
                ]
                == plan_runs[1]["folderClassifications"],
                "folderClassificationOracleStable": plan_runs[0][
                    "folderClassificationOracleDigest"
                ]
                == plan_runs[1]["folderClassificationOracleDigest"],
            },
        }

        if not args.skip_deep_analyze and args.deep_limit:
            selection_pool = select_deep_files(
                db_path, corpus_files, args.deep_limit + 1
            )
            if len(selection_pool) < args.deep_limit:
                raise RuntimeError(
                    f"selected only {len(selection_pool)} of {args.deep_limit} Deep Analyze files"
                )
            if len(selection_pool) > args.deep_limit:
                selected = selection_pool[: args.deep_limit]
                partial_selected = selection_pool[args.deep_limit :]
            else:
                selected = selection_pool[:-1]
                partial_selected = selection_pool[-1:]
            if not selected or len(partial_selected) != 1:
                raise RuntimeError(
                    "Deep Analyze validation requires at least two supported files"
                )
            effective_deep_limit = len(selected)
            required_deep_selection = required_deep_labels(
                args.model_kind,
                effective_deep_limit,
                available_deep_labels(db_path, corpus_files),
            )
            selected_ids = [int(item["fileID"]) for item in selected]
            deep_before = deep_db_snapshot(db_path, selected_ids)
            if not deep_snapshot_is_unprocessed(deep_before, selected_ids):
                raise RuntimeError("Deep Analyze selection was not initially unprocessed")
            deep_started = time.monotonic()
            deep_command_elapsed = driver.elapsed()
            driver.send(
                "deep-1",
                {
                    "deepAnalyzeAll": {
                        "modelKind": args.model_kind,
                        "skipExisting": False,
                        "fileIDs": selected_ids,
                        "tagsOnly": False,
                        "proposeRenames": True,
                    }
                },
            )
            deep_event = driver.wait_for(
                "deepAnalyzeComplete",
                command_id="deep-1",
                timeout_seconds=args.deep_timeout_minutes * 60,
                predicate=lambda value: (
                    isinstance(value, dict)
                    and value.get("modelKind") == args.model_kind
                    and not bool(value.get("cancelled", True))
                ),
            )
            deep_fence = settle_command(
                driver, "deep-1", {"deepAnalyzeComplete": 1}
            )
            deep_end = int(deep_fence["eventWindowEnd"])
            complete = inner_payload(deep_event.value, "deepAnalyzeComplete")
            deep_metrics = deep_event_metrics(
                driver.events_between(driver.command_mark("deep-1"), deep_end),
                selected,
                complete,
                deep_command_elapsed,
                args.model_kind,
                effective_deep_limit,
                required_deep_selection,
            )
            deep_metrics["wallSeconds"] = time.monotonic() - deep_started
            deep_metrics["selected"] = selected
            deep_metrics["commandFence"] = deep_fence
            deep_after = deep_db_snapshot(db_path, selected_ids)
            deep_delta = deep_snapshot_delta(deep_before, deep_after)
            deep_metrics["databaseBefore"] = deep_before
            deep_metrics["databaseAfter"] = deep_after
            deep_metrics["databaseDelta"] = deep_delta
            deep_metrics["database"] = deep_db_metrics(
                db_path, selected, args.model_kind
            )
            deep_metrics["database"]["checks"].update(
                {
                    "selectionInitiallyUnprocessed": deep_snapshot_is_unprocessed(
                        deep_before, selected_ids
                    ),
                    "allSelectedRowsChanged": set(deep_delta["changedFileIDs"])
                    == set(selected_ids),
                    "analyzedAtAdvancedForAll": set(
                        deep_delta["analyzedAtAdvancedFileIDs"]
                    )
                    == set(selected_ids),
                }
            )

            skip_before = deep_db_snapshot(db_path, selected_ids)
            skip_started = time.monotonic()
            driver.send(
                "deep-skip",
                {
                    "deepAnalyzeAll": {
                        "modelKind": args.model_kind,
                        "skipExisting": True,
                        "fileIDs": selected_ids,
                        "tagsOnly": False,
                        "proposeRenames": True,
                    }
                },
            )
            skip_event = driver.wait_for(
                "deepAnalyzeComplete",
                command_id="deep-skip",
                timeout_seconds=60,
                predicate=lambda value: (
                    isinstance(value, dict)
                    and value.get("modelKind") == args.model_kind
                ),
            )
            skip_fence = settle_command(
                driver, "deep-skip", {"deepAnalyzeComplete": 1}
            )
            skip_end = int(skip_fence["eventWindowEnd"])
            skip_complete = inner_payload(skip_event.value, "deepAnalyzeComplete")
            skip_events = driver.events_between(
                driver.command_mark("deep-skip"), skip_end
            )
            skip_kinds = [event_kind(item.value) for item in skip_events]
            skip_after = deep_db_snapshot(db_path, selected_ids)
            deep_metrics["skipExisting"] = {
                "wallSeconds": time.monotonic() - skip_started,
                "complete": skip_complete,
                "commandFence": skip_fence,
                "databaseBefore": skip_before,
                "databaseAfter": skip_after,
                "checks": {
                    "processedZero": int(skip_complete.get("processed", -1)) == 0,
                    "failedZero": int(skip_complete.get("failed", -1)) == 0,
                    "notCancelled": not bool(skip_complete.get("cancelled", True)),
                    "modelKindMatches": skip_complete.get("modelKind")
                    == args.model_kind,
                    "noModelLifecycle": "deepAnalyzeStarting" not in skip_kinds,
                    "noProgressEvents": "deepAnalyzeProgress" not in skip_kinds,
                    "noFileDoneEvents": "deepAnalyzeFileDone" not in skip_kinds,
                    "databaseUnchanged": skip_before == skip_after,
                    "nearImmediate": time.monotonic() - skip_started < 10,
                },
            }
            partial_file = partial_selected[0]
            partial_id = int(partial_file["fileID"])
            partial_before = deep_db_snapshot(db_path, [partial_id])
            partial_run = run_deep_command(
                driver,
                command_id="deep-partial-tags",
                wire_model_kind=args.model_kind,
                expected_model_kind=args.model_kind,
                file_ids=[partial_id],
                skip_existing=False,
                tags_only=True,
                propose_renames=False,
                timeout_seconds=args.deep_timeout_minutes * 60,
            )
            partial_after = deep_db_snapshot(db_path, [partial_id])
            partial_delta = deep_snapshot_delta(partial_before, partial_after)
            partial_events = partial_run.pop("events")
            partial_kinds = [event_kind(item.value) for item in partial_events]
            partial_errors = [
                inner_payload(item.value, "error")
                for item in partial_events
                if event_kind(item.value) == "error"
            ]
            partial_complete = partial_run["complete"]

            full_from_partial_before = partial_after
            full_from_partial = run_deep_command(
                driver,
                command_id="deep-partial-full",
                wire_model_kind=args.model_kind,
                expected_model_kind=args.model_kind,
                file_ids=[partial_id],
                skip_existing=True,
                tags_only=False,
                propose_renames=True,
                timeout_seconds=args.deep_timeout_minutes * 60,
            )
            full_from_partial_after = deep_db_snapshot(db_path, [partial_id])
            full_from_partial_delta = deep_snapshot_delta(
                full_from_partial_before, full_from_partial_after
            )
            full_from_partial_events = full_from_partial.pop("events")
            full_from_partial_kinds = [
                event_kind(item.value) for item in full_from_partial_events
            ]
            full_from_partial_errors = [
                inner_payload(item.value, "error")
                for item in full_from_partial_events
                if event_kind(item.value) == "error"
            ]
            full_from_partial_complete = full_from_partial["complete"]

            alias_wire_model = model_alias or args.model_kind
            alias_skip_before = full_from_partial_after
            alias_skip_run = run_deep_command(
                driver,
                command_id="deep-alias-skip",
                wire_model_kind=alias_wire_model,
                expected_model_kind=args.model_kind,
                file_ids=[partial_id],
                skip_existing=True,
                tags_only=False,
                propose_renames=True,
                timeout_seconds=60,
            )
            alias_skip_after = deep_db_snapshot(db_path, [partial_id])
            alias_skip_events = alias_skip_run.pop("events")
            alias_skip_kinds = [
                event_kind(item.value) for item in alias_skip_events
            ]
            alias_skip_complete = alias_skip_run["complete"]

            deep_metrics["partialThenFull"] = {
                "selected": partial_file,
                "partial": {
                    **partial_run,
                    "databaseBefore": partial_before,
                    "databaseAfter": partial_after,
                    "databaseDelta": partial_delta,
                    "eventKinds": partial_kinds,
                    "errors": partial_errors,
                },
                "full": {
                    **full_from_partial,
                    "databaseBefore": full_from_partial_before,
                    "databaseAfter": full_from_partial_after,
                    "databaseDelta": full_from_partial_delta,
                    "eventKinds": full_from_partial_kinds,
                    "errors": full_from_partial_errors,
                },
                "sameModelAliasSkip": {
                    **alias_skip_run,
                    "aliasAvailable": model_alias is not None,
                    "databaseBefore": alias_skip_before,
                    "databaseAfter": alias_skip_after,
                    "eventKinds": alias_skip_kinds,
                },
                "checks": {
                    **deep_partial_full_content_checks(
                        partial_before,
                        partial_after,
                        full_from_partial_after,
                        partial_id,
                        args.model_kind,
                    ),
                    "partialProcessedExactlyOnce": int(
                        partial_complete.get("processed", -1)
                    )
                    == 1,
                    "partialFailedZero": int(partial_complete.get("failed", -1))
                    == 0,
                    "partialModelCanonical": partial_complete.get("modelKind")
                    == args.model_kind,
                    "partialFileDoneExactlyOnce": partial_kinds.count(
                        "deepAnalyzeFileDone"
                    )
                    == 1,
                    "partialNoErrors": not partial_errors,
                    "partialChangedSelectedFile": partial_delta["changedFileIDs"]
                    == [partial_id],
                    "legacyNullReranExactlyOnce": int(
                        full_from_partial_complete.get("processed", -1)
                    )
                    == 1,
                    "fullFailedZero": int(
                        full_from_partial_complete.get("failed", -1)
                    )
                    == 0,
                    "fullModelCanonical": full_from_partial_complete.get("modelKind")
                    == args.model_kind,
                    "fullFileDoneExactlyOnce": full_from_partial_kinds.count(
                        "deepAnalyzeFileDone"
                    )
                    == 1,
                    "fullNoErrors": not full_from_partial_errors,
                    "fullChangedSelectedFile": full_from_partial_delta[
                        "changedFileIDs"
                    ]
                    == [partial_id],
                    "fullAnalyzedAtAdvanced": full_from_partial_delta[
                        "analyzedAtAdvancedFileIDs"
                    ]
                    == [partial_id],
                    "aliasWireDistinctWhenAvailable": model_alias is None
                    or model_alias != args.model_kind,
                    "aliasEventCanonical": alias_skip_complete.get("modelKind")
                    == args.model_kind,
                    "sameModelAliasSkipped": int(
                        alias_skip_complete.get("processed", -1)
                    )
                    == 0
                    and int(alias_skip_complete.get("failed", -1)) == 0,
                    "aliasSkipNoLifecycle": "deepAnalyzeStarting"
                    not in alias_skip_kinds
                    and "deepAnalyzeProgress" not in alias_skip_kinds
                    and "deepAnalyzeFileDone" not in alias_skip_kinds,
                    "aliasSkipDatabaseUnchanged": alias_skip_before
                    == alias_skip_after,
                },
            }
            stl = select_unsupported_stl(
                db_path,
                corpus_files,
                {int(item["fileID"]) for item in selected} | {partial_id},
            )
            stl_id = int(stl["fileID"])
            stl_before = deep_db_snapshot(db_path, [stl_id])
            driver.send(
                "deep-unsupported-stl",
                {
                    "deepAnalyzeAll": {
                        "modelKind": args.model_kind,
                        "skipExisting": False,
                        "fileIDs": [stl_id],
                        "tagsOnly": False,
                        "proposeRenames": True,
                    }
                },
            )
            stl_error_event = driver.wait_for(
                "error",
                command_id="deep-unsupported-stl",
                timeout_seconds=30,
                predicate=lambda value: (
                    isinstance(value, dict)
                    and value.get("kind") == "deep_analyze_no_supported_files"
                ),
            )
            stl_complete_event = driver.wait_for(
                "deepAnalyzeComplete",
                after=stl_error_event.sequence + 1,
                timeout_seconds=30,
                predicate=lambda value: (
                    isinstance(value, dict)
                    and value.get("modelKind") == args.model_kind
                ),
            )
            stl_fence = settle_command(
                driver,
                "deep-unsupported-stl",
                {"error": 1, "deepAnalyzeComplete": 1},
            )
            stl_end = int(stl_fence["eventWindowEnd"])
            stl_events = driver.events_between(
                driver.command_mark("deep-unsupported-stl"), stl_end
            )
            stl_errors = [
                inner_payload(item.value, "error")
                for item in stl_events
                if event_kind(item.value) == "error"
            ]
            stl_done = [
                inner_payload(item.value, "deepAnalyzeFileDone")
                for item in stl_events
                if event_kind(item.value) == "deepAnalyzeFileDone"
            ]
            stl_terminals = [
                inner_payload(item.value, "deepAnalyzeComplete")
                for item in stl_events
                if event_kind(item.value) == "deepAnalyzeComplete"
            ]
            stl_complete = inner_payload(
                stl_complete_event.value, "deepAnalyzeComplete"
            )
            expected_stl_message = (
                "None of the selected files can be analyzed. Select an image, "
                "video, document, audio file, PDF, or OBJ model and try again."
            )
            stl_after = deep_db_snapshot(db_path, [stl_id])
            deep_metrics["unsupportedStl"] = {
                "selected": stl,
                "fixtureSource": (
                    "missing-id-fallback"
                    if stl.get("syntheticMissingIDFallback")
                    else "corpus"
                ),
                "error": inner_payload(stl_error_event.value, "error"),
                "complete": stl_complete,
                "commandFence": stl_fence,
                "databaseBefore": stl_before,
                "databaseAfter": stl_after,
                "checks": {
                    "exactlyOneTypedError": len(stl_errors) == 1
                    and stl_errors[0].get("kind")
                    == "deep_analyze_no_supported_files"
                    and stl_errors[0].get("message") == expected_stl_message,
                    "exactlyOneTerminal": len(stl_terminals) == 1,
                    "processedZero": int(stl_complete.get("processed", -1)) == 0,
                    "failedZero": int(stl_complete.get("failed", -1)) == 0,
                    "notCancelled": not bool(stl_complete.get("cancelled", True)),
                    "modelKindMatches": stl_complete.get("modelKind")
                    == args.model_kind,
                    "noStarting": not any(
                        event_kind(item.value) == "deepAnalyzeStarting"
                        for item in stl_events
                    ),
                    "noProgress": not any(
                        event_kind(item.value) == "deepAnalyzeProgress"
                        for item in stl_events
                    ),
                    "noFileDone": not stl_done,
                    "databaseUnchanged": stl_before == stl_after,
                },
            }
            if args.deep_cancel_limit:
                cancel_selected = select_deep_files(
                        db_path,
                        corpus_files,
                        args.deep_cancel_limit,
                        {int(item["fileID"]) for item in selected} | {partial_id},
                    )
                if len(cancel_selected) != args.deep_cancel_limit:
                    raise RuntimeError(
                        f"selected only {len(cancel_selected)} of "
                        f"{args.deep_cancel_limit} Deep Analyze cancellation files"
                    )
                cancel_ids = [int(item["fileID"]) for item in cancel_selected]
                cancel_before = deep_db_snapshot(db_path, cancel_ids)
                if not deep_snapshot_is_unprocessed(cancel_before, cancel_ids):
                    raise RuntimeError(
                        "Deep Analyze cancellation selection was not unprocessed"
                    )
                cancel_started = time.monotonic()
                driver.send(
                    "deep-cancel-run",
                    {
                        "deepAnalyzeAll": {
                            "modelKind": args.model_kind,
                            "skipExisting": False,
                            "fileIDs": cancel_ids,
                            "tagsOnly": False,
                            "proposeRenames": True,
                        }
                    },
                )
                starting_event = driver.wait_for(
                    "deepAnalyzeStarting",
                    command_id="deep-cancel-run",
                    timeout_seconds=60,
                    predicate=lambda value: (
                        isinstance(value, dict)
                        and value.get("modelKind") == args.model_kind
                        and value.get("phase") in {"loadingModel", "resolvingTargets"}
                    ),
                )
                driver.send("deep-cancel-request", {"deepAnalyzeCancel": {}})
                cancel_event = driver.wait_for(
                    "deepAnalyzeComplete",
                    command_id="deep-cancel-run",
                    timeout_seconds=60,
                    predicate=lambda value: (
                        isinstance(value, dict)
                        and value.get("modelKind") == args.model_kind
                        and bool(value.get("cancelled", False))
                    ),
                )
                cancel_fence = settle_command(
                    driver, "deep-cancel-run", {"deepAnalyzeComplete": 1}
                )
                cancel_end = int(cancel_fence["eventWindowEnd"])
                cancel_complete = inner_payload(
                    cancel_event.value, "deepAnalyzeComplete"
                )
                cancel_events = driver.events_between(
                    driver.command_mark("deep-cancel-run"), cancel_end
                )
                cancel_error_events = [
                    inner_payload(item.value, "error")
                    for item in cancel_events
                    if event_kind(item.value) == "error"
                ]
                cancel_terminals = sum(
                    event_kind(item.value) == "deepAnalyzeComplete"
                    for item in cancel_events
                )
                cancel_after = deep_db_snapshot(db_path, cancel_ids)
                cancel_delta = deep_snapshot_delta(cancel_before, cancel_after)

                cancel_done = [
                    inner_payload(item.value, "deepAnalyzeFileDone")
                    for item in cancel_events
                    if event_kind(item.value) == "deepAnalyzeFileDone"
                ]
                cancel_done_ids = [
                    int(item.get("fileID", -1)) for item in cancel_done
                ]
                cancel_progress = [
                    inner_payload(item.value, "deepAnalyzeProgress")
                    for item in cancel_events
                    if event_kind(item.value) == "deepAnalyzeProgress"
                ]
                deep_metrics["cancellation"] = {
                    "selected": cancel_selected,
                    "starting": inner_payload(
                        starting_event.value, "deepAnalyzeStarting"
                    ),
                    "complete": cancel_complete,
                    "wallSeconds": time.monotonic() - cancel_started,
                    "databaseBefore": cancel_before,
                    "databaseAfter": cancel_after,
                    "databaseDelta": cancel_delta,
                    "fileDone": cancel_done,
                    "progress": cancel_progress,
                    "commandFence": cancel_fence,
                    "errorEvents": cancel_error_events,
                    "checks": {
                        "distinctUnprocessedSelection": all(
                                row.get("vlm_model") is None
                                and row.get("vlm_full_model") is None
                                for row in cancel_before["rows"]
                        ),
                        "cancelledTerminalExactlyOnce": cancel_terminals == 1,
                        "cancelledTrue": bool(cancel_complete.get("cancelled", False)),
                        "modelKindMatches": cancel_complete.get("modelKind")
                        == args.model_kind,
                        "failedZero": int(cancel_complete.get("failed", -1)) == 0,
                        "cancelledBeforeAllFiles": int(
                            cancel_complete.get("processed", -1)
                        )
                        < len(cancel_ids),
                        "fileDoneCountMatchesProcessed": len(cancel_done_ids)
                        == int(cancel_complete.get("processed", -1)),
                        "fileDoneIDsUniqueAndSelected": len(cancel_done_ids)
                        == len(set(cancel_done_ids))
                        and set(cancel_done_ids).issubset(set(cancel_ids)),
                        "databaseChangesMatchCompletedFiles": set(
                            cancel_delta["changedFileIDs"]
                        )
                        == set(cancel_done_ids),
                        "unprocessedRowsRemainUnchanged": bool(
                            cancel_delta["unchangedFileIDs"]
                        ),
                        "progressBounded": all(
                            0 <= int(item.get("processed", -1)) < len(cancel_ids)
                            and int(item.get("total", -1)) == len(cancel_ids)
                            and item.get("modelKind") == args.model_kind
                            for item in cancel_progress
                        ),
                        "noErrorEvents": not cancel_error_events,
                        "boundedCompletion": time.monotonic() - cancel_started < 60,
                        "healthRecovered": bool(
                            cancel_fence.get("healthBarrier", {}).get("requestID")
                        ),
                    },
                }
            summary["deepAnalyze"] = deep_metrics

        shutdown_mark = driver.mark()
        driver.send("shutdown-1", {"shutdown": {}})
        exit_code = driver.stop(30)
        summary["shutdown"] = {
            "commandEventOffset": shutdown_mark,
            "exitCode": exit_code,
            "cleanExit": exit_code == 0,
        }
    except BaseException as error:
        run_error = f"{type(error).__name__}: {error}"
        if driver.process is not None and driver.process.poll() is None:
            try:
                driver.send("shutdown-error", {"shutdown": {}})
                exit_code = driver.stop(15)
            except BaseException:
                exit_code = driver.force_stop()
        else:
            driver.force_stop()
        summary["runError"] = run_error
        summary["shutdown"] = {
            "exitCode": exit_code,
            "cleanExit": exit_code == 0,
        }
    finally:
        if driver.process is not None and driver.process.poll() is None:
            driver.force_stop()
        if monitor is not None:
            monitor.stop()
            summary["resources"] = monitor.summary()
        else:
            summary["resources"] = {
                "started": False,
                "checks": {"monitorStarted": False},
            }
        summary["resources"]["jobObject"] = driver.job_summary()

    raw_error_events = [
        inner_payload(item.value, "error")
        for item in driver.events
        if event_kind(item.value) == "error"
    ]
    malformed_error_events = [
        repr(event) for event in raw_error_events if not isinstance(event, dict)
    ]
    error_events = [
        event for event in raw_error_events if isinstance(event, dict)
    ]
    expected_error_counts: collections.Counter[str] = collections.Counter()
    if not args.skip_restructure:
        expected_error_counts["plan_restructure_cancelled"] = 1
    if not args.skip_deep_analyze and args.deep_limit:
        expected_error_counts["deep_analyze_no_supported_files"] = 1
    observed_error_counts = collections.Counter(
        str(event.get("kind", "")) for event in error_events
    )
    expected_typed_error_events = [
        event
        for event in error_events
        if event.get("kind") in expected_error_counts
    ]
    unexpected_error_events = [
        event
        for event in error_events
        if event.get("kind") not in expected_error_counts
    ]
    bad_stderr = [line for line in driver.stderr_lines if BAD_LOG_MARKER.search(line)]
    state_logs = scan_state_logs(app_root / "logs")
    command_kinds = [str(command["kind"]) for command in driver.commands]
    summary["logs"] = {
        "events": len(driver.events),
        "invalidStdoutLines": driver.invalid_stdout[:100],
        "protocolErrors": driver.protocol_errors[:100],
        "stderrLines": len(driver.stderr_lines),
        "errorEvents": error_events,
        "malformedErrorEvents": malformed_error_events,
        "expectedTypedErrorEvents": expected_typed_error_events,
        "expectedTypedErrorCounts": dict(expected_error_counts),
        "observedTypedErrorCounts": dict(observed_error_counts),
        "unexpectedErrorEvents": unexpected_error_events,
        "badStderrMarkers": bad_stderr[:100],
        "readerState": {
            "stdoutEOF": driver.stdout_eof,
            "stderrEOF": driver.stderr_eof,
            "stdoutError": driver.stdout_reader_error,
            "stderrError": driver.stderr_reader_error,
            "threadsAlive": [
                thread.name for thread in driver._threads if thread.is_alive()
            ],
        },
        "commands": driver.commands,
        "commandKinds": command_kinds,
        "stateLogs": state_logs,
        "checks": {
            "stdoutAllJSON": not driver.invalid_stdout,
            "protocolErrorsEmpty": not driver.protocol_errors,
            "typedErrorCountsExact": observed_error_counts
            == expected_error_counts,
            "typedErrorPayloadsWellFormed": not malformed_error_events,
            "noUnexpectedErrorEvents": not unexpected_error_events,
            "noErrorOrPanicMarkers": not bad_stderr,
            "stdoutReaderHealthy": driver.stdout_reader_error is None,
            "stderrReaderHealthy": driver.stderr_reader_error is None,
            "readerThreadsStopped": not any(
                thread.is_alive() for thread in driver._threads
            ),
            "stdoutEOFObservedAfterCleanShutdown": exit_code == 0
            and driver.stdout_eof,
            "stderrEOFObservedAfterCleanShutdown": exit_code == 0
            and driver.stderr_eof,
            "commandKindsWithinWhitelist": all(
                kind in HARNESS_COMMAND_KINDS for kind in command_kinds
            ),
            "noMutatingCommands": not any(
                kind in FORBIDDEN_MUTATING_COMMAND_KINDS for kind in command_kinds
            ),
        },
    }

    source_models_after_run: dict[str, Any] | None = None
    isolated_models_after_run: dict[str, Any] | None = None
    source_engine_after_run: dict[str, Any] | None = None
    isolated_engine_after_run: dict[str, Any] | None = None
    source_engine_runtime_after_run: dict[str, dict[str, Any]] | None = None
    isolated_engine_runtime_after_run: dict[str, dict[str, Any]] | None = None
    source_face_crops_after_run: FaceCropInventory | None = None
    seed_after: dict[str, Any] | None = None
    wal_final: dict[str, Any] | None = None
    shm_final: dict[str, Any] | None = None
    journal_final: dict[str, Any] | None = None
    after_fingerprint: dict[str, Any] | None = None
    after_corpus_files: set[str] | None = None
    after_corpus_directories: set[str] | None = None
    after_corpus_index_errors: list[str] | None = None
    final_database_integrity: dict[str, Any] | None = None
    post_run_safety_errors: list[str] = []
    post_guard = FileIDProcessGuard("post-run source verification")
    post_guard_started = False
    try:
        post_guard.start()
        post_guard_started = True
        source_models_after_run = full_tree_manifest(source_models)
        isolated_models_after_run = full_tree_manifest(models)
        source_engine_after_run = file_snapshot(source_engine)
        isolated_engine_after_run = file_snapshot(engine)
        source_engine_runtime_after_run = {
            path.name: file_snapshot(path) for path in source_engine_runtime_files
        }
        isolated_engine_runtime_after_run = {
            path.name: file_snapshot(engine_directory / path.name)
            for path in source_engine_runtime_files
        }
        source_face_crops_after_run = (
            collect_face_crop_inventory(source_face_crops)
            if source_face_crops is not None
            else None
        )
        seed_after = {
            "path": str(seed_db),
            "size": seed_db.stat().st_size,
            "modifiedNS": seed_db.stat().st_mtime_ns,
            "sha256": sha256_file(seed_db),
        }
        wal_final = file_snapshot(wal_path)
        shm_final = file_snapshot(shm_path)
        journal_final = file_snapshot(journal_path)
        (
            after_corpus_files,
            after_corpus_directories,
            after_corpus_index_errors,
        ) = corpus_index(corpus)
        if after_corpus_index_errors:
            raise RuntimeError(
                "post-run corpus traversal is not safe: "
                + "; ".join(after_corpus_index_errors[:20])
            )
        after_fingerprint = safe_tree_fingerprint(
            corpus, args.fingerprint_samples, args.max_sample_bytes
        )
        final_database_integrity = sqlite_integrity_snapshot(
            db_path, immutable=False
        )
    except BaseException as error:
        post_run_safety_errors.append(f"{type(error).__name__}: {error}")
    finally:
        if post_guard_started:
            try:
                post_guard.stop_and_assert_clean()
            except BaseException as error:
                post_run_safety_errors.append(f"{type(error).__name__}: {error}")

    if after_fingerprint is None:
        after_fingerprint = {
            "errors": ["post-run fingerprint was not completed under the process guard"],
            "contentSamples": [],
        }
    write_json(artifacts / "corpus-after.json", after_fingerprint)
    after_fingerprint_errors = list(after_fingerprint.get("errors") or [])
    after_fingerprint_samples = list(after_fingerprint.get("contentSamples") or [])
    corpus_unchanged = (
        not before_fingerprint.get("errors")
        and not after_fingerprint_errors
        and bool(before_fingerprint.get("contentSamples"))
        and bool(after_fingerprint_samples)
        and stable_fingerprint(before_fingerprint)
        == stable_fingerprint(after_fingerprint)
    )
    summary["safety"] = {
        "corpusBeforeEqualsAfter": corpus_unchanged,
        "seedBeforeEqualsAfter": seed_before == seed_after,
        "seedAfter": seed_after,
        "seedSidecarsAfter": {
            "wal": wal_final,
            "shm": shm_final,
            "journal": journal_final,
        },
        "sourceModelsAfterRun": source_models_after_run,
        "isolatedModelsAfterRun": isolated_models_after_run,
        "sourceEngineAfterRun": source_engine_after_run,
        "isolatedEngineAfterRun": isolated_engine_after_run,
        "sourceEngineRuntimeAfterRun": source_engine_runtime_after_run,
        "isolatedEngineRuntimeAfterRun": isolated_engine_runtime_after_run,
        "sourceFaceCropsAfterRun": (
            source_face_crops_after_run.summary()
            if source_face_crops_after_run is not None
            else None
        ),
        "postRunFileIDProcessSnapshotSamples": post_guard.samples,
        "postRunSafetyErrors": post_run_safety_errors,
        "afterCorpusIndexErrors": after_corpus_index_errors,
        "afterFingerprintErrors": after_fingerprint_errors,
        "afterFingerprintSampleCount": len(after_fingerprint_samples),
        "checks": {
            "corpusUnchanged": corpus_unchanged,
            "corpusFileIndexUnchanged": after_corpus_files == corpus_files,
            "corpusDirectoryIndexUnchanged": after_corpus_directories
            == corpus_directories,
            "afterCorpusIndexErrorFree": after_corpus_index_errors == [],
            "afterFingerprintErrorFree": not after_fingerprint_errors,
            "afterFingerprintHasSamples": bool(after_fingerprint_samples),
            "seedUnchanged": seed_before == seed_after,
            "seedWalUnchanged": wal_before == wal_final,
            "seedShmUnchanged": shm_before == shm_final,
            "seedJournalUnchanged": journal_before == journal_final,
            "sourceModelsUnchanged": source_models_before
            == source_models_after_run,
            "isolatedModelsUnchanged": isolated_models_manifest
            == isolated_models_after_run,
            "sourceEngineUnchanged": source_engine_before
            == source_engine_after_run,
            "isolatedEngineUnchanged": isolated_engine_snapshot
            == isolated_engine_after_run,
            "sourceEngineRuntimeUnchanged": source_engine_runtime_before
            == source_engine_runtime_after_run,
            "isolatedEngineRuntimeUnchanged": isolated_engine_runtime
            == isolated_engine_runtime_after_run,
            "sourceFaceCropMembershipUnchanged": source_face_crops_before
            == source_face_crops_after_run,
            "postRunSafetyCompleted": not post_run_safety_errors,
            "cloneSeparateFromSeed": normalized(db_path) != normalized(seed_db),
            "stateOutsideCorpus": not under_root(state, corpus),
            "artifactsOutsideCorpus": not under_root(artifacts, corpus),
            "engineOutsideCorpus": not under_root(engine, corpus),
            "modelsOutsideCorpus": not under_root(models, corpus),
            "databaseOutsideCorpus": not under_root(db_path, corpus),
            "workingDirectoryOutsideCorpus": not under_root(state, corpus),
            "runtimeTempOutsideCorpus": not under_root(runtime_temp, corpus),
            "engineInsideIsolatedState": under_root(engine, state),
            "modelsIsolationPolicySatisfied": (
                normalized(models) == normalized(source_models)
                if args.reuse_models_in_place
                else under_root(models, state)
            ),
            "databaseInsideIsolatedState": under_root(db_path, state),
            "runtimeTempInsideIsolatedState": under_root(runtime_temp, state),
            "ortInsideIsolatedRuntime": ort_dylib_path is None
            or under_root(ort_dylib_path, models)
            or ort_dylib_path.parent == engine_directory,
            "sourceModelsOutsideCorpus": not under_root(source_models, corpus),
            "sourceEngineOutsideCorpus": not under_root(source_engine, corpus),
            "seedOutsideCorpus": not under_root(seed_db, corpus),
        },
    }
    summary["databaseIntegrityAfterRun"] = final_database_integrity or {
        "checks": {
            "integritySnapshotCompleted": False,
        }
    }
    summary["finishedAt"] = utc_now()
    summary["failedChecks"] = all_checks(summary)
    summary["result"] = (
        "GREEN"
        if run_error is None
        and exit_code == 0
        and not summary["failedChecks"]
        else "RED"
    )
    job_checks = (
        summary.get("resources", {})
        .get("jobObject", {})
        .get("checks")
    )
    _RETENTION_DELETE_APPROVED = (
        summary["result"] == "GREEN"
        and isinstance(job_checks, dict)
        and bool(job_checks)
        and all(value is True for value in job_checks.values())
    )
    write_json(artifacts / "summary.json", summary)
    cleanup_error = cleanup_validation_state()
    record_retention_result(cleanup_error)
    if cleanup_error is not None:
        raise RuntimeError(f"isolated validation state cleanup failed: {cleanup_error}")
    summary = json.loads((artifacts / "summary.json").read_text(encoding="utf-8"))
    print(json.dumps(
        {
                "result": summary["result"],
                "sourceRoot": str(corpus),
                "stateDirectory": str(state),
                "stateRetention": _RETENTION_CLEANUP_DISPOSITION,
                "fileIDDB": str(db_path),
            "modelsDirectory": str(models),
            "artifacts": str(artifacts),
            "failedChecks": summary["failedChecks"],
            "runError": run_error,
        },
        indent=2,
    ))
    return 0 if summary["result"] == "GREEN" else 1


def main() -> int:
    return run_validation()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, sqlite3.Error) as error:
        print(f"ENV-FAIL: {error}", file=sys.stderr)
        raise SystemExit(2)
