#!/usr/bin/env python3
"""Data-driven engine conformance suite.

Runs the JSON cases in shared/test-corpus/conformance/ against the shared Rust
engine (fileid-engine, plain or as the Mac app hosts it). Each case builds a
disposable state directory, an optional generated file tree and SQL fixture,
sends IPC commands and checks events, database rows and files.

Expected values are literal (exact) or a tolerance object:
  {"$tol": "exact", "value": V}   deep equality, including every object key
  {"$tol": "set", "value": [..]}  same elements in any order
  {"$tol": "count", "value": N}   a list of N elements
  {"$tol": "prefix", "value": [..]}  a list that starts with these elements
  {"$tol": "contains", "value": [..]}  a list holding a match for each element
  {"$tol": "last", "value": V}    a non-empty list whose last element matches V
  {"$tol": "present"}             any non-null value
  {"$tol": "hamming", "value": N, "bits": B}  an integer hash within B differing bits (default 4)
  {"$engine": {"rust": A, "rustHosted": B}}  A applies to every build; an optional
      "rustHosted" applies to the Rust engine the Mac app hosts (the
      apple-adapters build, events on stderr) instead
  {"$os": {"linux": A, "default": B}}   per-OS expectation
A plain object matches a subset of keys; a list matches element by element.
Strings expand {root}, {path:rel}, {std_path:rel} (the path as Foundation standardizes it on
macOS, how the engine names output files: /private/var/… → /var/…), {nfc_path:rel}, {path_hash:rel}, {sha256:rel},
{size:rel}, {mtime:rel}, {inode:rel}, {pid}, {state}, {conversation}, {hardware} (the Mac's benchmark hardware string), {clip_model} (the CLIP
embedding model id the engine derives from the model manifest), {clip_index_job} (the id of the visual index job for that model) and {bookmark:rel} (a base64
bookmark of the path, as the Mac app sends; macOS only); {"$var": "name.key"}
substitutes a value saved by an earlier step's "save" (a name, or
{"name", "path", "after"} to keep one field, optionally the text after a marker).

A command step with "commandID" sends the command under that id instead of a random one,
so a later "expect" can require the engine to echo it (bulkActionResult.commandID).

A catalog step with "until": seconds resends its request until "expect" matches or the
time runs out, for durable jobs that finish after their reply.
A step {"schemaOf": "<fixture>"} requires the catalog to have that fixture's schema objects and columns.
A case may reuse another case's tree, sql and env with "extends". Tree paths expand
placeholders, so "{state}/…" seeds a file in the engine's state folder; an "fs" step
also checks "symlinks", empties folders ("emptyDir"), checks a folder's exact entry names ("entries": {folder: [names]})
and writes tree entries ("tree") before its checks.
A tree entry with "copy" copies a repo file (synthetic media under shared/test-corpus).
A tree entry with "mtimeNs": [seconds, nanoseconds] sets the modification time exactly.
A {"restart": true} step stops the engine and starts it again on the same state.
A case with "appHosted": true runs only against the Rust engine the Mac app hosts
(built with the Apple adapters; writes events on stderr). Case "env" values expand
{state} and {root}.
A case with "fixture": "<name>" starts from state the retired Swift engine wrote
(fixtures/swift-written/<name>: state/, tree/ overlay and fixture.json with file
times, removed files, saved step values and chat conversation), laid out in the fixed folder
/private/tmp/fileid-conformance-fixture/<case name> it was captured in, so the paths
inside the catalog stay true; the file identities the Swift run recorded are
refreshed to the replayed files. Such cases are macOS-only ("os": ["darwin"]) and
must not run alongside another conformance run. A fixture case's "sql" runs after the
fixture is laid out (e.g. to move a journaled time to now for a time-limited undo).
A case with "fixture": "<name>" that names no folder under fixtures/swift-written may instead name a folder
under shared/test-corpus/swift-written/<name>/ (see its README), with optional "fixtureOmit" (state files
to leave out), "fixtureDropLines" ({state file: text that removes the lines holding it}) and "sql" (statements
run on the seeded catalog): the harness seeds the state folder, the library tree and the catalog from that folder
before the engine's first start. --verify-fixture <name> seeds such a folder in a scratch library and captures it
again, which must give the same folder; it needs no engine. Fixture text expands {root}, {state}, {std_root},
{std_state} (the folders as Foundation standardizes them), {device} (the library's volume), {inode:rel},
{state_inode:rel}, {path_hash:rel} and {std_path_hash:rel}.
Cases that name "requires" another engine or a "start" engine need the retired Swift engine and are skipped.
A case with "model": {"repo": "<org>/<name>"} needs that MLX model installed
($FILEID_CONFORMANCE_MODELS, else ~/Documents/huggingface, read only) and is skipped
without it: a real model is too large to ship in the repo and no engine may download
one here. The harness clones the install into the case's scratch Hugging Face root and
sets FILEID_HF_CACHE_ROOT for every engine it starts.
A case may list knownDivergences ({"engine", "reason", optional "os" list and
"run": false when running it is unsafe}). A divergent case is expected to fail on
that engine; if it passes, the marker is stale and the run fails so the tracker
stays honest.

--test-trash-root is for an engine built with a test Trash root (the Rust
`test-trash-root` cargo feature; test builds only, never shipped): each case's
engine moves trashed files into <state>/Trash instead of the user's Trash,
divergences marked "liftedByTestTrashRoot" no longer apply, and cases marked
"requiresTestTrashRoot" (true, or a list of OS names such as ["darwin"] where only
that OS needs it) run; they are skipped otherwise, so no engine ever reaches the
user's Trash.

Cases marked "requiresSymlinks" are skipped when the runner's account cannot create
symlinks (Windows without Developer Mode or the privilege).
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
import shutil
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
FIXTURES = ROOT / "shared/test-corpus/swift-written"
TIMEOUT = 30.0
FORBIDDEN_ROOTS = ("/volumes/adlon", "/srv/data")
PLACEHOLDER = re.compile(r"\{([a-z0-9_]+(?::[^{}]*)?)\}")


BOOKMARK_JXA = (
    "ObjC.import('Foundation');"
    "function run(argv) {"
    " const url = $.NSURL.fileURLWithPath(argv[0]);"
    " const data = url.bookmarkDataWithOptionsIncludingResourceValuesForKeysRelativeToURLError(0, $(), $(), $());"
    " return data.base64EncodedStringWithOptions(0).js; }"
)


def bookmark(path):
    """A plain base64 bookmark of `path`, the kind the Mac app sends (macOS only)."""
    result = subprocess.run(["osascript", "-l", "JavaScript", "-e", BOOKMARK_JXA, path],
                            capture_output=True, text=True, timeout=30, check=False)
    if result.returncode != 0 or not result.stdout.strip():
        raise Failure(f"could not make a bookmark for {path}: {result.stderr.strip()}")
    return result.stdout.strip()


def foundation_standardized(path):
    """An output file as the engine names it: its folder through
    `URL.standardizedFileURL`, which on macOS drops a leading /private when the rest
    exists (/private/var/… → /var/…), then the file name."""
    folder, name = os.path.split(path)
    if sys.platform == "darwin" and folder.startswith("/private/") and os.path.exists(folder[len("/private"):]):
        folder = folder[len("/private"):]
    return os.path.join(folder, name)


def clip_model():
    manifest = json.loads((ROOT / "shared/models/manifest.json").read_text(encoding="utf-8"))
    hashes = {entry["id"]: entry["sha256"] for entry in manifest["artifacts"]}
    ids = ["clip_vitb32_image", "clip_vitb32_text", "clip_bpe_vocab", "clip_bpe_merges"]
    descriptor = "|".join(["clip-rgb-stretch224-bpe77-l2-v1"] + [f"{key}:{hashes.get(key, 'missing')}" for key in ids])
    return "clip-vitb32-openai-v1:" + hashlib.sha256(descriptor.encode()).hexdigest()


def mac_hardware():
    """The hardware string the Mac engine files benchmarks under."""
    def sysctl(name):
        return subprocess.run(["sysctl", "-n", name], capture_output=True, text=True, check=True).stdout.strip()
    memory = round(int(sysctl("hw.memsize")) / 1073741824)
    version = ".".join(sysctl("kern.osproductversion").split(".")[:2])
    return f"{sysctl('machdep.cpu.brand_string')} \u00b7 {memory} GB \u00b7 {sysctl('hw.activecpu')} cores \u00b7 macOS {version}"


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


TEST_TRASH_ROOT = False


def case_env(state, case, extra_env=None):
    """Per-case defaults: UTC day boundaries, an empty Rust model directory and an
    empty CoreFoundation home (where the macOS engine looks for models), so results
    never depend on the runner's clock zone or installed models."""
    home = Path(state) / "home"
    home.mkdir(exist_ok=True)
    trash = {"FILEID_TEST_TRASH_ROOT": str(Path(state) / "Trash")} if TEST_TRASH_ROOT else {}
    places = {"{state}": str(Path(state)), "{root}": str((Path(state).parent / "tree").resolve())}
    extra = {}
    for key, value in case.get("env", {}).items():
        for token, replacement in places.items():
            value = value.replace(token, replacement)
        extra[key] = value
    return {"TZ": "UTC", "FILEID_MODELS_DIR": str(Path(state) / "Models"), "CFFIXED_USER_HOME": str(home),
            **trash, **extra, **(extra_env or {})}


