#!/usr/bin/env python3
import json
from pathlib import Path
import unittest

CASES = Path(__file__).resolve().parents[1] / "test-corpus/conformance"
TRASH_COMMANDS = ('"trashFiles"', '"restoreFromTrash"', '"emptyTrash"')


def guarded_on_windows(case):
    if "windows" not in case.get("os", ["windows"]):
        return True
    return any(
        item.get("engine") == "rust" and item.get("run") is False and "windows" in item.get("os", [])
        for item in case.get("knownDivergences", [])
    )


class TrashCasesStayOffTheRecycleBin(unittest.TestCase):
    def test_every_case_that_trashes_has_the_windows_guard(self):
        unguarded = []
        trashing = 0
        for path in sorted(CASES.glob("*.json")):
            for case in json.loads(path.read_text(encoding="utf-8")):
                steps = json.dumps(case.get("steps", []))
                if any(command in steps for command in TRASH_COMMANDS):
                    trashing += 1
                    if not guarded_on_windows(case):
                        unguarded.append(f"{path.name}: {case['name']}")
        self.assertGreater(trashing, 0)
        self.assertEqual(unguarded, [], "Trash cases would use the real Recycle Bin on Windows CI")

    def test_guard_detects_a_missing_block(self):
        self.assertFalse(guarded_on_windows({"name": "x"}))
        self.assertTrue(guarded_on_windows({"knownDivergences": [{"engine": "rust", "os": ["windows"], "run": False}]}))
        self.assertTrue(guarded_on_windows({"os": ["darwin"]}))


if __name__ == "__main__":
    unittest.main()
