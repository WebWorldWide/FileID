#!/usr/bin/env python3
"""Verify actual export previews, execution, and Undo across both native engines."""
import argparse
import hashlib
import sqlite3
import struct
import tempfile
from pathlib import Path
import zlib
from check_catalog_roundtrip import Engine


def tool(engine, action, **fields):
    import uuid
    request_id = str(uuid.uuid4())
    engine.send({"toolRequest": {"request": {"requestID": request_id, "action": action, **fields}}})
    result = engine.wait("toolResponse", request_id)
    if result["status"] != "ok":
        raise RuntimeError(result["message"])
    return result


def png():
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 16, 8, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress((b"\x00" + b"\xff\x00\x00" * 16) * 8)) + chunk(b"IEND", b"")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--swift-engine", type=Path, required=True)
    parser.add_argument("--rust-engine", type=Path, required=True)
    args = parser.parse_args()
    root = Path(tempfile.gettempdir()).resolve()
    if str(root).casefold() == "/volumes/adlon" or str(root).casefold().startswith("/volumes/adlon/"):
        raise RuntimeError("Temporary fixtures must not be placed on Adlon")
    with tempfile.TemporaryDirectory(prefix="FileIDToolsRoundtrip-", dir=root) as temporary:
        directory = Path(temporary)
        source = directory / "Picture.png"
        source.write_bytes(png())
        original = hashlib.sha256(source.read_bytes()).hexdigest()
        engine = Engine(args.swift_engine.resolve(), directory, swift=True)
        try:
            engine.wait("ready")
        finally:
            engine.close()
        with sqlite3.connect(directory / "FileID/fileid.sqlite") as db:
            db.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,?,1,?,?,0,'image','png')", (str(source), source.stat().st_size, source.stat().st_mtime))
        for preview_swift, execute_swift, output_format in [(True, False, "jpeg"), (False, True, "tiff")]:
            engine = Engine((args.swift_engine if preview_swift else args.rust_engine).resolve(), directory, swift=preview_swift)
            try:
                engine.wait("ready")
                capabilities = tool(engine, "capabilities")["capabilities"]
                assert any(c["id"] == "photo" and c["available"] for c in capabilities)
                preview = tool(engine, "preview", fileIDs=[1], destination=str(directory), recipe={"kind": "photo", "format": output_format, "maxDimension": 16})
            finally:
                engine.close()
            operation_id = preview["operationID"]
            output = Path(preview["outputs"][0]["outputPath"])
            assert not output.exists()
            engine = Engine((args.swift_engine if execute_swift else args.rust_engine).resolve(), directory, swift=execute_swift)
            try:
                engine.wait("ready")
                exported = tool(engine, "execute", operationID=operation_id)
                assert exported["outputs"][0]["state"] == "completed" and output.is_file()
            finally:
                engine.close()
            assert hashlib.sha256(source.read_bytes()).hexdigest() == original
            engine = Engine((args.swift_engine if preview_swift else args.rust_engine).resolve(), directory, swift=preview_swift)
            try:
                engine.wait("ready")
                assert tool(engine, "history")["operationID"] == operation_id
                undone = tool(engine, "undo", operationID=operation_id)
                assert undone["outputs"][0]["state"] == "undone" and not output.exists()
            finally:
                engine.close()
            assert hashlib.sha256(source.read_bytes()).hexdigest() == original
            with sqlite3.connect(directory / "FileID/fileid.sqlite") as db:
                assert db.execute("SELECT COUNT(*) FROM catalog_assets").fetchone()[0] == 0
        print("Swift/Rust export preview → cross-engine execution → persistent Undo passed")


if __name__ == "__main__":
    main()
