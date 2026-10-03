#!/usr/bin/env python3
"""Isolated FileID IPC scan benchmark and independent read-only volume sample."""

import argparse
import hashlib
from contextlib import closing
import json
import os
from pathlib import Path
import re
import queue
import shutil
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib

REPO = Path(__file__).resolve().parents[2]
TRUTH = REPO / "shared" / "test-corpus" / "assertions.json"


def linux_mount(path):
    resolved = Path(path).resolve()
    best = None
    with open("/proc/self/mountinfo", encoding="utf-8") as mounts:
        for line in mounts:
            before, separator, after = line.partition(" - ")
            if not separator:
                continue
            fields = before.split()
            mount = Path(re.sub(r"\\([0-7]{3})", lambda m: chr(int(m[1], 8)), fields[4]))
            if resolved != mount and mount not in resolved.parents:
                continue
            if best is None or len(str(mount)) > len(str(best[0])):
                best = (mount, after.split()[0])
    if best is None:
        raise OSError(f"Cannot identify mount for {resolved}")
    return best


def volume(path):
    resolved = Path(path).resolve()
    if os.name == "nt":
        return os.path.normcase(os.path.splitdrive(str(resolved))[0])
    existing = resolved
    while not existing.exists():
        existing = existing.parent
    return existing.stat().st_dev


def volume_label(path):
    if os.name == "nt":
        import ctypes

        label = ctypes.create_unicode_buffer(261)
        if not ctypes.windll.kernel32.GetVolumeInformationW(
            volume(path) + "\\", label, len(label), None, None, None, None, 0
        ):
            raise OSError(f"Cannot identify volume for {path}")
        return label.value
    labels = Path("/dev/disk/by-label")
    if labels.is_dir():
        for link in labels.iterdir():
            if link.stat().st_rdev == volume(path):
                return link.name
    return None


def require_safe(path, forbidden=None, writable=True):
    label = volume_label(path)
    if label and label.casefold() == "adlon":
        raise ValueError(f"Adlon volume prohibited for benchmark: {path}")
    if forbidden is not None and volume(path) == volume(forbidden):
        raise ValueError(f"Path must be on a different volume from {forbidden}: {path}")
    if os.name != "nt" and writable:
        mount, filesystem = linux_mount(path)
        if mount != Path("/") and filesystem not in ("tmpfs", "overlay") and label is None:
            raise ValueError(f"Unknown external mount prohibited for writable benchmark path: {path}")


def require_plain_models(root):
    if not root.exists():
        return
    if root.is_symlink():
        raise ValueError("Model directory is a symlink; refusing engine launch")
    pending = [root]
    while pending:
        with os.scandir(pending.pop()) as children:
            for entry in children:
                info = entry.stat(follow_symlinks=False)
                if entry.is_symlink() or (getattr(info, "st_file_attributes", 0) & 0x400):
                    raise ValueError("Model directory contains a junction or symlink; refusing engine launch")
                if entry.is_dir(follow_symlinks=False):
                    pending.append(entry.path)


def png(seed):
    width = height = 96
    pixels = b"".join(
        b"\0" + b"".join(bytes(((x * 3 + seed * 29) % 256,
                                    (y * 5 + seed * 11) % 256,
                                    (x ^ y ^ seed) % 256)) for x in range(width))
        for y in range(height)
    )

    def chunk(name, data):
        return struct.pack(">I", len(data)) + name + data + struct.pack(">I", zlib.crc32(name + data))

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">2I5B", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b""))


def make_library(root, count):
    truth = {}
    for i in range(count):
        if i % 2:
            name, kind = f"document_{i:04d}.txt", "doc"
            (root / name).write_text(f"FileID benchmark document {i} containing prism-{i:04d}.\n", encoding="utf-8")
        else:
            name, kind = f"photo_{i:04d}.png", "image"
            (root / name).write_bytes(png(i))
        truth[str(root / name)] = kind
    assertions = json.loads(TRUTH.read_text(encoding="utf-8"))
    source = REPO / "shared" / "test-corpus" / assertions["ocr"]["file"]
    target = root / "known-text.png"
    shutil.copyfile(source, target)
    truth[str(target)] = "image"
    return truth, [token.casefold() for token in assertions["ocr"]["mustContainTokens"]]


