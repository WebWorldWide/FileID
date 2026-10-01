#!/usr/bin/env python3
"""Verify local chat history and catalog retrieval across actual native engines."""
import argparse
import hashlib
import sqlite3
import tempfile
import uuid
from pathlib import Path
from check_catalog_roundtrip import Engine


def chat(engine, conversation, action, **fields):
    request_id = str(uuid.uuid4())
    engine.send({"chatRequest": {"request": {"requestID": request_id, "conversationID": conversation, "action": action, **fields}}})
    while True:
        result = engine.wait("chatResponse", request_id)
        if result["status"] == "error":
            raise RuntimeError(result["message"])
        if result["status"] in ("completed", "cancelled"):
            return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--swift-engine", type=Path, required=True)
    parser.add_argument("--rust-engine", type=Path, required=True)
    args = parser.parse_args()
    root = Path(tempfile.gettempdir()).resolve()
    if str(root).casefold() == "/volumes/adlon" or str(root).casefold().startswith("/volumes/adlon/"):
        raise RuntimeError("Chat fixtures must remain internal")
    with tempfile.TemporaryDirectory(prefix="FileIDChat-", dir=root) as temporary:
        directory = Path(temporary)
        conversation = str(uuid.uuid4())
        engine = Engine(args.swift_engine.resolve(), directory, swift=True)
        try:
            engine.wait("ready")
        finally:
            engine.close()
        source = directory / "birthday.txt"
        source.write_text("Birthday gift opening", encoding="utf-8")
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        database = directory / "FileID/fileid.sqlite"
        with sqlite3.connect(database) as db:
            db.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(1,?,1,21,0,'doc','txt','Birthday gift opening')", (str(source),))
            db.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(2,'/offline/birthday.mov',2,21,0,'video','mov','Birthday gift opening'),(3,'/offline/birthday.jpg',3,21,0,'image','jpg','Birthday gift opening')")
        for binary, swift in [(args.swift_engine, True), (args.rust_engine, False)]:
            engine = Engine(binary.resolve(), directory, swift=swift)
            try:
                engine.wait("ready")
                if not swift:
                    assert len(chat(engine, conversation, "history")["messages"]) == 2
                result = chat(engine, conversation, "send", text="Please find my birthday files" if swift else "only videos", useModel=False)
                if swift:
                    assert {hit["fileID"] for hit in result["hits"]} == {1, 2, 3}
                else:
                    assert [hit["fileID"] for hit in result["hits"]] == [2]
                    assert "birthday" in result["message"]
                assert len(result["messages"]) == (2 if swift else 4)
            finally:
                engine.close()
        engine = Engine(args.swift_engine.resolve(), directory, swift=True)
        try:
            engine.wait("ready")
            assert len(chat(engine, conversation, "history")["messages"]) == 4
            result = chat(engine, conversation, "send", text="now photos", useModel=False)
            assert [hit["fileID"] for hit in result["hits"]] == [3]
            assert "birthday" in result["message"]
            assert len(result["messages"]) == 6
            assert chat(engine, conversation, "clear")["messages"] == []
        finally:
            engine.close()
        assert hashlib.sha256(source.read_bytes()).hexdigest() == digest
        with sqlite3.connect(database) as db:
            assert db.execute("SELECT COUNT(*) FROM catalog_chat").fetchone()[0] == 0
            assert db.execute("SELECT COUNT(*) FROM files").fetchone()[0] == 3
        print("Swift → Rust → Swift local chat, contextual media filters, and history deletion passed")


if __name__ == "__main__":
    main()
