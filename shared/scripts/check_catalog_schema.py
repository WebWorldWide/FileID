#!/usr/bin/env python3
from pathlib import Path

root = Path(__file__).resolve().parents[2]
versions = [21,22]
sqls = [(version,(root / f"shared/catalog/v{version}.sql").read_text()) for version in versions]
swift = (root / "platforms/apple/engine/Sources/FileIDEngine/Storage/CatalogSchema.swift").read_text()
expected = 'enum CatalogSchema {\n' + ''.join(f'    static let v{version} = #"""\n' + sql + '"""#\n' for version,sql in sqls) + '}\n'
if swift != expected:
    raise SystemExit("Catalog SQL drift: regenerate CatalogSchema.swift from canonical SQL")
rust = (root / "platforms/windows/src/engine/src/db/migrations.rs").read_text()
for version in versions:
    if f'include_str!("../../../../../../shared/catalog/v{version}.sql")' not in rust:
        raise SystemExit(f"Rust migration must include canonical v{version} SQL")
print("Catalog migration sources match")