def sample_drive(path, max_entries, max_dirs):
    root = Path(path)
    if os.name == "nt":
        if not root.drive or os.path.normcase(str(root)) != os.path.normcase(root.anchor):
            raise ValueError("--sample-drive must be a volume root such as H:\\")
    else:
        root = root.resolve()
        if root != linux_mount(root)[0]:
            raise ValueError("--sample-drive must be a mounted filesystem root")
    if not root.is_dir():
        raise ValueError(f"Volume root unavailable: {root}")
    label = volume_label(root)
    started = time.perf_counter()
    folders = [root]
    scanned_dirs = files = directories = entries = errors = 0
    while folders and scanned_dirs < max_dirs and entries < max_entries:
        folder = folders.pop(0)
        scanned_dirs += 1
        try:
            with os.scandir(folder) as children:
                for entry in children:
                    if entries >= max_entries:
                        break
                    entries += 1
                    try:
                        entry.stat(follow_symlinks=False)
                        if entry.is_dir(follow_symlinks=False):
                            directories += 1
                            if scanned_dirs + len(folders) < max_dirs:
                                folders.append(Path(entry.path))
                        elif entry.is_file(follow_symlinks=False):
                            files += 1
                    except OSError:
                        errors += 1
        except OSError:
            errors += 1
    return {"mode": "read_only_metadata_sample", "volume_label": label,
            "drive": str(root), "entries": entries, "files": files,
            "directories": directories, "dirs_visited": scanned_dirs,
            "stat_errors": errors, "seconds": round(time.perf_counter() - started, 3),
            "bounded": bool(folders or entries >= max_entries or (scanned_dirs >= max_dirs and directories >= max_dirs)),
            "engine_invoked": False}


def event_reader(stream, events):
    for line in stream:
        try:
            events.put(json.loads(line))
        except json.JSONDecodeError:
            events.put({"invalidFrame": line[:200]})
    events.put({"eof": True})


def await_event(events, deadline, wanted):
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("Engine IPC deadline exceeded")
        try:
            item = events.get(timeout=remaining)
        except queue.Empty as exc:
            raise TimeoutError("Engine IPC deadline exceeded") from exc
        if "eof" in item:
            raise RuntimeError("Engine exited before terminal IPC event")
        if "invalidFrame" in item:
            raise RuntimeError(f"Invalid engine IPC: {item['invalidFrame']}")
        payload = item.get("payload", {})
        if not isinstance(payload, dict):
            continue
        for name in wanted:
            if name in payload:
                body = payload[name]
                return name, body.get("_0", body) if isinstance(body, dict) else body


def quality(db_path, truth, ocr_tokens):
    uri = db_path.as_uri() + "?mode=ro&immutable=1"
    with closing(sqlite3.connect(uri, uri=True)) as db:
        rows = db.execute("SELECT path_text, kind, failed FROM files").fetchall()
        observed = {path: (kind, bool(failed)) for path, kind, failed in rows if path in truth}
        confusion = {}
        for path, expected in truth.items():
            actual = observed.get(path, ("missing", True))[0]
            confusion[f"{expected}->{actual}"] = confusion.get(f"{expected}->{actual}", 0) + 1
        doc_hits = 0
        for path in truth:
            if path.endswith(".txt"):
                document = db.execute("SELECT d.text FROM doc_text d JOIN files f ON f.id=d.file_id WHERE f.path_text=?", (path,)).fetchone()
                if document and f"prism-{int(Path(path).stem.split('_')[-1]):04d}" in document[0]:
                    doc_hits += 1
        ocr = db.execute("SELECT o.text FROM ocr_text o JOIN files f ON f.id=o.file_id WHERE f.path_text=?",
                         (next(path for path in truth if path.endswith("known-text.png")),)).fetchone()
    found = [token for token in ocr_tokens if ocr and token in re.findall(r"\w+", ocr[0].casefold())]
    return {"expected_files": len(truth), "catalogued": len(observed),
            "failed": sum(failed for _, failed in observed.values()),
            "kind_correct": sum(observed.get(path, (None,))[0] == kind for path, kind in truth.items()),
            "kind_confusion": confusion, "document_text_hits": doc_hits,
            "document_count": sum(kind == "doc" for kind in truth.values()),
            "ocr_label_recall": len(found) / len(ocr_tokens), "ocr_tokens_found": found,
            "ocr_tokens_expected": ocr_tokens,
            "precision": None, "precision_note": "No labeled negative OCR/AI examples in shared corpus"}


