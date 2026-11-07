#!/usr/bin/env python3
"""Model-free checks of a staged llama.cpp runtime: version, checksums, no outbound HTTP."""

from __future__ import annotations

import argparse
import hashlib
import http.server
import os
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

BUILD = "9254"
COMMIT = "e947228222147356bc7e64154d3439e142481632"


def check_sums(runtime: Path) -> list[str]:
    failures = []
    listed = set()
    for line in (runtime / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        digest, name = line.split("  ", 1)
        listed.add(name)
        path = runtime / name
        if path.is_symlink() or not path.is_file():
            failures.append(f"{name}: not a regular file")
        elif hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            failures.append(f"{name}: checksum mismatch")
    regular = {p.name for p in runtime.iterdir() if p.is_file() and not p.is_symlink()} - {"SHA256SUMS"}
    if regular != listed:
        failures.append(f"SHA256SUMS does not list exactly the staged files: {sorted(regular ^ listed)}")
    for link in (p for p in runtime.iterdir() if p.is_symlink()):
        target = os.readlink(link)
        if "/" in target or not (runtime / target).is_file():
            failures.append(f"{link.name}: link leaves the runtime directory")
    return failures


def run(server: Path, args: list[str], scratch: Path) -> subprocess.CompletedProcess[str]:
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": str(scratch),
        "XDG_CACHE_HOME": str(scratch / "cache"),
        "LLAMA_CACHE": str(scratch / "cache"),
    }
    return subprocess.run(
        [str(server), *args], stdin=subprocess.DEVNULL, capture_output=True,
        text=True, timeout=60, env=env, cwd=scratch,
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("runtime", type=Path)
    args = parser.parse_args()
    runtime = args.runtime.resolve()
    server = runtime / "llama-server"
    failures = check_sums(runtime)
    for notice in ("LICENSE", "THIRD-PARTY-NOTICES.txt"):
        if not (runtime / notice).is_file():
            failures.append(f"missing {notice}")

    captured: list[str] = []

    class Trap(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            captured.append(self.path)
            self.send_response(404)
            self.end_headers()

        do_HEAD = do_GET

        def log_message(self, *_: object) -> None:
            pass

    trap = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Trap)
    thread = threading.Thread(target=trap.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="fileid-llama-probe-") as raw:
            scratch = Path(raw)
            version = run(server, ["--version"], scratch)
            text = version.stdout + version.stderr
            if version.returncode != 0 or f"version: {BUILD} ({COMMIT})" not in text:
                failures.append(f"--version: exit {version.returncode}: {text.strip()[:400]}")
            target = scratch / "never-downloaded.gguf"
            url = f"http://127.0.0.1:{trap.server_port}/model.gguf"
            fetch = run(server, ["-mu", url, "-m", str(target), "--host", "127.0.0.1", "--port", "0"], scratch)
            if fetch.returncode == 0:
                failures.append("remote model acquisition did not fail")
            if target.exists():
                failures.append("remote model file was written")
    finally:
        trap.shutdown()
        trap.server_close()
        thread.join(timeout=5)
    if captured:
        failures.append(f"outbound HTTP reached the trap: {captured}")
    if failures:
        print("llama.cpp runtime probe failed:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    print(f"llama.cpp runtime probe passed: b{BUILD} {COMMIT[:8]}, checksums match, outbound HTTP refused.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
