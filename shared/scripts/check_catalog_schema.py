#!/usr/bin/env python3
from pathlib import Path

root = Path(__file__).resolve().parents[2]
sql = (root / "shared/catalog/v21.sql").read_text()
swift = (root / "platforms/apple/engine/Sources/FileIDEngine/Storage/CatalogSchema.swift").read_text()
expected = 'enum CatalogSchema {\n    static let v21 = #"""\n' + sql + '"""#\n}\n'
if swift != expected:
    raise SystemExit("Catalog SQL drift: regenerate CatalogSchema.swift from shared/catalog/v21.sql")
rust = (root / "platforms/windows/src/engine/src/db/migrations.rs").read_text()
if 'include_str!("../../../../../../shared/catalog/v21.sql")' not in rust:
    raise SystemExit("Rust migration must include the canonical v21 SQL")
print("Catalog migration sources match")