def model_identity(models):
    names = {
        "RAM++": "ram_plus/ram_plus.onnx",
        "CLIP ViT-B/32": "mobileclip/mobileclip_s2_image.onnx",
        "YuNet": "yunet/face_detection_yunet_2023mar.onnx",
        "SFace": "sface/face_recognition_sface_2021dec.onnx",
    }
    identities = {}
    for name, relative in names.items():
        path = models / relative
        if not path.is_file():
            continue
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        identities[name] = {"sha256": digest.hexdigest(), "bytes": path.stat().st_size}
    return identities


def run_engine(engine, models, temp_root, count, timeout, forbidden=None):
    library = temp_root / "library"
    state = temp_root / "state"
    require_safe(library, forbidden)
    require_safe(state, forbidden)
    library.mkdir()
    truth, tokens = make_library(library, count)
    env = dict(os.environ, FILEID_MODELS_DIR=str(models), FILEID_TEST_FILE_CAP="0")
    env["LOCALAPPDATA" if os.name == "nt" else "XDG_DATA_HOME"] = str(state)
    # No engine command is ever sent a real-volume path. All persistent state
    # (DB, logs, thumbnails, face crops) is redirected under this temp root.
    proc = subprocess.Popen([str(engine)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True, encoding="utf-8", env=env,
                            cwd=str(temp_root))
    events = queue.Queue()
    reader = threading.Thread(target=event_reader, args=(proc.stdout, events), daemon=True)
    reader.start()
    try:
        ready_deadline = time.monotonic() + min(timeout, 90)
        name, body = await_event(events, ready_deadline, ("ready", "error"))
        if name == "error":
            return {"mode": "engine_unavailable", "reason": body}
        hardware = body.get("hardware") or {}
        environment = {"engine_version": body.get("version"), "worker_cap": body.get("workerCap"),
                       "gpu_vendor": hardware.get("gpuVendor"), "adapter": hardware.get("adapterName"),
                       "execution_provider": hardware.get("executionProvider"),
                       "physical_cpu_cores": hardware.get("physicalCpuCores"),
                       "ram_total_mb": hardware.get("ramTotalMB"),
                       "model_identity": model_identity(models)}
        command = {"id": "bench-scan", "payload": {"startScan": {"rootPath": str(library), "rescan": False}}}
        started = time.perf_counter()
        proc.stdin.write(json.dumps(command) + "\n")
        proc.stdin.flush()
        deadline = time.monotonic() + timeout
        scan_failed = False
        scan_completed = False
        while True:
            name, body = await_event(events, deadline, ("scanComplete", "phaseChanged", "error"))
            if name == "error":
                return {"mode": "model_unavailable" if body.get("kind") in ("models_not_installed", "model_load_failed", "model_load_timeout") else "engine_failed",
                        "reason": body, "environment": environment,
                        "scan_attempt_seconds": round(time.perf_counter() - started, 3)}
            if name == "phaseChanged":
                if body == "cancelled":
                    return {"mode": "engine_failed", "phase": body, "environment": environment}
                scan_failed = scan_failed or body == "failed"
                scan_completed = scan_completed or body == "completed"
            if name == "scanComplete":
                if scan_failed or not scan_completed:
                    return {"mode": "engine_failed", "reason": {"kind": "incomplete_scan",
                            "message": "scanComplete arrived without a successful completed phase"},
                            "environment": environment}
                wall = time.perf_counter() - started
                break
        proc.stdin.write('{"id":"bench-stop","payload":{"shutdown":{}}}\n')
        proc.stdin.flush()
        proc.stdin.close()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError("Engine failed to shut down") from exc
        if proc.returncode != 0:
            raise RuntimeError(f"Engine shutdown exit code {proc.returncode}")
        measured = quality(temp_root / "state" / "FileID" / "fileid.sqlite", truth, tokens)
        scan_seconds = body["totalSeconds"]
        return {"mode": "engine_scan", "model_loaded": True, "environment": environment,
                "ipc": body, "wall_seconds_including_model_load": round(wall, 3),
                "files_per_second": round(body["processedFiles"] / scan_seconds, 3) if scan_seconds > 0 else None,
                "quality": measured}
    except TimeoutError as exc:
        return {"mode": "engine_failed", "reason": {"kind": "timeout", "message": str(exc)}}
    except RuntimeError as exc:
        return {"mode": "engine_failed", "reason": {"kind": "engine_error", "message": str(exc)}}
    except sqlite3.Error as exc:
        return {"mode": "engine_failed", "reason": {"kind": "quality_db_error", "message": str(exc)}}
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=15)
        reader.join(timeout=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", type=Path, help="FileIDEngine executable for real IPC scan")
    parser.add_argument("--models", type=Path, help="Existing models folder, default user's local FileID Models")
    parser.add_argument("--files", type=int, default=8, help="Synthetic files (plus one labeled OCR fixture)")
    parser.add_argument("--timeout", type=int, default=300, help="Scan deadline in seconds")
    parser.add_argument("--sample-drive", help="Read-only metadata sample of a volume root (never engine scanned)")
    parser.add_argument("--max-entries", type=int, default=1000)
    parser.add_argument("--max-dirs", type=int, default=25)
    args = parser.parse_args()
    if args.files < 2 or args.timeout < 1 or args.max_entries < 1 or args.max_dirs < 1:
        parser.error("--files >= 2 and other limits >= 1 are required")
    if not args.engine and not args.sample_drive:
        parser.error("Provide --engine for IPC benchmark and/or --sample-drive for metadata sampling")
    result = {"benchmark": "fileid-benchmark-v1"}
    if args.sample_drive:
        result["drive_sample"] = sample_drive(args.sample_drive, args.max_entries, args.max_dirs)
    if args.engine:
        try:
            engine = args.engine.resolve(strict=True)
        except OSError as exc:
            result["scan"] = {"mode": "engine_unavailable",
                              "reason": {"kind": "engine_not_found", "message": str(exc)}}
            print(json.dumps(result, sort_keys=True))
            return 2
        forbidden = args.sample_drive
        require_safe(engine, forbidden, writable=False)
        require_safe(tempfile.gettempdir(), forbidden)
        with tempfile.TemporaryDirectory(prefix="fileid-bench-") as temp:
            require_safe(temp, forbidden)
            if os.name == "nt":
                default_models = Path(os.environ["LOCALAPPDATA"]) / "FileID" / "Models"
            else:
                default_models = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "FileID" / "Models"
            models = args.models or default_models
            if not models.exists() and not args.models:
                models = Path(temp) / "models-unavailable"
            if args.models and not models.is_dir():
                raise ValueError(f"Models folder unavailable: {models}")
            models = models.resolve()
            require_safe(models, forbidden)
            require_plain_models(models)
            try:
                result["scan"] = run_engine(engine, models, Path(temp), args.files, args.timeout, forbidden)
            except OSError as exc:
                result["scan"] = {"mode": "engine_failed",
                                  "reason": {"kind": "engine_launch_error", "message": str(exc)}}
    print(json.dumps(result, sort_keys=True))
    scan = result.get("scan", {})
    if scan.get("mode") == "model_unavailable":
        return 3
    if scan.get("mode") == "engine_unavailable":
        return 2
    if scan.get("mode") == "engine_failed":
        return 1
    if scan.get("mode") == "engine_scan":
        quality_result = scan["quality"]
        if (quality_result["catalogued"] != quality_result["expected_files"] or
                quality_result["failed"] != 0 or
                quality_result["kind_correct"] != quality_result["expected_files"] or
                quality_result["document_text_hits"] != quality_result["document_count"] or
                quality_result["ocr_label_recall"] != 1.0):
            return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, RuntimeError, TimeoutError, sqlite3.Error) as exc:
        print(f"benchmark error: {exc}", file=sys.stderr)
        sys.exit(2)
