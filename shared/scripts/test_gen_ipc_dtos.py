import copy
import json
import tempfile
import unittest
from pathlib import Path

import gen_ipc_dtos as gen


class GenIpcDtosTests(unittest.TestCase):
    def setUp(self) -> None:
        self.schema = json.loads(gen.SCHEMA_PATH.read_text(encoding="utf-8"))
        self.config = json.loads(gen.CONFIG_PATH.read_text(encoding="utf-8"))

    def command_body(self, schema: dict, tag: str) -> dict:
        for branch in schema["$defs"]["CommandPayload"]["oneOf"]:
            if tag in branch["properties"]:
                return branch["properties"][tag]
        raise KeyError(tag)

    def test_committed_files_are_current(self) -> None:
        self.assertEqual(gen.main(["--check"]), 0)

    def test_new_schema_field_changes_rust_and_breaks_mirrors(self) -> None:
        schema = copy.deepcopy(self.schema)
        self.command_body(schema, "prewarmModel")["properties"]["priority"] = {"type": "integer"}
        model = gen.build_model(schema)
        rust = gen.RustGen(model, self.config["rust"]).render()
        self.assertIn("pub priority: Option<i64>,", rust)
        for lang in ("csharp", "swift"):
            problems = gen.mirror_problems(model, lang, self.config[lang])
            self.assertIn(f"CommandPayload.prewarmModel: schema fields missing from {lang}: priority", problems)

    def test_new_variant_is_required_everywhere(self) -> None:
        schema = copy.deepcopy(self.schema)
        schema["$defs"]["CommandPayload"]["oneOf"].append({
            "type": "object", "required": ["ping"], "additionalProperties": False,
            "properties": {"ping": {"type": "object", "additionalProperties": False}},
        })
        model = gen.build_model(schema)
        self.assertIn('#[serde(rename = "ping")]\n    Ping(Empty),', gen.RustGen(model, self.config["rust"]).render())
        problems = gen.mirror_problems(model, "csharp", self.config["csharp"])
        self.assertIn("CommandPayload.ping: schema variant missing from csharp", problems)

    def test_mirror_field_absent_from_schema_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "Dtos.cs"
            source.write_text(
                'public sealed record LibraryWiped(bool Ok, string? Message = null, '
                '[property: JsonPropertyName("wipedAt")] double? WipedAt = null);\n',
                encoding="utf-8",
            )
            records, _, _ = gen.parse_csharp([source])
        self.assertEqual(records["LibraryWiped"], ["ok", "message", "wipedAt"])

    def test_swift_coding_keys_override_property_names(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "IPC.swift"
            source.write_text(
                "public struct Thing: Codable {\n"
                "    public let fileID: Int64\n"
                "    public var label: String { \"x\" }\n"
                "    private enum CodingKeys: String, CodingKey {\n"
                "        case fileID = \"file_id\"\n"
                "    }\n"
                "}\n",
                encoding="utf-8",
            )
            structs, _, _ = gen.parse_swift([source])
        self.assertEqual(structs["Thing"], ["file_id"])

    def test_config_cannot_name_fields_the_schema_lacks(self) -> None:
        config = copy.deepcopy(self.config["rust"])
        config["types"]["LibraryWiped"] = {"fields": {"wipedAt": {"type": "f64"}}}
        with self.assertRaises(gen.GenError):
            gen.RustGen(gen.build_model(self.schema), config).render()

    def test_stale_known_drift_fails(self) -> None:
        spec = copy.deepcopy(self.config["swift"])
        spec["known_drift"] = {"LibraryWiped (LibraryWiped): schema fields missing from swift: ok": "fixed"}
        problems = gen.mirror_problems(gen.build_model(self.schema), "swift", spec)
        self.assertTrue(any(p.startswith("stale codegen.json swift.known_drift") for p in problems))

    def test_name_conversions(self) -> None:
        self.assertEqual(gen.snake("faceIDsToRevert"), "face_ids_to_revert")
        self.assertEqual(gen.snake("physicalMemoryGB"), "physical_memory_gb")
        self.assertEqual(gen.snake("visionP50Ms"), "vision_p50_ms")
        self.assertEqual(gen.dotnet_camel("FileIDs"), "fileIDs")
        self.assertEqual(gen.dotnet_camel("PCores"), "pCores")
        self.assertEqual(gen.dotnet_camel("ID"), "id")


if __name__ == "__main__":
    unittest.main()
