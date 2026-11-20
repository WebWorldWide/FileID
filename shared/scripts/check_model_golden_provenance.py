#!/usr/bin/env python3
"""Every model golden must come from the reference runtime, not from tract itself."""

import json
from pathlib import Path
import sys

REFERENCE_GENERATOR_PREFIX = "onnxruntime"


def check(golden_dir: Path) -> list[str]:
    paths = sorted(golden_dir.glob("*.json"))
    if not paths:
        return [f"{golden_dir}: no golden files found"]
    failures = []
    for path in paths:
        try:
            generator = json.loads(path.read_text()).get("generator")
        except (OSError, ValueError, AttributeError) as error:
            failures.append(f"{path.name}: unreadable golden ({error})")
            continue
        if not isinstance(generator, str) or not generator.startswith(REFERENCE_GENERATOR_PREFIX):
            failures.append(f"{path.name}: generator {generator!r} does not start with {REFERENCE_GENERATOR_PREFIX!r}")
    return failures


if __name__ == "__main__":
    errors = check(Path(__file__).resolve().parents[1] / "test-corpus" / "model-golden")
    if errors:
        print("Model golden provenance failed:\n  " + "\n  ".join(errors))
        sys.exit(1)
    print("Model golden provenance passed.")
