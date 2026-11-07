#!/usr/bin/env python3
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


class ModelLicensePolicyTests(unittest.TestCase):
    def run_checker(self, reviewed_at: str, edit=None) -> subprocess.CompletedProcess[str]:
        repository = Path(__file__).resolve().parents[2]
        manifest = json.loads(
            (repository / "shared/models/manifest.json").read_text(encoding="utf-8")
        )
        for policy in manifest["licensePolicies"].values():
            policy["reviewedAt"] = reviewed_at
        if edit:
            edit(manifest)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "manifest.json"
            path.write_text(manifest if isinstance(manifest, str) else json.dumps(manifest), encoding="utf-8")
            return subprocess.run(
                [
                    sys.executable,
                    str(repository / "shared/scripts/check_model_license_policy.py"),
                    str(path),
                ],
                capture_output=True,
                text=True,
                check=False,
                cwd=repository,
            )

    def test_valid_calendar_date_passes(self) -> None:
        result = self.run_checker("2026-07-16")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_impossible_month_and_day_fail(self) -> None:
        for invalid in ["2026-99-16", "2026-02-30"]:
            with self.subTest(invalid=invalid):
                result = self.run_checker(invalid)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("valid YYYY-MM-DD calendar date", result.stdout)

    def test_non_date_text_fails(self) -> None:
        result = self.run_checker("not-a-date")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("valid YYYY-MM-DD calendar date", result.stdout)

    def test_every_entry_names_a_reviewed_license(self) -> None:
        def unknown_artifact(manifest: dict) -> None:
            manifest["artifacts"][0]["license"] = "CC-BY-NC-4.0"

        def missing_repo_license(manifest: dict) -> None:
            del manifest["vlmRepos"][0]["license"]

        def no_artifacts(manifest: dict) -> None:
            manifest["artifacts"] = []

        for edit, expected in [
            (unknown_artifact, "unknown license policy 'CC-BY-NC-4.0'"),
            (missing_repo_license, "unknown license policy None"),
            (no_artifacts, "artifacts must be a non-empty list"),
        ]:
            with self.subTest(expected=expected):
                result = self.run_checker("2026-07-16", edit)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected, result.stdout)


def catalog_entry(**fields: object) -> dict:
    entry = {
        "id": "qwen3_vl_2b_instruct_gguf_q8_0",
        "name": "Qwen3-VL-2B-Instruct",
        "family": "Qwen3-VL",
        "upstream": "Qwen/Qwen3-VL-2B-Instruct",
        "repo": "ggml-org/Qwen3-VL-2B-Instruct-GGUF",
        "license": "Apache-2.0",
        "termsRequired": False,
    }
    entry.update(fields)
    return entry