class Engine:
    """One engine process. Events are read from stdout and stderr because the
    macOS engine writes its event stream to stderr."""

    def __init__(self, binary, directory, env=None):
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
                if error.get("kind") in {"db_open_failed", "db_disk_full", "db_newer_than_engine"}:
                    raise RuntimeError(error)

    def request(self, action, **fields):
        request_id = str(uuid.uuid4())
        self.send({"catalogRequest": {"request": {"requestID": request_id, "action": action, **fields}}})
        response = self.wait("catalogResponse", request_id)
        if response["status"] != "ok":
            raise RuntimeError(response.get("message", "Catalog request failed"))
        return response

    def send(self, payload, command_id=None):
        self.process.stdin.write(json.dumps({"id": command_id or str(uuid.uuid4()), "payload": payload}) + "\n")
        self.process.stdin.flush()

    def close(self):
        try:
            if self.process.poll() is None:
                try:
                    self.send({"shutdown": {}})
                except (BrokenPipeError, OSError):
                    pass
                # The engine allows itself 30 s to quiesce active work at shutdown; a
                # slow hosted Windows runner has taken longer than 10 s to exit.
                self.process.wait(timeout=30)
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
        self.hosted = False
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
        self.extra_env = {}

    def path(self, relative):
        return str(self.root / relative) if relative else str(self.root)

    def placeholder(self, token):
        name, _, argument = token.partition(":")
        if name in ("root", "state", "std_root", "std_state"):
            value = {"root": str(self.root), "state": str(self.state),
                     "std_root": foundation_standardized(str(self.root)),
                     "std_state": foundation_standardized(str(self.state))}[name]
            return value.replace("/", "\\/") if argument == "json" else value
        if name == "device":
            return self.root.stat().st_dev
        if name == "state_inode":
            return (self.state / argument).stat().st_ino
        if name == "std_path_hash":
            return path_hash(foundation_standardized(self.path(argument)), self.os)
        if name == "pid":
            return self.pid
        if name == "path":
            return self.path(argument)
        if name == "std_path":
            return foundation_standardized(self.path(argument))
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
        if name == "clip_model":
            return clip_model()
        if name == "clip_index_job":
            return "catalog-index-" + hashlib.sha256(clip_model().encode()).hexdigest()
        if name == "hardware":
            return mac_hardware()
        if name == "bookmark":
            return bookmark(self.path(argument))
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
            if self.flavor == "rust" and self.hosted and "rustHosted" in options:
                return self.select(options["rustHosted"])
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


def _matches(expected, actual, ctx, where):
    try:
        match(expected, actual, ctx, where)
    except Failure:
        return False
    return True


