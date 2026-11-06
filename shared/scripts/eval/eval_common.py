"""Shared helpers for the People and timeline evaluations.

Drives one native engine (the macOS Swift FileIDEngine or the shared Rust
fileid-engine) over newline-delimited JSON, always with its database, logs and
state redirected into a scratch folder inside the evaluation directory. Never
point an engine at the owner's live catalog: every Engine here refuses to start
without an explicit state directory, and refuses protected source roots.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time
import uuid

PROTECTED_ROOTS = ("/volumes/adlon",)
LIVE_STATE = (
    Path.home() / "Library/Application Support/FileID",
    Path.home() / ".local/share/FileID",
)


def require_internal(path: Path, what: str) -> Path:
    resolved = Path(os.path.realpath(path))
    if any(str(resolved).lower().startswith(root) for root in PROTECTED_ROOTS):
        raise SystemExit(f"{what} resolves to a protected read-only volume: {resolved}")
    for live in LIVE_STATE:
        live = Path(os.path.realpath(live))
        if resolved == live or live in resolved.parents:
            raise SystemExit(f"{what} is inside the live FileID state folder: {resolved}")
    return resolved


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def engine_environment(state: Path, extra: dict | None = None) -> dict:
    state = require_internal(state, "engine state directory")
    database = state / "FileID/fileid.sqlite"
    database.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env.update({
        "FILEID_DATABASE_PATH": str(database),
        "FILEID_DB": str(database),
        "XDG_DATA_HOME": str(state),
        "LOCALAPPDATA": str(state),
    })
    env.update(extra or {})
    return env


def database_path(state: Path) -> Path:
    return Path(state) / "FileID/fileid.sqlite"


class Engine:
    """One engine process; events are read from stdout and stderr because the
    macOS engine writes its event stream to stderr."""

    def __init__(self, binary: Path, state: Path, env: dict | None = None, log: Path | None = None):
        self.process = subprocess.Popen(
            [str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=engine_environment(state, env), text=True, encoding="utf-8", bufsize=1,
        )
        self.events: queue.Queue = queue.Queue()
        self.log = open(log, "a", encoding="utf-8") if log else None
        for stream in (self.process.stdout, self.process.stderr):
            threading.Thread(target=self._read, args=(stream,), daemon=True).start()

    def _read(self, stream):
        for line in stream:
            if self.log:
                self.log.write(line)
            try:
                event = json.loads(line)
            except (json.JSONDecodeError, TypeError):
                continue
            if isinstance(event, dict) and isinstance(event.get("payload"), dict):
                self.events.put(event["payload"])

    def send(self, payload: dict):
        self.process.stdin.write(json.dumps({"id": str(uuid.uuid4()), "payload": payload}) + "\n")
        self.process.stdin.flush()

    def wait(self, name: str, timeout: float, request_id: str | None = None, on_event=None):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                payload = self.events.get(timeout=1)
            except queue.Empty:
                if self.process.poll() is not None:
                    raise RuntimeError(f"engine exited with {self.process.returncode} while waiting for {name}")
                continue
            if on_event:
                on_event(payload)
            if "error" in payload:
                error = payload["error"]
                error = error.get("_0", error) if isinstance(error, dict) else error
                if isinstance(error, dict) and error.get("kind") in {"db_open_failed", "db_newer_than_engine"}:
                    raise RuntimeError(error)
            if name in payload:
                result = payload[name]
                result = result.get("_0", result) if isinstance(result, dict) else result
                if request_id is None or (isinstance(result, dict) and result.get("requestID") == request_id):
                    return result
        raise TimeoutError(f"engine did not emit {name} within {timeout:.0f} s")

    def catalog(self, action: str, timeout: float = 120, **fields) -> dict:
        request_id = str(uuid.uuid4())
        self.send({"catalogRequest": {"request": {"requestID": request_id, "action": action, **fields}}})
        return self.wait("catalogResponse", timeout, request_id)

    def close(self):
        try:
            if self.process.poll() is None:
                try:
                    self.send({"shutdown": {}})
                except (BrokenPipeError, OSError):
                    pass
                self.process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            pass
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            if self.log:
                self.log.close()


def live_state_snapshot() -> dict:
    """Name -> (size, mtime) for the owner's live state folders, minus the
    thumbnail cache contents, so a run can prove it left them untouched."""
    snapshot = {}
    for root in LIVE_STATE:
        if not root.exists():
            continue
        for directory, names, files in os.walk(root):
            if Path(directory).name == "thumbnails":
                names[:] = []
            for name in files + names:
                path = Path(directory) / name
                try:
                    stat = path.lstat()
                except OSError:
                    continue
                snapshot[str(path)] = (stat.st_size if not path.is_dir() else 0, stat.st_mtime)
    return snapshot


def diff_snapshots(before: dict, after: dict) -> list[str]:
    changes = []
    for key in sorted(set(before) | set(after)):
        if before.get(key) != after.get(key):
            changes.append(key)
    return changes


def iou(a, b) -> float:
    """Intersection over union of two (x, y, w, h) boxes in the same frame."""
    ax2, ay2 = a[0] + a[2], a[1] + a[3]
    bx2, by2 = b[0] + b[2], b[1] + b[3]
    iw = max(0.0, min(ax2, bx2) - max(a[0], b[0]))
    ih = max(0.0, min(ay2, by2) - max(a[1], b[1]))
    inter = iw * ih
    union = a[2] * a[3] + b[2] * b[3] - inter
    return inter / union if union > 0 else 0.0
