#!/usr/bin/env python3
"""Isolated FileIDEngine driver for end-to-end checks. The engine gets FILEID_DATABASE_PATH inside a
throwaway directory, so a run never touches the real catalog. Set FILEID_ENGINE to the engine binary."""
import json, os, subprocess, sys, threading, time, uuid, queue

ENGINE = os.environ.get("FILEID_ENGINE") or os.path.expanduser("~/Library/Developer/FileIDBuild/debug/FileIDEngine")


class Engine:
    def __init__(self, workdir, binary=None, extra_env=None):
        os.makedirs(workdir, exist_ok=True)
        self.db = os.path.join(workdir, "fileid.sqlite")
        env = dict(os.environ, FILEID_DATABASE_PATH=self.db, **(extra_env or {}))
        self.p = subprocess.Popen([binary or ENGINE], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, env=env, text=True, bufsize=1)
        self.events = queue.Queue()
        self.log = []
        for stream in (self.p.stderr, self.p.stdout):
            threading.Thread(target=self._pump, args=(stream,), daemon=True).start()

    def _pump(self, stream):
        for line in stream:
            line = line.strip()
            if not line.startswith("{"):
                continue
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if "payload" in event:
                self.events.put(event["payload"])
                self.log.append(event["payload"])

    def send(self, name, body=None):
        command = {"id": str(uuid.uuid4()), "payload": {name: body if body is not None else {}}}
        self.p.stdin.write(json.dumps(command) + "\n")
        self.p.stdin.flush()
        return command["id"]

    def wait(self, predicate, timeout=600, on_event=None):
        end = time.time() + timeout
        while time.time() < end:
            try:
                payload = self.events.get(timeout=1)
            except queue.Empty:
                if self.p.poll() is not None:
                    raise RuntimeError("engine exited with %s" % self.p.returncode)
                continue
            if on_event:
                on_event(payload)
            if predicate(payload):
                return payload
        raise TimeoutError("timed out")

    def close(self):
        try:
            self.send("shutdown")
        except Exception:
            pass
        try:
            self.p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.p.kill()


def kind(payload):
    return next(iter(payload))
