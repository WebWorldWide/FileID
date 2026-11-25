#!/usr/bin/env python3
from pathlib import Path

root = Path(__file__).resolve().parents[2]
versions = [21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39]
rust = (root / "platforms/windows/src/engine/src/db/migrations.rs").read_text()
for version in versions:
    if not (root / f"shared/catalog/v{version}.sql").is_file():
        raise SystemExit(f"Canonical v{version} SQL is missing")
    if f'include_str!("../../../../../../shared/catalog/v{version}.sql")' not in rust:
        raise SystemExit(f"Rust migration must include canonical v{version} SQL")
print("Catalog migration sources match")