class ExtendedCatalogPolicyTests(unittest.TestCase):
    def run_catalog(self, *entries: dict) -> subprocess.CompletedProcess[str]:
        repository = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "catalog.json"
            path.write_text(json.dumps({"schemaVersion": 1, "entries": list(entries)}), encoding="utf-8")
            return subprocess.run(
                [
                    sys.executable,
                    str(repository / "shared/scripts/check_model_license_policy.py"),
                    "--catalog",
                    str(path),
                ],
                capture_output=True,
                text=True,
                check=False,
                cwd=repository,
            )

    def assert_fails(self, message: str, *entries: dict) -> None:
        result = self.run_catalog(*entries)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stdout)

    def test_permitted_entries_pass(self) -> None:
        result = self.run_catalog(
            catalog_entry(),
            catalog_entry(
                id="gemma_3_4b_it_gguf_q4_k_m", name="gemma-3-4b-it", family="gemma",
                upstream="google/gemma-3-4b-it", repo="ggml-org/gemma-3-4b-it-GGUF",
                license="Gemma", termsRequired=True,
            ),
            catalog_entry(
                id="gemma_4_e2b_it_gguf_q4_0", name="gemma-4-E2B-it", family="gemma",
                upstream="google/gemma-4-E2B-it", repo="ggml-org/gemma-4-E2B-it-GGUF",
            ),
            catalog_entry(
                id="deepseek_r1_distill_qwen_7b_gguf_q4_k_m", name="DeepSeek-R1-Distill-Qwen-7B",
                family="DeepSeek-R1-Distill-Qwen", upstream="deepseek-ai/DeepSeek-R1-Distill-Qwen-7B",
                repo="lmstudio-community/DeepSeek-R1-Distill-Qwen-7B-GGUF", license="MIT",
            ),
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_name_family_must_match_repo(self) -> None:
        self.assert_fails(
            "does not match repo",
            catalog_entry(
                id="internvl3_1b_instruct_mlx_4bit", name="InternVL3-1B-Instruct", family="InternVL3",
                upstream="OpenGVLab/InternVL3-1B-Instruct", repo="mlx-community/gemma-3-4b-it-qat-4bit",
            ),
        )
        self.assert_fails(
            "does not match repo",
            catalog_entry(
                id="qwen2_5_vl_7b_instruct_mlx_8bit", name="Qwen2.5-VL-7B-Instruct", family="Qwen2.5-VL",
                upstream="Qwen/Qwen2.5-VL-7B-Instruct", repo="mlx-community/olmOCR-2-7B-1025-mlx-8bit",
            ),
        )

    def test_name_size_must_match_repo(self) -> None:
        self.assert_fails("is not in repo", catalog_entry(repo="ggml-org/Qwen3-VL-8B-Instruct-GGUF"))

    def test_denylisted_models_fail(self) -> None:
        for name, family, upstream, repo in [
            ("Qwen2.5-VL-3B-Instruct", "Qwen2.5-VL", "Qwen/Qwen2.5-VL-3B-Instruct",
             "ggml-org/Qwen2.5-VL-3B-Instruct-GGUF"),
            ("Qwen2.5-3B-Instruct", "Qwen2.5", "Qwen/Qwen2.5-3B-Instruct", "Qwen/Qwen2.5-3B-Instruct-GGUF"),
            ("Qwen2.5-72B-Instruct", "Qwen2.5", "Qwen/Qwen2.5-72B-Instruct", "Qwen/Qwen2.5-72B-Instruct-GGUF"),
            ("DeepSeek-R1-Distill-Llama-8B", "DeepSeek-R1-Distill-Llama", "deepseek-ai/DeepSeek-R1-Distill-Llama-8B",
             "lmstudio-community/DeepSeek-R1-Distill-Llama-8B-GGUF"),
            ("FastVLM-0.5B", "FastVLM", "apple/FastVLM-0.5B", "mlx-community/FastVLM-0.5B-bf16"),
            ("LFM2-VL-1.6B", "LFM2-VL", "LiquidAI/LFM2-VL-1.6B", "LiquidAI/LFM2-VL-1.6B-GGUF"),
        ]:
            with self.subTest(name=name):
                self.assert_fails(
                    "denylisted model",
                    catalog_entry(id="x", name=name, family=family, upstream=upstream, repo=repo, license="MIT"),
                )

    def test_gemma_repo_requires_terms(self) -> None:
        self.assert_fails(
            "termsRequired",
            catalog_entry(
                id="gemma_3_12b_it_mlx_4bit", name="gemma-3-12b-it", family="gemma",
                upstream="google/gemma-3-12b-it", repo="mlx-community/gemma-3-12b-it-4bit",
            ),
        )
        self.assert_fails(
            "termsRequired",
            catalog_entry(
                id="translategemma_4b_it_mlx_4bit", name="translategemma-4b-it", family="translategemma",
                upstream="google/translategemma-4b-it", repo="mlx-community/translategemma-4b-it-4bit",
                license="Gemma", termsRequired=False,
            ),
        )

    def test_safety_stripped_and_vendor_named_models_fail(self) -> None:
        for name, family, upstream, repo in [
            ("Qwen3.6-27B-Uncensored-Heretic", "Qwen3.6", "Qwen/Qwen3.6-27B", "mlx-community/Qwen3.6-27B-Uncensored-Heretic-4bit"),
            ("Qwen2.5-7B-Instruct-abliterated", "Qwen2.5", "Qwen/Qwen2.5-7B-Instruct",
             "bartowski/Qwen2.5-7B-Instruct-abliterated-GGUF"),
            ("Qwen3.5-27B-Claude-4.6-Opus-Reasoning-Distilled", "Qwen3.5", "Qwen/Qwen3.5-27B",
             "mlx-community/Qwen3.5-27B-Claude-4.6-Opus-Reasoning-Distilled-4bit"),
            ("DeepSeek-V4-Pro-Qwen3.5-4B", "DeepSeek-V4-Pro-Qwen3.5", "Jackrong/DeepSeek-V4-Pro-Qwen3.5-4B",
             "mlx-community/DeepSeek-V4-Pro-Qwen3.5-4B-4bit"),
        ]:
            with self.subTest(name=name):
                self.assert_fails(
                    "denylisted model",
                    catalog_entry(id="x", name=name, family=family, upstream=upstream, repo=repo),
                )

    def test_image_generation_and_tts_are_not_offered(self) -> None:
        self.assert_fails(
            "image-generation or text-to-speech",
            catalog_entry(
                id="qwen_image_gguf_q4_k_m", name="Qwen-Image", family="Qwen-Image", upstream="Qwen/Qwen-Image",
                repo="unsloth/Qwen-Image-GGUF", role="text",
            ),
        )
        self.assert_fails(
            "image-generation or text-to-speech",
            catalog_entry(
                id="voxcpm2_mlx_8bit", name="VoxCPM2", family="VoxCPM2", upstream="openbmb/VoxCPM2",
                repo="mlx-community/VoxCPM2-8bit", role="text",
            ),
        )

    def test_mac_catalog_denylist(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as temporary:
            catalog = Path(temporary) / "catalog.json"
            catalog.write_text(json.dumps({"entries": [catalog_entry()]}), encoding="utf-8")
            mac = Path(temporary) / "model-catalog.json"
            mac.write_text(json.dumps({"entries": [{
                "repo": "mlx-community/Josiefied-Qwen2.5-7B-Instruct-abliterated-v2-4-bit",
                "bases": [{"repo": "Qwen/Qwen2.5-7B"}], "files": [],
            }]}), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(repository / "shared/scripts/check_model_license_policy.py"),
                 "--catalog", str(catalog), "--mac-catalog", str(mac)],
                capture_output=True, text=True, check=False, cwd=repository,
            )
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Mac catalog mlx-community/Josiefied", result.stdout)

    def test_unknown_license_fails(self) -> None:
        self.assert_fails("is not allowed", catalog_entry(license="Qwen-Research"))


if __name__ == "__main__":
    unittest.main()
