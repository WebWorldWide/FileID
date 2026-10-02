#!/usr/bin/env python3
"""Verify command-channel health correlation against an actual engine process."""

import argparse
from pathlib import Path
import queue
import tempfile
import time

from check_catalog_roundtrip import Engine


def next_reply(engine):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            payload = engine.events.get(timeout=min(1, max(0.01, deadline - time.monotonic())))
        except queue.Empty:
            if engine.process.poll() is not None:
                raise RuntimeError(f"Engine exited with {engine.process.returncode}")
            continue
        if "log" in payload:
            continue
        assert len(payload) == 1, payload
        return payload
    raise TimeoutError("Engine did not acknowledge the health probe")


def verify(binary, swift):
    root = Path(tempfile.gettempdir()).resolve()
    if str(root).casefold().startswith(("/volumes/adlon", "/srv/data")) or any(part.casefold() == "adlon" for part in root.parts):
        raise RuntimeError("Health fixtures must remain internal")
    with tempfile.TemporaryDirectory(prefix="FileIDHealth-", dir=root) as temporary:
        engine = Engine(binary.resolve(), Path(temporary), swift=swift)
        try:
            ready = engine.wait("ready")
            assert ready["pid"] == engine.process.pid, ready
            nonces = ["probe-1", "GEN_4-probe_2", "a" * 128]
            for nonce in nonces:
                engine.send({"healthCheck": {"requestID": nonce}})
            for nonce in nonces:
                payload = next_reply(engine)
                assert set(payload) == {"healthCheckResult"}, payload
                assert payload["healthCheckResult"] == {"_0": {"requestID": nonce, "pid": engine.process.pid}}, payload

            for nonce in ["", " ", "../file", "line\nfeed", "é", "a\0b", "a" * 129]:
                engine.send({"healthCheck": {"requestID": nonce}})
                payload = next_reply(engine)
                assert set(payload) == {"error"}, payload
                error = payload["error"]["_0"]
                assert error["kind"] == "invalid_health_request", error
                assert error.get("path") is None, error
                assert error["message"] == "Health request ID must contain 1–128 ASCII letters, digits, underscores or hyphens.", error

            engine.send({"healthCheck": {"requestID": "after-invalid"}})
            assert next_reply(engine) == {"healthCheckResult": {"_0": {"requestID": "after-invalid", "pid": engine.process.pid}}}
        finally:
            engine.close()
        assert engine.process.returncode == 0, engine.process.returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--swift", action="store_true")
    args = parser.parse_args()
    verify(args.engine, args.swift)
    print("Health probes preserve nonce/PID, reject invalid IDs and keep the command channel responsive")


if __name__ == "__main__":
    main()
