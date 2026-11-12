#!/usr/bin/env python3
"""Tests for check_app_command_coverage.py on small synthetic repositories."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_app_command_coverage as coverage  # noqa: E402

PROTOCOL = """public struct IPCCommand {
    public enum Payload: Codable, Sendable {
        case pauseScan
        case catalogRequest(request: CatalogRequest)
        case wipeLibrary
    }
}
"""

SCHEMA = {"$defs": {
    "CatalogRequest": {"properties": {"action": {"enum": ["search", "jobs"]}}},
    "ToolRequest": {"properties": {"action": {"enum": ["capabilities"]}}},
    "ChatRequest": {"properties": {"action": {"enum": ["send"]}}},
    "ToolRecipe": {"properties": {"kind": {"enum": ["photo"]}}},
}}

APP = """func run(engine: EngineClient) {
    engine.send(.pauseScan)
    engine.send(.catalogRequest(request: CatalogRequest(requestID: "x", action: "search")))
}
"""

SERVER = """match cmd.payload {
    CommandPayload::PauseScan(_) => { pause() }
    CommandPayload::CatalogRequest(payload) => { catalog(payload) }
    CommandPayload::WipeLibrary(_) => {}
}
"""

CASE = [{"name": "controls", "steps": [
    {"command": {"pauseScan": {}}},
    {"catalog": {"action": "search", "query": "x"}},
]}]


class CoverageTests(unittest.TestCase):
    def repo(self, app=APP, server=SERVER, cases=CASE, catalog='"search" => {}'):
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        files = {
            coverage.PROTOCOL: PROTOCOL,
            coverage.SCHEMA: json.dumps(SCHEMA),
            f"{coverage.APP_SOURCES}/FileID/App.swift": app,
            f"{coverage.APP_SOURCES}/FileID/App 2.swift": "engine.send(.wipeLibrary)",
            f"{coverage.ENGINE}/server.rs": server,
            f"{coverage.ENGINE}/commands/catalog.rs": catalog,
            f"{coverage.CASES}/controls.json": json.dumps(cases),
        }
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_served_and_exercised_commands_pass(self):
        root = self.repo()
        self.assertEqual(coverage.reachable(root), {("command", "pauseScan"), ("command", "catalogRequest"), ("catalog", "search")})
        self.assertEqual(coverage.check(root), [])

    def test_sync_conflict_copies_are_ignored(self):
        self.assertNotIn(("command", "wipeLibrary"), coverage.reachable(self.repo()))

    def test_a_command_without_a_rust_arm_fails(self):
        failures = coverage.check(self.repo(server=SERVER.replace("PauseScan", "ResumeScan")))
        self.assertTrue(any("pauseScan: no CommandPayload::PauseScan arm" in failure for failure in failures))

    def test_an_empty_rust_arm_fails(self):
        app = APP + "    engine.send(.wipeLibrary)\n"
        cases = [{"name": "wipe", "steps": [{"command": {"wipeLibrary": {}}}]}] + CASE
        failures = coverage.check(self.repo(app=app, cases=cases))
        self.assertTrue(any("wipeLibrary: the server.rs arm does nothing" in failure for failure in failures))

    def test_an_action_the_handler_does_not_name_fails(self):
        failures = coverage.check(self.repo(catalog='"jobs" => {}'))
        self.assertTrue(any("catalog search: not named" in failure for failure in failures))

    def test_a_command_no_case_runs_on_rust_fails(self):
        swift_only = [{"name": "controls", "engines": ["swift"], "steps": CASE[0]["steps"]}]
        failures = coverage.check(self.repo(cases=swift_only))
        self.assertTrue(any("pauseScan: the Mac app sends it but no conformance case" in failure for failure in failures))

    def test_a_darwin_run_false_divergence_does_not_count(self):
        blocked = [{**CASE[0], "knownDivergences": [{"engine": "rust", "os": ["darwin"], "run": False, "reason": "x"}]}]
        self.assertTrue(coverage.check(self.repo(cases=blocked)))
        lifted = [{**CASE[0], "knownDivergences": [{"engine": "rust", "os": ["darwin"], "run": False, "liftedByTestTrashRoot": True, "reason": "x"}]}]
        self.assertEqual(coverage.check(self.repo(cases=lifted)), [])

    def test_a_swift_led_case_counts_steps_after_the_restart_into_rust(self):
        cross = [{"name": "cross", "engines": ["swift"], "requires": ["swift", "rust"], "start": "swift", "steps": [
            {"command": {"pauseScan": {}}},
            {"restart": "rust"},
            {"catalog": {"action": "search"}},
        ]}]
        failures = coverage.check(self.repo(cases=cross))
        self.assertEqual(len(failures), 1)
        self.assertIn("pauseScan", failures[0])


if __name__ == "__main__":
    unittest.main()
