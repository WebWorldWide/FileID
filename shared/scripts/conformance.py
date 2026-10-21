#!/usr/bin/env python3
"""Data-driven engine conformance suite.

Runs the JSON cases in shared/test-corpus/conformance/ against one native engine
binary (the macOS Swift FileIDEngine or the shared Rust fileid-engine). Each case
builds a disposable state directory, an optional generated file tree and SQL
fixture, sends IPC commands and checks events, database rows and files.

Expected values are literal (exact) or a tolerance object:
  {"$tol": "exact", "value": V}   deep equality, including every object key
  {"$tol": "set", "value": [..]}  same elements in any order
  {"$tol": "count", "value": N}   a list of N elements
  {"$tol": "present"}             any non-null value
  {"$engine": {"swift": A, "rust": B}}  per-engine expectation
  {"$os": {"linux": A, "default": B}}   per-OS expectation
A plain object matches a subset of keys; a list matches element by element.
Strings expand {root}, {path:rel}, {nfc_path:rel}, {path_hash:rel}, {sha256:rel},
{size:rel}, {mtime:rel}, {inode:rel}, {pid}, {state} and {conversation}; {"$var": "name.key"}
substitutes a value saved by an earlier step's "save" (a name, or
{"name", "path", "after"} to keep one field, optionally the text after a marker).

A case may reuse another case's tree, sql and env with "extends".
Cross-engine cases name "requires": ["swift", "rust"], a "start" engine and
{"restart": "rust"} steps; they run only when --peer-engine supplies the other.
A case may list knownDivergences ({"engine", "reason", optional "os" list and
"run": false when running it is unsafe}). A divergent case is expected to fail on
that engine; if it passes, the marker is stale and the run fails so the tracker
stays honest.
"""
from __future__ import annotations

import argparse
import contextlib
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unicodedata
import uuid
import wave
import zlib

ROOT = Path(__file__).resolve().parents[2]
CASES = ROOT / "shared/test-corpus/conformance"
TIMEOUT = 30.0
FORBIDDEN_ROOTS = ("/volumes/adlon", "/srv/data")
PLACEHOLDER = re.compile(r"\{([a-z0-9_]+(?::[^{}]*)?)\}")


def host_os():
    if sys.platform.startswith("linux"):
        return "linux"
    if sys.platform == "darwin":
        return "darwin"
    if sys.platform.startswith("win"):
        return "windows"
    return sys.platform


def engine_env(state):
    """Environment that confines an engine's database, logs and state to `state`."""
    state = Path(state)
    database = state / "FileID/fileid.sqlite"
    environment = dict(os.environ)
    environment.update({
        "FILEID_DATABASE_PATH": str(database),
        "FILEID_DB": str(database),
        "XDG_DATA_HOME": str(state),
        "LOCALAPPDATA": str(state),
    })
    return environment


def case_env(state, case):
    """Per-case defaults: UTC day boundaries, an empty Rust model directory and an
    empty CoreFoundation home (where the macOS engine looks for models), so results
    never depend on the runner's clock zone or installed models."""
    home = Path(state) / "home"
    home.mkdir(exist_ok=True)
    return {"TZ": "UTC", "FILEID_MODELS_DIR": str(Path(state) / "Models"), "CFFIXED_USER_HOME": str(home),
            **case.get("env", {})}