def match(expected, actual, ctx, where="$"):
    expected = ctx.select(expected)
    if isinstance(expected, dict) and "$tol" in expected:
        tolerance = expected["$tol"]
        value = ctx.expand(expected.get("value"))
        if tolerance == "present":
            if actual is None:
                raise Failure(f"{where}: expected a value, got null/absent")
            return
        if tolerance == "hamming":
            # Perceptual hashes: CoreGraphics' resampling can move a bit between
            # macOS releases; engines are compared exactly in cross-engine cases.
            bits = expected.get("bits", 4)
            if not isinstance(actual, int) or bin((actual ^ value) & (2**64 - 1)).count("1") > bits:
                raise Failure(f"{where}: expected {value} within {bits} bits, got {canonical(actual)}")
            return
        if tolerance == "count":
            if not isinstance(actual, list) or len(actual) != value:
                raise Failure(f"{where}: expected {value} elements, got {canonical(actual)}")
            return
        if tolerance == "exact":
            if canonical(value) != canonical(actual) and not same(value, actual):
                raise Failure(f"{where}: expected exactly {canonical(value)}, got {canonical(actual)}")
            return
        if tolerance == "prefix":
            if not isinstance(actual, list) or len(actual) < len(value):
                raise Failure(f"{where}: expected a list starting with {canonical(value)}, got {canonical(actual)}")
            for index, item in enumerate(value):
                match(item, actual[index], ctx, f"{where}[{index}]")
            return
        if tolerance == "last":
            if not isinstance(actual, list) or not actual:
                raise Failure(f"{where}: expected a non-empty list, got {canonical(actual)}")
            match(value, actual[-1], ctx, f"{where}[-1]")
            return
        if tolerance == "contains":
            if not isinstance(actual, list):
                raise Failure(f"{where}: expected a list, got {canonical(actual)}")
            for item in value:
                if not any(_matches(item, candidate, ctx, where) for candidate in actual):
                    raise Failure(f"{where}: no element matches {canonical(item)}")
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
        target = ctx.root / ctx.expand(entry["path"])
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
        elif "copy" in entry:
            target.write_bytes((ROOT / entry["copy"]).read_bytes())
        elif "contentHex" in entry:
            target.write_bytes(bytes.fromhex(entry["contentHex"]))
        else:
            data = ctx.expand(entry.get("content", "")).encode()
            if "size" in entry:
                data = (data * (entry["size"] // max(len(data), 1) + 1))[:entry["size"]] if data else bytes(entry["size"])
            target.write_bytes(data)
        if "mtime" in entry:
            os.utime(target, (entry["mtime"], entry["mtime"]))
        if "mtimeNs" in entry:
            seconds, nanoseconds = entry["mtimeNs"]
            os.utime(target, ns=(seconds * 1_000_000_000 + nanoseconds,) * 2)
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


def run_step(engine, ctx, step, binary):
    seen = []
    timeout = step.get("timeout", TIMEOUT)
    if "restart" in step:
        engine.close()
        return start(binary, ctx)
    if "command" in step:
        engine.send(ctx.expand(step["command"]), ctx.expand(step.get("commandID")))
        actual = await_event(engine, ctx, step, timeout, seen) if "await" in step else None
    elif "catalog" in step:
        deadline = time.monotonic() + step.get("until", 0)
        while True:
            request = {"requestID": str(uuid.uuid4()), **ctx.expand(step["catalog"])}
            engine.send({"catalogRequest": {"request": request}})
            actual = engine.wait("catalogResponse", request["requestID"], timeout=timeout, seen=seen)
            if "until" not in step or time.monotonic() >= deadline:
                break
            try:
                match(step.get("expect", {}), actual, ctx)
                break
            except Failure:
                time.sleep(0.1)
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
    elif "schemaOf" in step:
        same_schema_as(ctx, step["schemaOf"])
        return engine
    elif "fs" in step:
        spec = ctx.expand(step["fs"])
        for relative in spec.get("emptyDir", []):
            for child in Path(ctx.path(relative)).iterdir():
                shutil.rmtree(child) if child.is_dir() and not child.is_symlink() else child.unlink()
        if "tree" in spec:
            build_tree(ctx, spec["tree"])
        for relative in spec.get("exists", []):
            if not Path(ctx.path(relative)).exists():
                raise Failure(f"expected {relative} to exist")
        for relative, names in spec.get("entries", {}).items():
            found = sorted(os.listdir(ctx.path(relative)))
            if found != sorted(names):
                raise Failure(f"{relative} holds {[ascii(n) for n in found]}, expected {[ascii(n) for n in sorted(names)]}")
        for relative in spec.get("absent", []):
            if Path(ctx.path(relative)).exists() or Path(ctx.path(relative)).is_symlink():
                raise Failure(f"expected {relative} to be absent")
        for name in spec.get("inTestTrash", []) if TEST_TRASH_ROOT else []:
            if not (ctx.state / "Trash" / name).exists():
                raise Failure(f"expected {name} in the test Trash root")
        for relative in spec.get("symlinks", []):
            if not Path(ctx.path(relative)).is_symlink():
                raise Failure(f"expected {relative} to be a symbolic link")
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


def start(binary, ctx):
    engine = Engine(binary, ctx.state, env=case_env(ctx.state, ctx.case, ctx.extra_env))
    ready = engine.wait("ready")
    ctx.pid = ready.get("pid")
    ctx.hosted = engine.stream.get("ready") == "stderr"
    return engine


def can_symlink(parent):
    """Windows needs Developer Mode or SeCreateSymbolicLinkPrivilege; the self-hosted runner's account may have neither."""
    with tempfile.TemporaryDirectory(prefix="FileIDSymlinkProbe-", dir=parent) as temporary:
        target = Path(temporary) / "target"
        target.write_text("x")
        try:
            os.symlink(target, Path(temporary) / "link")
        except (OSError, NotImplementedError):
            return False
    return True


def divergence_for(case, flavor, os_name):
    for item in case.get("knownDivergences", []):
        if item.get("liftedByTestTrashRoot") and TEST_TRASH_ROOT:
            continue
        if item["engine"] == flavor and os_name in item.get("os", [os_name]):
            return item
    return None


def model_source(case):
    """The installed copy of the case's "model" (a Hugging Face repo such as
    mlx-community/Qwen3-VL-2B-Instruct-4bit), or None. Looks under
    $FILEID_CONFORMANCE_MODELS, else ~/Documents/huggingface; read only."""
    wanted = case.get("model")
    if not wanted:
        return None
    root = Path(os.environ.get("FILEID_CONFORMANCE_MODELS") or Path.home() / "Documents/huggingface")
    folder = root / "models" / wanted["repo"]
    return folder if (folder / "config.json").is_file() and (folder / ".fileid-installed").is_file() else None


def stage_model(ctx, source):
    """Clone the installed model into this case's Hugging Face root and point both
    engines at it, so no engine touches (or writes install records into) the original."""
    target = ctx.state.parent / "hf/models" / ctx.case["model"]["repo"]
    target.parent.mkdir(parents=True)
    if subprocess.run(["cp", "-cR", str(source), str(target)], capture_output=True, check=False).returncode != 0:
        shutil.rmtree(target, ignore_errors=True)
        shutil.copytree(source, target)
    ctx.extra_env["FILEID_HF_CACHE_ROOT"] = str(ctx.state.parent / "hf")


class Raw(str):
    """SQL text emitted as is (a placeholder standing in for an integer)."""


def sql_literal(value):
    if isinstance(value, Raw):
        return str(value)
    if value is None:
        return "NULL"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        if value != value or value in (float("inf"), float("-inf")):
            raise Failure("a catalog holds a non-finite number; it cannot be a text fixture")
        return repr(value)
    if isinstance(value, bytes):
        return "X'" + value.hex() + "'"
    if "\x00" in value:
        return "CAST(X'" + value.encode().hex() + "' AS TEXT)"
    return "'" + value.replace("'", "''") + "'"


def quote_name(name):
    return '"' + name.replace('"', '""') + '"'


FTS_SHADOWS = ("_data", "_idx", "_content", "_docsize", "_config")


def catalog_objects(connection):
    """sqlite_master rows in creation order, split into the tables (virtual tables
    included, their shadow tables not), the shadow tables of the full-text indexes, and
    the indexes, triggers and views that are created after the data."""
    rows = connection.execute("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY rowid").fetchall()
    virtual = [name for kind, name, _, sql in rows if kind == "table" and (sql or "").upper().startswith("CREATE VIRTUAL")]

    def shadow(name):
        return any(name == v + suffix for v in virtual for suffix in FTS_SHADOWS)
    tables = [r for r in rows if r[0] == "table" and r[3] and not shadow(r[1]) and r[1] != "sqlite_sequence"]
    shadows = [r for r in rows if r[0] == "table" and shadow(r[1])]
    after = [r for r in rows if r[0] in ("index", "trigger", "view") and r[3]]
    return tables, shadows, after, virtual


def fts_content(sql):
    """None for a full-text table that keeps its own text, else the table its text lives in."""
    found = re.search(r"content\s*=\s*'?(\w*)'?", sql)
    if found and not found.group(1):
        raise Failure("a contentless full-text table cannot be dumped as text")
    return found.group(1) if found else None


def table_columns(connection, name):
    """(columns to dump, whether the rowid is a column of its own, whether the table is WITHOUT ROWID)."""
    info = connection.execute(f"PRAGMA table_xinfo({quote_name(name)})").fetchall()
    columns = [row[1] for row in info if row[6] == 0]
    without_rowid = bool(connection.execute("SELECT wr FROM pragma_table_list WHERE name=? AND schema='main'", (name,)).fetchone()[0])
    keys = [row for row in info if row[5] > 0]
    alias = len(keys) == 1 and (keys[0][2] or "").upper() == "INTEGER"
    return columns, (not without_rowid and not alias), without_rowid


def table_rows(connection, name):
    columns, own_rowid, without_rowid = table_columns(connection, name)
    names = (["rowid"] if own_rowid else []) + columns
    order = ",".join(quote_name(c) for c in columns) if without_rowid else "rowid"
    select = ",".join("rowid" if c == "rowid" else quote_name(c) for c in names)
    return names, connection.execute(f"SELECT {select} FROM {quote_name(name)} ORDER BY {order}").fetchall()


JSON_SLASH = "\\/"
LEFTOVERS = ("/Users/", "/private/var", "/var/folders", "FileIDConformance-", "\\/Users\\/", "\\/private\\/var", "\\/var\\/folders")


class Templater:
    """Turns a live run's machine-specific text and numbers into placeholders and
    refuses to write anything machine-specific that it could not turn."""

    def __init__(self, ctx):
        self.ctx = ctx
        pairs = {}
        for needle, name in ((str(ctx.root), "root"), (foundation_standardized(str(ctx.root)), "std_root"),
                             (str(ctx.state), "state"), (foundation_standardized(str(ctx.state)), "std_state")):
            pairs.setdefault(needle, name)
        self.pairs = sorted(pairs.items(), key=lambda item: -len(item[0]))
        self.numbers = {ctx.root.stat().st_dev: "{device}"}
        for path in sorted(ctx.root.rglob("*")):
            if path.is_file():
                self.numbers.setdefault(path.stat().st_ino, "{inode:%s}" % path.relative_to(ctx.root).as_posix())
        trash = ctx.state / "Trash"
        for path in sorted(trash.rglob("*")) if trash.is_dir() else []:
            if path.is_file():
                self.numbers.setdefault(path.stat().st_ino, "{state_inode:Trash/%s}" % path.relative_to(trash).as_posix())

    def add_paths(self, texts):
        """A hash placeholder for every library path seen in the catalog or a journal, kept
        or gone: the hash is a function of the path text alone."""
        for text in texts:
            text = text.replace(JSON_SLASH, "/")
            for needle, _ in self.pairs:
                if text.startswith(needle + "/"):
                    relative = text[len(needle) + 1:]
                    full = str(self.ctx.root / relative)
                    self.numbers.setdefault(path_hash(full, self.ctx.os), "{path_hash:%s}" % relative)
                    self.numbers.setdefault(path_hash(foundation_standardized(full), self.ctx.os), "{std_path_hash:%s}" % relative)

    def text(self, value):
        if PLACEHOLDER.search(value):
            raise Failure(f"text already looks like a placeholder: {value[:80]!r}")
        for needle, name in self.pairs:
            value = value.replace(needle.replace("/", JSON_SLASH), "{%s:json}" % name).replace(needle, "{%s}" % name)
        return value

    def number(self, value):
        if isinstance(value, int) and abs(value) >= 1_000_000 and value in self.numbers:
            return Raw(self.numbers[value])
        return value

    def journal(self, raw):
        """A journal or plan file: paths become placeholders, and so do the large integers
        that stand for a file of this run (its inode, its path hash)."""
        out = self.text(raw)
        return re.sub(r"(?<![\w.])-?\d{7,}(?![\w.])", lambda m: self.numbers.get(int(m.group(0)), m.group(0)), out)

    def data(self, value):
        if isinstance(value, str):
            return self.text(value)
        if isinstance(value, list):
            return [self.data(item) for item in value]
        if isinstance(value, dict):
            return {key: self.data(item) for key, item in value.items()}
        return value


def refuse_leftovers(text, where):
    lowered = text.lower()
    for marker in (*LEFTOVERS, str(Path.home()), tempfile.gettempdir(), "/private/tmp", "/tmp/"):
        if marker.lower() in lowered:
            raise Failure(f"{where} still holds {marker}")


def dump_catalog(path, templater):
    """The catalog as SQL a reviewer can read: its schema exactly as stored, one INSERT per
    row, and the objects that fire on writes created after the data."""
    with tempfile.TemporaryDirectory(prefix="FileIDDump-") as temporary:
        copy = Path(temporary) / "fileid.sqlite"
        for suffix in ("", "-wal", "-shm"):
            if Path(str(path) + suffix).exists():
                shutil.copy2(str(path) + suffix, str(copy) + suffix)
        connection = sqlite3.connect(copy)
        try:
            tables, shadows, after, virtual = catalog_objects(connection)
            pragmas = {name: connection.execute(f"PRAGMA {name}").fetchone()[0]
                       for name in ("user_version", "application_id", "journal_mode", "page_size", "auto_vacuum")}
            # The full-text indexes are rebuilt from their text on load: their segment files are a
            # product of insertion history and would carry the capture machine's paths as tokens.
            data = [(name, True, table_rows(connection, name)) for _, name, _, _ in shadows if name.endswith("_config")]
            data += [(name, False, table_rows(connection, name)) for _, name, _, sql in tables if name not in virtual]
            for _, name, _, sql in tables:
                if name in virtual and not fts_content(sql):
                    columns = [row[1] for row in connection.execute(f"PRAGMA table_xinfo({quote_name(name)})") if row[6] == 0]
                    rows = connection.execute(
                        f"SELECT rowid,{','.join(quote_name(c) for c in columns)} FROM {quote_name(name)} ORDER BY rowid").fetchall()
                    data.append((name, False, (["rowid"] + columns, rows)))
            templater.add_paths([cell for _, _, (_, rows) in data for row in rows for cell in row if isinstance(cell, str)])
            lines = ["BEGIN;"] + [sql.rstrip(";") + ";" for _, _, _, sql in tables]
            for name, shadow, (names, rows) in data:
                if shadow:
                    lines.append(f"DELETE FROM {quote_name(name)};")
                columns = ",".join("rowid" if c == "rowid" else quote_name(c) for c in names)
                for row in rows:
                    cells = [sql_literal(templater.text(v) if isinstance(v, str) else templater.number(v)) for v in row]
                    lines.append(f"INSERT INTO {quote_name(name)}({columns}) VALUES({','.join(cells)});")
            lines.extend(f"INSERT INTO {quote_name(name)}({quote_name(name)}) VALUES('rebuild');"
                         for _, name, _, sql in tables if name in virtual and fts_content(sql))
            has_sequence = connection.execute("SELECT 1 FROM sqlite_master WHERE name='sqlite_sequence'").fetchone()
            if has_sequence:
                names, rows = table_rows(connection, "sqlite_sequence")
                if rows:
                    lines.append("DELETE FROM sqlite_sequence;")
                    lines.extend(f"INSERT INTO sqlite_sequence({','.join(names)}) VALUES({','.join(sql_literal(v) for v in row)});" for row in rows)
            lines.extend(sql.rstrip(";") + ";" for _, _, _, sql in after)
            lines.append("COMMIT;")
        finally:
            connection.close()
    return "\n".join(lines) + "\n", pragmas


def load_catalog(database, sql_text, pragmas):
    database.parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(database, isolation_level=None)
    try:
        if pragmas.get("page_size"):
            connection.execute(f"PRAGMA page_size={int(pragmas['page_size'])}")
        if pragmas.get("auto_vacuum"):
            connection.execute(f"PRAGMA auto_vacuum={int(pragmas['auto_vacuum'])}")
        if str(pragmas.get("journal_mode", "")).lower() == "wal":
            connection.execute("PRAGMA journal_mode=WAL")
        connection.execute("PRAGMA foreign_keys=OFF")
        connection.executescript(sql_text)
        connection.execute(f"PRAGMA application_id={int(pragmas.get('application_id', 0))}")
        connection.execute(f"PRAGMA user_version={int(pragmas['user_version'])}")
    finally:
        connection.close()


def catalog_image(path):
    """Everything a catalog file holds, to compare two of them: its schema objects, the
    rows of every table (the shadow tables of the full-text indexes included) and its pragmas."""
    with tempfile.TemporaryDirectory(prefix="FileIDImage-") as temporary:
        copy = Path(temporary) / "c.sqlite"
        for suffix in ("", "-wal", "-shm"):
            if Path(str(path) + suffix).exists():
                shutil.copy2(str(path) + suffix, str(copy) + suffix)
        connection = sqlite3.connect(copy)
        try:
            master = sorted(connection.execute("SELECT type,name,tbl_name,sql FROM sqlite_master"), key=repr)
            image = {"master": master,
                     "pragmas": {name: connection.execute(f"PRAGMA {name}").fetchone()[0]
                                 for name in ("user_version", "application_id", "page_size", "auto_vacuum", "encoding")}}
            virtual = [name for kind, name, _, sql in master if kind == "table" and (sql or "").upper().startswith("CREATE VIRTUAL")]
            for kind, name, _, sql in master:
                if kind != "table" or any(name == v + suffix for v in virtual for suffix in FTS_SHADOWS if suffix != "_config"):
                    continue
                if name in virtual:
                    # the index itself: every term with its row, column and position, whatever its segment layout
                    connection.execute(f"CREATE VIRTUAL TABLE temp.vocab USING fts5vocab(main,{sql_literal(name)},'instance')")
                    image["index " + name] = sorted(connection.execute("SELECT term,doc,col,offset FROM temp.vocab").fetchall())
                    connection.execute("DROP TABLE temp.vocab")
                    columns = [row[1] for row in connection.execute(f"PRAGMA table_xinfo({quote_name(name)})") if row[6] == 0]
                    image["text " + name] = sorted(connection.execute(
                        f"SELECT rowid,{','.join(quote_name(c) for c in columns)} FROM {quote_name(name)}").fetchall(), key=repr)
                else:
                    image["table " + name] = sorted(table_rows(connection, name)[1], key=repr)
            return image
        finally:
            connection.close()


def differing(left, right):
    return [key for key in sorted(set(left) | set(right)) if left.get(key) != right.get(key)]


FIXTURE_FORMAT = 1
STATE_SKIP = ("FileID/fileid.sqlite", "FileID/logs/", "home/", "Models/")


def fixture_files(ctx):
    """(area, path, relative path) of the library files and state files a fixture keeps:
    the state folder minus the catalog (kept as SQL), logs and the empty scratch folders."""
    for path in sorted(ctx.root.rglob("*")):
        if path.is_symlink():
            raise Failure(f"{path} is a symbolic link; a fixture holds none")
        if path.is_file():
            yield "tree", path, path.relative_to(ctx.root).as_posix()
    for path in sorted(ctx.state.rglob("*")):
        relative = path.relative_to(ctx.state).as_posix()
        if path.is_file() and not any(relative.startswith(skip) for skip in STATE_SKIP):
            yield "state", path, relative


CATALOG_HEADER = "-- The catalog the Swift engine wrote, as SQL (see README.md); placeholders expand when a case seeds it.\n"


def capture_fixture(ctx, case, target, copies):
    """Write the folder `target` from the closed-engine state of a run: the catalog as SQL,
    the library tree, the state files, and the values earlier steps saved."""
    target = Path(target)
    if target.exists():
        if not (target / "fixture.json").exists():
            raise Failure(f"{target} exists and is not a fixture folder")
        shutil.rmtree(target)
    target.mkdir(parents=True)
    templater = Templater(ctx)
    state_texts = {}
    for area, path, relative in fixture_files(ctx):
        if area == "state" and relative.endswith((".json", ".ndjson")):
            state_texts[relative] = path.read_text(encoding="utf-8")
            templater.add_paths(re.findall(r'"([^"]*)"', state_texts[relative].replace(JSON_SLASH, "/")))
    sql_text, pragmas = dump_catalog(ctx.database, templater)
    refuse_leftovers(sql_text, "catalog.sql")
    (target / "catalog.sql").write_text(CATALOG_HEADER + sql_text, encoding="utf-8")
    files = []
    for area, path, relative in fixture_files(ctx):
        stat = path.stat()
        entry = {"area": area, "path": relative, "mtimeNs": [stat.st_mtime_ns // 1_000_000_000, stat.st_mtime_ns % 1_000_000_000]}
        data = path.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        destination = target / area / relative
        if area == "tree" and digest in copies:
            entry["copy"] = copies[digest]
        elif relative in state_texts:
            text = templater.journal(state_texts[relative])
            refuse_leftovers(text, relative)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(text, encoding="utf-8", newline="")
            entry["template"] = True
        else:
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
        files.append(entry)
    saved = templater.data(json.loads(json.dumps(ctx.saved)))
    meta = {"format": FIXTURE_FORMAT, "capturedFrom": case["name"], "writtenBy": ctx.flavor, "conversation": ctx.conversation,
            "pragmas": pragmas, "vars": saved, "files": files,
            "dirs": [p.relative_to(ctx.root).as_posix() for p in sorted(ctx.root.rglob("*")) if p.is_dir()]}
    refuse_leftovers(json.dumps(meta), "fixture.json")
    (target / "fixture.json").write_text(json.dumps(meta, indent=1, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")


def repo_copies():
    """sha256 → repo path of the synthetic media files that cases copy into a library."""
    found = {}
    for folder in ("shared/test-corpus/conformance-media", "shared/test-corpus/ocr"):
        for path in sorted((ROOT / folder).rglob("*")):
            if path.is_file():
                found.setdefault(hashlib.sha256(path.read_bytes()).hexdigest(), path.relative_to(ROOT).as_posix())
    return found


def expand_text(ctx, text):
    """Placeholders inside a whole text, always as text (ctx.expand returns a number for a
    string that is one placeholder)."""
    return PLACEHOLDER.sub(lambda m: str(ctx.placeholder(m.group(1))), text)


def seed_fixture(ctx, name, omit=(), drop_lines=None):
    """Recreate a captured run before the engine's first start: library files with their
    modification times, state files, the catalog, and the values earlier steps saved."""
    base = FIXTURES / name
    meta = json.loads((base / "fixture.json").read_text(encoding="utf-8"))
    if meta["format"] != FIXTURE_FORMAT:
        raise Failure(f"fixture {name} has format {meta['format']}")
    (ctx.state / "FileID").mkdir(parents=True, exist_ok=True)
    for relative in meta["dirs"]:
        (ctx.root / relative).mkdir(parents=True, exist_ok=True)
    roots = {"tree": ctx.root, "state": ctx.state}
    for templated in (False, True):
        for entry in meta["files"]:
            if bool(entry.get("template")) != templated or (entry["area"] == "state" and entry["path"] in omit):
                continue
            destination = roots[entry["area"]] / entry["path"]
            destination.parent.mkdir(parents=True, exist_ok=True)
            source = base / entry["area"] / entry["path"]
            if "copy" in entry:
                destination.write_bytes((ROOT / entry["copy"]).read_bytes())
            elif templated:
                text = expand_text(ctx, source.read_text(encoding="utf-8"))
                for needle in (drop_lines or {}).get(entry["path"], []):
                    text = "".join(line for line in text.splitlines(keepends=True) if needle not in line)
                destination.write_text(text, encoding="utf-8", newline="")
            else:
                destination.write_bytes(source.read_bytes())
            seconds, nanoseconds = entry["mtimeNs"]
            os.utime(destination, ns=(seconds * 1_000_000_000 + nanoseconds,) * 2)
    sql_text = (base / "catalog.sql").read_text(encoding="utf-8")
    load_catalog(ctx.database, expand_text(ctx, sql_text), meta["pragmas"])
    ctx.saved.update(ctx.expand(meta["vars"]))
    ctx.conversation = meta["conversation"]
    return meta


def schema_image(path):
    """The objects (type, name, table) and, per table, the columns (name, type, not null, key) of a catalog."""
    with tempfile.TemporaryDirectory(prefix="FileIDSchema-") as temporary:
        copy = Path(temporary) / "c.sqlite"
        for suffix in ("", "-wal", "-shm"):
            if Path(str(path) + suffix).exists():
                shutil.copy2(str(path) + suffix, str(copy) + suffix)
        connection = sqlite3.connect(copy)
        try:
            objects = connection.execute("SELECT type,name,tbl_name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").fetchall()
            tables = {name: connection.execute("SELECT name,type,\"notnull\",pk FROM pragma_table_info(?)", (name,)).fetchall()
                      for kind, name, _ in objects if kind == "table"}
            return {"objects": [list(row) for row in objects], "columns": tables}
        finally:
            connection.close()


def same_schema_as(ctx, name):
    """Fail unless this run's catalog has the objects and columns of the catalog in fixture `name`."""
    with tempfile.TemporaryDirectory(prefix="FileIDConformance-", dir=ctx.state.parent.parent) as temporary:
        other = Context({"name": name}, ctx.flavor, ctx.os, Path(temporary))
        seed_fixture(other, name)
        wanted, actual = schema_image(other.database), schema_image(ctx.database)
    problems = []
    wanted_objects, actual_objects = {tuple(o) for o in wanted["objects"]}, {tuple(o) for o in actual["objects"]}
    problems += [f"missing {kind} {object_name}" for kind, object_name, _ in sorted(wanted_objects - actual_objects)]
    problems += [f"extra {kind} {object_name}" for kind, object_name, _ in sorted(actual_objects - wanted_objects)]
    for table, columns in wanted["columns"].items():
        if table in actual["columns"] and [list(c) for c in columns] != [list(c) for c in actual["columns"][table]]:
            problems.append(f"table {table} has other columns")
    if problems:
        raise Failure(f"the catalog is not the {name} schema: " + "; ".join(problems[:12]))


def fixture_tree(path):
    return {p.relative_to(path).as_posix(): p.read_bytes() for p in sorted(Path(path).rglob("*")) if p.is_file()}


def verify_fixture(name, parent, os_name):
    """Seed a fixture into a scratch library and capture it again: the result must be the
    same folder, so no placeholder is lost or invented. Needs no engine."""
    with tempfile.TemporaryDirectory(prefix="FileIDConformance-", dir=parent) as temporary:
        ctx = Context({"name": name}, "swift", os_name, Path(temporary))
        meta = seed_fixture(ctx, name)
        ctx.saved = ctx.expand(meta["vars"])
        again = Path(temporary) / "again"
        capture_fixture(ctx, {"name": meta["capturedFrom"]}, again, repo_copies())
        left, right = fixture_tree(FIXTURES / name), fixture_tree(again)
        same_forms = str(ctx.root) == foundation_standardized(str(ctx.root))
        if same_forms:
            for tree in (left, right):
                for key, data in tree.items():
                    tree[key] = data.replace(b"{std_root", b"{root").replace(b"{std_state", b"{state")
        problems = [key for key in sorted(set(left) | set(right)) if left.get(key) != right.get(key)]
        if problems:
            raise Failure(f"fixture {name} does not capture back to itself: {', '.join(problems)}")
        return same_forms


def check_catalog_round_trip(ctx, directory):
    """The captured SQL, expanded here, must rebuild the live catalog exactly."""
    meta = json.loads((Path(directory) / "fixture.json").read_text(encoding="utf-8"))
    sql_text = expand_text(ctx, (Path(directory) / "catalog.sql").read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix="FileIDRoundTrip-") as temporary:
        rebuilt = Path(temporary) / "fileid.sqlite"
        load_catalog(rebuilt, sql_text, meta["pragmas"])
        problems = differing(catalog_image(ctx.database), catalog_image(rebuilt))
    if problems:
        raise Failure(f"the dump does not rebuild the catalog: {', '.join(problems)}")


FIXTURE_FOLDER = Path("/private/tmp/fileid-conformance-fixture")
PINNED_FIXTURES = CASES / "fixtures/swift-written"


def lay_out_fixture(ctx):
    """State the Swift engine wrote, at the paths it was captured under: the case's
    generated tree (no catalog rows: the fixture's catalog has them), then the files
    the Swift run added or changed, the ones it removed, every file's modification
    time, the chat conversation id, and the file identities of the replayed files (inodes cannot be committed)."""
    source = PINNED_FIXTURES / ctx.case["fixture"]
    manifest = json.loads((source / "fixture.json").read_text(encoding="utf-8"))
    build_tree(ctx, ctx.case.get("tree", []))
    if (source / "tree").is_dir():
        shutil.copytree(source / "tree", ctx.root, dirs_exist_ok=True)
    for relative in manifest["removed"]:
        (ctx.root / relative).unlink()
    for relative, (seconds, nanoseconds) in manifest["mtimes"].items():
        os.utime(ctx.root / relative, ns=(seconds * 1_000_000_000 + nanoseconds,) * 2)
    shutil.copytree(source / "state", ctx.state, dirs_exist_ok=True)
    ctx.saved.update(manifest["vars"])
    ctx.conversation = manifest.get("conversation", ctx.conversation)
    with connect(ctx.database) as database:
        for row_id, path in database.execute("SELECT id, path_text FROM files").fetchall():
            if os.path.exists(path):
                database.execute("UPDATE files SET file_ref = ? WHERE id = ?", (os.stat(path).st_ino, row_id))


@contextlib.contextmanager
def case_directory(case, parent):
    if "fixture" not in case or not (PINNED_FIXTURES / case["fixture"]).is_dir():
        with tempfile.TemporaryDirectory(prefix="FileIDConformance-", dir=parent) as temporary:
            yield Path(temporary)
        return
    directory = FIXTURE_FOLDER / case["name"]
    shutil.rmtree(directory, ignore_errors=True)
    directory.mkdir(parents=True)
    try:
        yield directory
    finally:
        shutil.rmtree(directory, ignore_errors=True)


def run_case(case, flavor, binary, os_name, parent):
    with case_directory(case, parent) as directory:
        ctx = Context(case, flavor, os_name, directory)
        if case.get("model"):
            stage_model(ctx, model_source(case))
        if "fixture" in case and (PINNED_FIXTURES / case["fixture"]).is_dir():
            lay_out_fixture(ctx)
        elif "fixture" in case:
            seed_fixture(ctx, case["fixture"], case.get("fixtureOmit", ()), case.get("fixtureDropLines"))
        else:
            start(binary, ctx).close()
            rows = build_tree(ctx, case.get("tree", []))
            if rows:
                insert_rows(ctx, rows)
        if case.get("sql"):
            with connect(ctx.database) as database:
                for statement in case["sql"]:
                    database.execute(ctx.expand(statement))
        engine = start(binary, ctx)
        try:
            for index, step in enumerate(case["steps"]):
                label = step.get("label", f"step {index + 1}")
                try:
                    engine = run_step(engine, ctx, step, binary)
                except (Failure, TimeoutError, RuntimeError) as error:
                    raise Failure(f"{label}: {error}") from None
        finally:
            if engine:
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


def is_app_hosted(binary, parent):
    """Whether the engine writes its events on stderr, as the Rust engine the Mac app
    hosts does."""
    with tempfile.TemporaryDirectory(prefix="FileIDConformance-", dir=parent) as temporary:
        engine = Engine(binary, temporary)
        try:
            engine.wait("ready")
            return engine.stream.get("ready") == "stderr"
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
    parser.add_argument("--engine", type=Path, help="engine binary under test (not needed with --verify-fixture)")
    parser.add_argument("--flavor", choices=("auto", "rust"), default="auto")
    parser.add_argument("--case", action="append", default=[], help="glob of case names to run")
    parser.add_argument("--os", default=host_os(), help=argparse.SUPPRESS)
    parser.add_argument("--fixture-parent", type=Path, default=Path(tempfile.gettempdir()))
    parser.add_argument("--test-trash-root", action="store_true",
                        help="the engine was built with a test Trash root (see above)")
    parser.add_argument("--verify-fixture", action="append", default=[], metavar="NAME",
                        help="seed fixture NAME (a folder under shared/test-corpus/swift-written) in a scratch library, "
                             "capture it again and require the same folder; needs no engine")
    args = parser.parse_args()
    global TEST_TRASH_ROOT
    TEST_TRASH_ROOT = args.test_trash_root
    parent = safe_parent(args.fixture_parent)
    if args.verify_fixture:
        failed = 0
        for name in args.verify_fixture:
            try:
                same_forms = verify_fixture(name, parent, args.os)
                print(f"VERIFIED {name}" + (" (the scratch folder has no /private form; std tokens compared as plain)" if same_forms else ""))
            except Failure as failure:
                failed += 1
                print(f"FAIL   {name}: {failure}")
        return 1 if failed else 0
    if not args.engine:
        parser.error("--engine is required unless --verify-fixture is given")
    binary = args.engine.resolve(strict=True)
    flavor = "rust"
    hosted_engine = is_app_hosted(binary, parent)
    counts = {"pass": 0, "fail": 0, "known": 0, "stale": 0, "skip": 0}
    symlinks_ok = can_symlink(parent)
    print(f"conformance: {flavor} engine {binary} on {args.os}")
    for case in load_cases(args.case):
        name = case["name"]
        trash_root = case.get("requiresTestTrashRoot")
        needs_trash_root = not TEST_TRASH_ROOT and (args.os in trash_root if isinstance(trash_root, list) else bool(trash_root))
        needs_symlinks = bool(case.get("requiresSymlinks")) and not symlinks_ok
        no_model = case.get("model") and model_source(case) is None
        unhosted = case.get("appHosted") and not hosted_engine
        needs_peer = bool(case.get("requires") or case.get("start")) or "rust" not in case.get("engines", ["rust"])
        if needs_peer or needs_trash_root or unhosted or needs_symlinks or no_model or args.os not in case.get("os", [args.os]):
            counts["skip"] += 1
            reason = ("needs the retired Swift engine" if needs_peer
                      else "needs a test-trash-root engine build (--test-trash-root)" if needs_trash_root
                      else "needs a runner account that may create symlinks" if needs_symlinks
                      else "needs the engine the Mac app hosts" if unhosted
                      else f"model {case['model']['repo']} is not installed" if no_model
                      else "not applicable here")
            print(f"SKIP   {name} ({reason})")
            continue
        divergence = divergence_for(case, flavor, args.os)
        if divergence and divergence.get("run") is False:
            counts["known"] += 1
            print(f"KNOWN  {name} [{flavor}] not run: {divergence['reason']}")
            continue
        try:
            run_case(case, flavor, binary, args.os, parent)
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
    for stream in (sys.stdout, sys.stderr):
        with contextlib.suppress(AttributeError, ValueError, OSError):
            stream.reconfigure(encoding="utf-8", errors="replace")
    sys.exit(main())
