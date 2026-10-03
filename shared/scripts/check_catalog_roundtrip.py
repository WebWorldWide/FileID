#!/usr/bin/env python3
"""Exercise one internal temporary catalog through native Swift and Rust engines."""
import argparse
import json
import os
from pathlib import Path
import queue
import sqlite3
import subprocess
import tempfile
import threading
import time
import uuid


class Engine:
    def __init__(self, binary, directory, swift):
        environment = dict(os.environ)
        environment.update({
            "FILEID_DATABASE_PATH": str(directory / "FileID/fileid.sqlite"),
            "XDG_DATA_HOME": str(directory),
            "LOCALAPPDATA": str(directory),
        })
        self.process = subprocess.Popen(
            [str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, env=environment, text=True, encoding="utf-8", bufsize=1,
        )
        self.events = queue.Queue()
        stream = self.process.stderr if swift else self.process.stdout
        other = self.process.stdout if swift else self.process.stderr

        def read_events():
            for line in stream:
                try:
                    event = json.loads(line)
                    if "payload" in event:
                        self.events.put(event["payload"])
                except (json.JSONDecodeError, TypeError):
                    continue

        threading.Thread(target=read_events, daemon=True).start()
        def drain():
            for _ in other:
                pass
        threading.Thread(target=drain, daemon=True).start()

    def wait(self, name, request_id=None):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                payload = self.events.get(timeout=min(1, max(0.01, deadline - time.monotonic())))
            except queue.Empty:
                if self.process.poll() is not None:
                    raise RuntimeError(f"Engine exited with {self.process.returncode}")
                continue
            if name in payload:
                result = payload[name].get("_0", payload[name])
                if request_id is None or result.get("requestID") == request_id:
                    return result
            if "error" in payload:
                error = payload["error"].get("_0", payload["error"])
                if error.get("kind") in {"db_open_failed", "db_newer_than_engine"}:
                    raise RuntimeError(error)
        raise TimeoutError(f"Engine did not emit {name}")

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
                self.send({"shutdown": {}})
                self.process.wait(timeout=10)
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            for stream in [self.process.stdin, self.process.stdout, self.process.stderr]:
                stream.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--swift-engine", type=Path, required=True)
    parser.add_argument("--rust-engine", type=Path, required=True)
    args = parser.parse_args()
    for binary in [args.swift_engine, args.rust_engine]:
        if not binary.is_file():
            parser.error(f"Engine binary not found: {binary}")
    temp_root = Path(tempfile.gettempdir()).resolve()
    if str(temp_root).casefold() == "/volumes/adlon" or str(temp_root).casefold().startswith("/volumes/adlon/"):
        raise RuntimeError("Temporary fixtures must not be placed on Adlon")
    with tempfile.TemporaryDirectory(prefix="FileIDCatalogRoundtrip-", dir=temp_root) as temporary:
        directory = Path(temporary)
        engine = Engine(args.swift_engine.resolve(), directory, swift=True)
        try:
            engine.wait("ready")
        finally:
            engine.close()
        with sqlite3.connect(directory / "FileID/fileid.sqlite") as database:
            database.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,'/internal/Family Birthday.mov',1,100,10,0,'video','mov')")
            database.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(2,'/internal/Portrait.jpg',2,100,10,0,'image','jpg')")
            database.execute("INSERT INTO persons(id,name,created_at) VALUES(7,'Confirmed Person',0)")
            database.execute("INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(2,'fixture-revision','fixture',0)")
            vector = b"\x00\x00\x80\x3f" + bytes(127 * 4)
            database.execute("INSERT INTO face_prints(id,file_id,print_data,bbox,person_id,arcface_embedding,embedding_model,processing_version,source_revision) VALUES(4,2,X'00','0.1,0.1,0.2,0.2',7,?,'fixture-weight-hash','fixture-alignment','fixture-revision')", (vector,))
            database.execute("UPDATE catalog_observations SET user_edited=1,region_json='manual face marker' WHERE id='faceprint:4'")
        chapter = dict(id="gift", fileID=1, startSeconds=8.0, endSeconds=12.0, title="Gift Opening", summary="Grandma opens presents", sourceRevision="untrusted", modelVersion="untrusted", confidence=0.0, userEdited=False, stale=True)
        engine = Engine(args.swift_engine.resolve(), directory, swift=True)
        try:
            engine.wait("ready")
            assert engine.request("saveChapter", chapter=chapter)["chapters"][0]["userEdited"]
        finally:
            engine.close()
        engine = Engine(args.rust_engine.resolve(), directory, swift=False)
        try:
            engine.wait("ready")
            assert engine.request("search", query="Grandma presents")["hits"][0]["startSeconds"] == 8.0
            chapter.update(title="Gift Unwrapped", startSeconds=10.0)
            assert engine.request("saveChapter", chapter=chapter)["chapters"][0]["startSeconds"] == 10.0
        finally:
            engine.close()
        engine = Engine(args.swift_engine.resolve(), directory, swift=True)
        try:
            engine.wait("ready")
            restored = engine.request("undoChapterEdit", fileID=1)["chapters"][0]
            assert restored["startSeconds"] == 8.0 and restored["title"] == "Gift Opening"
            assert not restored["stale"] and restored["modelVersion"] == "user"
        finally:
            engine.close()
        with sqlite3.connect(directory / "FileID/fileid.sqlite") as database:
            assert database.execute("SELECT name FROM persons WHERE id=7").fetchone() == ("Confirmed Person",)
            assert database.execute("SELECT person_id,embedding_model,processing_version,source_revision,length(arcface_embedding) FROM face_prints WHERE id=4").fetchone() == (7,"fixture-weight-hash","fixture-alignment","fixture-revision",512)
            assert database.execute("SELECT region_json,user_edited,stale FROM catalog_observations WHERE id='faceprint:4'").fetchone() == ("manual face marker",1,0)
            assert database.execute("SELECT model,dimension,vector FROM catalog_embeddings WHERE entity_id='faceprint:4'").fetchone() == ("fixture-weight-hash|fixture-alignment",128,vector)
    print("Swift → Rust → Swift catalog, evidence search, face provenance, and correction Undo round trip passed")


if __name__ == "__main__":
    main()