class Engine:
    """One engine process. Events are read from stdout and stderr because the
    macOS engine writes its event stream to stderr."""

    def __init__(self, binary, directory, swift=None, env=None):
        environment = engine_env(directory)
        environment.update(env or {})
        self.process = subprocess.Popen(
            [str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, env=environment, text=True, encoding="utf-8", bufsize=1,
        )
        self.events = queue.Queue()
        self.stream = {}
        for name, stream in (("stdout", self.process.stdout), ("stderr", self.process.stderr)):
            threading.Thread(target=self._read, args=(name, stream), daemon=True).start()

    def _read(self, name, stream):
        for line in stream:
            try:
                event = json.loads(line)
            except (json.JSONDecodeError, TypeError):
                continue
            if isinstance(event, dict) and isinstance(event.get("payload"), dict):
                if "ready" in event["payload"]:
                    self.stream["ready"] = name
                self.events.put(event["payload"])

    @property
    def flavor(self):
        return "swift" if self.stream.get("ready") == "stderr" else "rust"

    def next_event(self, deadline):
        while time.monotonic() < deadline:
            try:
                return self.events.get(timeout=min(1, max(0.01, deadline - time.monotonic())))
            except queue.Empty:
                if self.process.poll() is not None and self.events.empty():
                    raise RuntimeError(f"Engine exited with {self.process.returncode}")
        return None

    def wait(self, name, request_id=None, timeout=TIMEOUT, seen=None):
        deadline = time.monotonic() + timeout
        while True:
            payload = self.next_event(deadline)
            if payload is None:
                raise TimeoutError(f"Engine did not emit {name}")
            if seen is not None:
                seen.append(payload)
            if name in payload:
                result = payload[name]
                result = result.get("_0", result) if isinstance(result, dict) else result
                if request_id is None or (isinstance(result, dict) and result.get("requestID") == request_id):
                    return result
            if "error" in payload:
                error = payload["error"].get("_0", payload["error"])
                if error.get("kind") in {"db_open_failed", "db_newer_than_engine"}:
                    raise RuntimeError(error)

    def request(self, action, **fields):
        request_id = str(uuid.uuid4())
        self.send({"catalogRequest": {"request": {"requestID": request_id, "action": action, **fields}}})
        response = self.wait("catalogResponse", request_id)
        if response["status"] != "ok":
            raise RuntimeError(response.get("message", "Catalog request failed"))
        return response

    def send(self, payload):
        self.process.stdin.write(json.dumps({"id": str(uuid.uuid4()), "payload": payload}) + "\n")
        self.process.stdin.flush()

    def close(self):
        try:
            if self.process.poll() is None:
                try:
                    self.send({"shutdown": {}})
                except (BrokenPipeError, OSError):
                    pass
                self.process.wait(timeout=10)
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
                try:
                    stream.close()
                except OSError:
                    pass
        return self.process.returncode


class Failure(AssertionError):
    pass


def sip_hash_13(data):
    mask = (1 << 64) - 1
    v = [0x736F6D6570736575, 0x646F72616E646F6D, 0x6C7967656E657261, 0x7465646279746573]

    def rotl(x, b):
        return ((x << b) | (x >> (64 - b))) & mask

    def sip_round():
        v[0] = (v[0] + v[1]) & mask; v[1] = rotl(v[1], 13); v[1] ^= v[0]; v[0] = rotl(v[0], 32)
        v[2] = (v[2] + v[3]) & mask; v[3] = rotl(v[3], 16); v[3] ^= v[2]
        v[0] = (v[0] + v[3]) & mask; v[3] = rotl(v[3], 21); v[3] ^= v[0]
        v[2] = (v[2] + v[1]) & mask; v[1] = rotl(v[1], 17); v[1] ^= v[2]; v[2] = rotl(v[2], 32)

    length = len(data)
    tail = length - length % 8
    for offset in range(0, tail, 8):
        m = int.from_bytes(data[offset:offset + 8], "little")
        v[3] ^= m
        sip_round()
        v[0] ^= m
    m = int.from_bytes(data[tail:] + bytes(8 - (length - tail)), "little") | ((length & 0xFF) << 56)
    v[3] ^= m
    sip_round()
    v[0] ^= m
    v[2] ^= 0xFF
    for _ in range(3):
        sip_round()
    value = v[0] ^ v[1] ^ v[2] ^ v[3]
    return value - (1 << 64) if value >= 1 << 63 else value


def path_hash(path, os_name):
    """Catalog path_hash: SipHash-1-3 (zero key) over UTF-8 + 0xFF, ASCII-folded
    except on Linux, whose filesystems are case-sensitive."""
    data = path.encode() if os_name == "linux" else path.encode().lower()
    return sip_hash_13(data + b"\xff")


def png_bytes(width=2, height=2):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    rows = b"".join(b"\x00" + b"".join(bytes(((x * 37 + y * 91) % 256, (x * y * 13) % 256, (x + y * 7) % 256))
                                          for x in range(width)) for y in range(height))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def checker_png(width, height, blur=0):
    """Grey 4-pixel checkerboard, optionally softened by `blur` passes of a
    5-tap average: a sharp and a soft try of the same picture for Best Takes."""
    pixels = [[40 if ((x // 4) + (y // 4)) % 2 == 0 else 200 for x in range(width)] for y in range(height)]
    for _ in range(blur):
        src = [row[:] for row in pixels]
        for y in range(1, height - 1):
            for x in range(1, width - 1):
                pixels[y][x] = (src[y][x] * 4 + src[y][x - 1] + src[y][x + 1] + src[y - 1][x] + src[y + 1][x]) // 8
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    rows = b"".join(b"\x00" + bytes(v for value in row for v in (value, value, value)) for row in pixels)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def wav_bytes(path):
    with wave.open(str(path), "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(8000)
        out.writeframes(bytes(1600))
    return path.read_bytes()


def pdf_bytes(text="FileID conformance"):
    stream = f"BT /F1 12 Tf 20 50 Td ({text}) Tj ET".encode()
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for number, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % number + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    out += b"".join(b"%010d 00000 n \n" % offset for offset in offsets)
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, xref)
    return bytes(out)


class Context:
    def __init__(self, case, flavor, os_name, directory):
        self.case = case
        self.flavor = flavor
        self.os = os_name
        self.state = directory / "state"
        self.root = (directory / "tree")
        self.state.mkdir()
        self.root.mkdir()
        self.root = self.root.resolve()
        self.database = self.state / "FileID/fileid.sqlite"
        self.pid = None
        self.conversation = str(uuid.uuid4())
        self.saved = {}

    def path(self, relative):
        return str(self.root / relative) if relative else str(self.root)

    def placeholder(self, token):
        name, _, argument = token.partition(":")
        if name == "root":
            return str(self.root)
        if name == "state":
            return str(self.state)
        if name == "pid":
            return self.pid
        if name == "path":
            return self.path(argument)
        if name == "nfc_path":
            return unicodedata.normalize("NFC", self.path(argument))
        if name == "path_hash":
            return path_hash(self.path(argument), self.os)
        if name == "sha256":
            return hashlib.sha256(Path(self.path(argument)).read_bytes()).hexdigest()
        if name == "size":
            return Path(self.path(argument)).stat().st_size
        if name == "mtime":
            return Path(self.path(argument)).stat().st_mtime
        if name == "inode":
            return Path(self.path(argument)).stat().st_ino
        if name == "conversation":
            return self.conversation
        raise Failure(f"unknown placeholder {{{token}}}")

    def expand(self, value):
        if isinstance(value, str):
            whole = PLACEHOLDER.fullmatch(value)
            if whole:
                return self.placeholder(whole.group(1))
            return PLACEHOLDER.sub(lambda m: str(self.placeholder(m.group(1))), value)
        if isinstance(value, list):
            return [self.expand(item) for item in value]
        if isinstance(value, dict) and set(value) == {"$var"}:
            return self.lookup(value["$var"])
        if isinstance(value, dict):
            return {key: self.expand(item) for key, item in value.items()}
        return value

    def lookup(self, dotted):
        value = self.saved
        for part in dotted.split("."):
            value = value[int(part)] if isinstance(value, list) else value[part]
        return value

    def select(self, expected):
        if isinstance(expected, dict) and set(expected) == {"$var"}:
            return self.lookup(expected["$var"])
        if isinstance(expected, dict) and set(expected) == {"$engine"}:
            options = expected["$engine"]
            return self.select(options.get(self.flavor, options.get("default")))
        if isinstance(expected, dict) and set(expected) == {"$os"}:
            options = expected["$os"]
            return self.select(options.get(self.os, options.get("default")))
        return expected


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False)


def same(expected, actual):
    if isinstance(expected, bool) or isinstance(actual, bool):
        return expected is actual
    if isinstance(expected, (int, float)) and isinstance(actual, (int, float)):
        return float(expected) == float(actual)
    return expected == actual


def match(expected, actual, ctx, where="$"):
    expected = ctx.select(expected)
    if isinstance(expected, dict) and "$tol" in expected:
        tolerance = expected["$tol"]
        value = ctx.expand(expected.get("value"))
        if tolerance == "present":
            if actual is None:
                raise Failure(f"{where}: expected a value, got null/absent")
            return
        if tolerance == "count":
            if not isinstance(actual, list) or len(actual) != value:
                raise Failure(f"{where}: expected {value} elements, got {canonical(actual)}")
            return
        if tolerance == "exact":
            if canonical(value) != canonical(actual) and not same(value, actual):
                raise Failure(f"{where}: expected exactly {canonical(value)}, got {canonical(actual)}")
            return
        if tolerance == "set":
            if not isinstance(actual, list) or len(actual) != len(value):
                raise Failure(f"{where}: expected set {canonical(value)}, got {canonical(actual)}")
            remaining = list(actual)
            for item in value:
                for index, candidate in enumerate(remaining):
                    try:
                        match(item, candidate, ctx, where)
                    except Failure:
                        continue
                    del remaining[index]
                    break
                else:
                    raise Failure(f"{where}: no element matches {canonical(item)} in {canonical(actual)}")
            return
        raise Failure(f"{where}: unknown tolerance {tolerance}")
    if isinstance(expected, dict):
        if not isinstance(actual, dict):
            raise Failure(f"{where}: expected object, got {canonical(actual)}")
        for key, item in expected.items():
            if key not in actual:
                selected = ctx.select(item)
                if selected is None:
                    continue
                raise Failure(f"{where}.{key}: missing (expected {canonical(selected)})")
            match(item, actual[key], ctx, f"{where}.{key}")
        return
    if isinstance(expected, list):
        if not isinstance(actual, list) or len(actual) != len(expected):
            raise Failure(f"{where}: expected {canonical(ctx.expand(expected))}, got {canonical(actual)}")
        for index, (item, candidate) in enumerate(zip(expected, actual)):
            match(item, candidate, ctx, f"{where}[{index}]")
        return
    expected = ctx.expand(expected)
    if not same(expected, actual):
        raise Failure(f"{where}: expected {canonical(expected)}, got {canonical(actual)}")


def build_tree(ctx, entries):
    rows = []
    for entry in entries:
        target = ctx.root / entry["path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        generator = entry.get("generate")
        if generator == "png":
            target.write_bytes(png_bytes(*entry.get("dimensions", (2, 2))))
        elif generator == "checker":
            target.write_bytes(checker_png(*entry.get("dimensions", (96, 64)), entry.get("blur", 0)))
        elif generator == "wav":
            wav_bytes(target)
        elif generator == "pdf":
            target.write_bytes(pdf_bytes(entry.get("content", "FileID conformance")))
        elif "contentHex" in entry:
            target.write_bytes(bytes.fromhex(entry["contentHex"]))
        else:
            data = entry.get("content", "").encode()
            if "size" in entry:
                data = (data * (entry["size"] // max(len(data), 1) + 1))[:entry["size"]] if data else bytes(entry["size"])
            target.write_bytes(data)
        if "mtime" in entry:
            os.utime(target, (entry["mtime"], entry["mtime"]))
        if "row" in entry:
            rows.append((target, entry["row"]))
    return rows


@contextlib.contextmanager
def connect(path, **options):
    """sqlite3's own context manager commits but never closes, and an open
    handle stops Windows from deleting the fixture directory afterwards."""
    connection = sqlite3.connect(path, **options)
    try:
        with connection:
            yield connection
    finally:
        connection.close()


def insert_rows(ctx, rows):
    with connect(ctx.database) as database:
        for target, row in rows:
            stat = target.stat()
            values = {
                "path_text": str(target), "path_hash": path_hash(str(target), ctx.os),
                "size_bytes": stat.st_size, "modified_at": stat.st_mtime, "scanned_at": 0,
                "extension": target.suffix[1:].lower(),
                "content_hash": hashlib.sha256(target.read_bytes()).digest(),
                "file_ref": stat.st_ino,
            }
            values.update(ctx.expand(row))
            columns = ",".join(values)
            database.execute(f"INSERT INTO files({columns}) VALUES({','.join('?' * len(values))})",
                             list(values.values()))


def db_rows(ctx, sql):
    uri = "file:" + str(ctx.database) + "?mode=ro"
    with connect(uri, uri=True) as database:
        rows = database.execute(ctx.expand(sql)).fetchall()
    return [[value.hex() if isinstance(value, bytes) else value for value in row] for row in rows]


def chat_until_done(engine, request, seen):
    while True:
        result = engine.wait("chatResponse", request["requestID"], seen=seen)
        if result["status"] in ("completed", "cancelled", "error"):
            return result


def await_event(engine, ctx, step, timeout, seen):
    """Wait for the step's event matching its filter. With "abortOnError", any
    engine error event except the listed benign kinds fails the step at once."""
    deadline = time.monotonic() + timeout
    benign = set(step.get("benignErrors", ["deep_analyze_unavailable"]))
    while True:
        payload = engine.next_event(deadline)
        if payload is None:
            raise TimeoutError(f"Engine did not emit {step['await']}")
        seen.append(payload)
        if step.get("abortOnError") and "error" in payload and step["await"] != "error":
            error = payload["error"].get("_0", payload["error"])
            if error.get("kind") not in benign:
                raise Failure(f"engine error {error.get('kind')}: {error.get('message')}")
        if step["await"] in payload:
            actual = payload[step["await"]]
            actual = actual.get("_0", actual) if isinstance(actual, dict) else actual
            try:
                match(step.get("filter", {}), actual, ctx)
                return actual
            except Failure:
                continue


def run_step(engine, ctx, step, binaries):
    seen = []
    timeout = step.get("timeout", TIMEOUT)
    if "restart" in step:
        engine.close()
        return start(binaries, step["restart"] if isinstance(step["restart"], str) else ctx.flavor, ctx)
    if "command" in step:
        engine.send(ctx.expand(step["command"]))
        actual = await_event(engine, ctx, step, timeout, seen) if "await" in step else None
    elif "catalog" in step:
        request = {"requestID": str(uuid.uuid4()), **ctx.expand(step["catalog"])}
        engine.send({"catalogRequest": {"request": request}})
        actual = engine.wait("catalogResponse", request["requestID"], timeout=timeout, seen=seen)
    elif "tool" in step:
        request = {"requestID": str(uuid.uuid4()), **ctx.expand(step["tool"])}
        engine.send({"toolRequest": {"request": request}})
        actual = engine.wait("toolResponse", request["requestID"], timeout=timeout, seen=seen)
    elif "chat" in step:
        request = {"requestID": str(uuid.uuid4()), "conversationID": ctx.conversation, **ctx.expand(step["chat"])}
        engine.send({"chatRequest": {"request": request}})
        actual = chat_until_done(engine, request, seen)
    elif "db" in step:
        actual = db_rows(ctx, step["db"])
    elif "fs" in step:
        spec = ctx.expand(step["fs"])
        for relative in spec.get("exists", []):
            if not Path(ctx.path(relative)).exists():
                raise Failure(f"expected {relative} to exist")
        for relative in spec.get("absent", []):
            if Path(ctx.path(relative)).exists():
                raise Failure(f"expected {relative} to be absent")
        for relative, text in spec.get("contents", {}).items():
            if Path(ctx.path(relative)).read_text(encoding="utf-8") != text:
                raise Failure(f"{relative} content changed")
        for relative, digest in spec.get("sha256", {}).items():
            if hashlib.sha256(Path(ctx.path(relative)).read_bytes()).hexdigest() != digest:
                raise Failure(f"{relative} bytes changed")
        for relative, width, height in spec.get("pngSize", []):
            data = Path(ctx.path(relative)).read_bytes()
            if data[:8] != b"\x89PNG\r\n\x1a\n" or struct.unpack(">II", data[16:24]) != (width, height):
                raise Failure(f"{relative} is not a {width}x{height} PNG")
        return engine
    elif "shutdown" in step:
        code = engine.close()
        match(step["shutdown"], {"exitCode": code}, ctx)
        return engine
    else:
        raise Failure(f"unknown step {sorted(step)}")
    saves = step.get("save", [])
    for save in saves if isinstance(saves, list) else [saves]:
        if isinstance(save, str):
            ctx.saved[save] = actual
            continue
        value = actual
        for part in save["path"].split(".") if save.get("path") else []:
            value = value[int(part)] if isinstance(value, list) else value[part]
        if "after" in save:
            value = str(value).partition(save["after"])[2]
        ctx.saved[save["name"]] = value
    if "expect" in step:
        try:
            match(step["expect"], actual, ctx)
        except Failure as failure:
            detail = canonical(actual)
            raise Failure(f"{failure}\n         actual: {detail[:800]}{'…' if len(detail) > 800 else ''}") from None
    for name, expected in step.get("collect", {}).items():
        collected = [payload[name].get("_0", payload[name]) for payload in seen if name in payload]
        match(expected, collected, ctx, f"collect.{name}")
    return engine


def start(binaries, flavor, ctx):
    ctx.flavor = flavor
    engine = Engine(binaries[flavor], ctx.state, env=case_env(ctx.state, ctx.case))
    ready = engine.wait("ready")
    ctx.pid = ready.get("pid")
    return engine


def divergence_for(case, flavor, os_name):
    for item in case.get("knownDivergences", []):
        if item["engine"] == flavor and os_name in item.get("os", [os_name]):
            return item
    return None


def run_case(case, flavor, binaries, os_name, parent):
    with tempfile.TemporaryDirectory(prefix="FileIDConformance-", dir=parent) as temporary:
        ctx = Context(case, flavor, os_name, Path(temporary))
        first = case.get("start", flavor)
        start(binaries, first, ctx).close()
        rows = build_tree(ctx, case.get("tree", []))
        if rows:
            insert_rows(ctx, rows)
        if case.get("sql"):
            with connect(ctx.database) as database:
                for statement in case["sql"]:
                    database.execute(ctx.expand(statement))
        engine = start(binaries, first, ctx)
        try:
            for index, step in enumerate(case["steps"]):
                label = step.get("label", f"step {index + 1}")
                try:
                    engine = run_step(engine, ctx, step, binaries)
                except (Failure, TimeoutError, RuntimeError) as error:
                    raise Failure(f"{label}: {error}") from None
        finally:
            engine.close()


def load_cases(selected):
    every = []
    for path in sorted(CASES.glob("*.json")):
        for case in json.loads(path.read_text(encoding="utf-8")):
            case["file"] = path.name
            every.append(case)
    named = {case["name"]: case for case in every}
    for case in every:
        if "extends" in case:
            for key in ("tree", "sql", "env"):
                if key not in case and key in named[case["extends"]]:
                    case[key] = named[case["extends"]][key]
    return [case for case in every
            if not selected or any(fnmatch.fnmatch(case["name"], pattern) for pattern in selected)]


def detect_flavor(binary, parent):
    with tempfile.TemporaryDirectory(prefix="FileIDConformance-", dir=parent) as temporary:
        engine = Engine(binary, temporary)
        try:
            engine.wait("ready")
            return engine.flavor
        finally:
            engine.close()


def safe_parent(path):
    path = Path(path).resolve()
    text = str(path).casefold()
    if any(text == root or text.startswith(root + "/") for root in FORBIDDEN_ROOTS):
        raise SystemExit("Conformance fixtures must stay on internal temporary storage")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", type=Path, required=True, help="engine binary under test")
    parser.add_argument("--peer-engine", type=Path, help="the other engine, for cross-engine cases")
    parser.add_argument("--flavor", choices=("auto", "swift", "rust"), default="auto")
    parser.add_argument("--case", action="append", default=[], help="glob of case names to run")
    parser.add_argument("--os", default=host_os(), help=argparse.SUPPRESS)
    parser.add_argument("--fixture-parent", type=Path, default=Path(tempfile.gettempdir()))
    args = parser.parse_args()
    parent = safe_parent(args.fixture_parent)
    binary = args.engine.resolve(strict=True)
    flavor = detect_flavor(binary, parent) if args.flavor == "auto" else args.flavor
    binaries = {flavor: binary}
    if args.peer_engine:
        peer = args.peer_engine.resolve(strict=True)
        binaries[detect_flavor(peer, parent)] = peer
    counts = {"pass": 0, "fail": 0, "known": 0, "stale": 0, "skip": 0}
    print(f"conformance: {flavor} engine {binary} on {args.os}")
    for case in load_cases(args.case):
        name = case["name"]
        missing = set(case.get("requires", [])) - set(binaries)
        if flavor not in case.get("engines", ["swift", "rust"]) or missing or args.os not in case.get("os", [args.os]):
            counts["skip"] += 1
            reason = f"needs {', '.join(sorted(missing))} engine" if missing else "not applicable here"
            print(f"SKIP   {name} ({reason})")
            continue
        divergence = divergence_for(case, flavor, args.os)
        if divergence and divergence.get("run") is False:
            counts["known"] += 1
            print(f"KNOWN  {name} [{flavor}] not run: {divergence['reason']}")
            continue
        try:
            run_case(case, flavor, binaries, args.os, parent)
            error = None
        except Failure as failure:
            error = str(failure)
        except Exception as failure:  # an engine crash or broken fixture fails only this case
            error = f"{type(failure).__name__}: {failure}"
        if divergence and error:
            counts["known"] += 1
            print(f"KNOWN  {name} [{flavor}]: {divergence['reason']}\n         {error}")
        elif divergence:
            counts["stale"] += 1
            print(f"STALE  {name} [{flavor}]: passes now; remove the known divergence ({divergence['reason']})")
        elif error:
            counts["fail"] += 1
            print(f"FAIL   {name}: {error}")
        else:
            counts["pass"] += 1
            print(f"PASS   {name}")
    print("conformance: " + ", ".join(f"{value} {key}" for key, value in counts.items()))
    return 1 if counts["fail"] or counts["stale"] else 0


if __name__ == "__main__":
    sys.exit(main())
