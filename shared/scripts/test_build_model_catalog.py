#!/usr/bin/env python3
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_model_catalog as catalog  # noqa: E402


def hop(repo: str, model_type: str | None = None, **card: object) -> tuple[str, dict]:
    info: dict = {"id": repo, "cardData": card}
    if model_type:
        info["config"] = {"model_type": model_type}
    return repo, info


class ModelNameTests(unittest.TestCase):
    def test_strips_publisher_prefix_and_conversion_suffixes(self) -> None:
        self.assertEqual(catalog.model_name("bartowski/mistralai_Voxtral-Mini-3B-2507-GGUF"), "Voxtral-Mini-3B-2507")
        self.assertEqual(catalog.model_name("ggml-org/Qwen3-VL-30B-A3B-Instruct-Q8_0-GGUF"), "Qwen3-VL-30B-A3B-Instruct")
        self.assertEqual(catalog.model_name("mlx-community/gemma-3n-E2B-it-lm-4bit"), "gemma-3n-E2B-it")
        self.assertEqual(catalog.model_name("Qwen/Qwen3-4B-MLX-4bit"), "Qwen3-4B")


class IdentityTests(unittest.TestCase):
    def check(self, chain: list[tuple[str, dict]]) -> str | None:
        upstream, info = catalog.upstream_of(chain)
        return catalog.identity_problem(chain, upstream, info)

    def test_card_copied_from_another_model_is_rejected(self) -> None:
        chain = [
            hop("mlx-community/gemma-3-4b-it-qat-4bit", "gemma3"),
            hop("OpenGVLab/InternVL3-1B-Instruct", "internvl_chat"),
        ]
        self.assertIn("does not match repo", self.check(chain))

    def test_fine_tune_listed_as_its_base_is_rejected(self) -> None:
        chain = [
            hop("mlx-community/olmOCR-2-7B-1025-mlx-8bit", "qwen2_5_vl"),
            hop("Qwen/Qwen2.5-VL-7B-Instruct", "qwen2_5_vl"),
        ]
        self.assertIn("does not match repo", self.check(chain))

    def test_instruct_conversion_of_declared_base_is_rejected(self) -> None:
        chain = [hop("mlx-community/Qwen2.5-7B-Instruct-4bit", "qwen2"), hop("Qwen/Qwen2.5-7B", "qwen2")]
        self.assertIn("declared base", self.check(chain))

    def test_architecture_mismatch_is_rejected(self) -> None:
        chain = [
            hop("mlx-community/PaddleOCR-VL-1.5-4bit", "paddleocr_vl"),
            hop("PaddlePaddle/PaddleOCR-VL-1.5", "ernie4_5"),
        ]
        self.assertIn("architecture", self.check(chain))

    def test_unresolvable_upstream_is_rejected(self) -> None:
        chain = [hop("lmstudio-community/gemma-4-26B-A4B-it-QAT-MLX-4bit", "gemma4"), ("google/gemma-4-26B-A4B-it-QAT", {})]
        self.assertIn("cannot resolve", self.check(chain))

    def test_community_conversion_resolves_to_the_original_release(self) -> None:
        chain = [
            hop("mlx-community/Mistral-Small-3.2-24B-Instruct-2506-4bit", "mistral3"),
            hop("mlx-community/Mistral-Small-3.2-24B-Instruct-2506-bf16", "mistral3"),
            hop("mistralai/Mistral-Small-3.2-24B-Instruct-2506", "mistral3"),
        ]
        self.assertEqual(catalog.upstream_of(chain)[0], "mistralai/Mistral-Small-3.2-24B-Instruct-2506")
        self.assertIsNone(self.check(chain))

    def test_matching_conversion_passes(self) -> None:
        chain = [
            hop("lmstudio-community/Phi-3.1-mini-4k-instruct-GGUF"),
            hop("microsoft/Phi-3-mini-4k-instruct", "phi3"),
        ]
        self.assertIsNone(self.check(chain))


class LicenseReferenceTests(unittest.TestCase):
    def test_restricted_license_name_overrides_apache_field(self) -> None:
        _, info = hop("Qwen/Qwen2.5-VL-3B-Instruct", license="apache-2.0", license_name="qwen-research")
        self.assertTrue(catalog.restricted_license_reference(info))

    def test_permissive_license_link_to_own_license_passes(self) -> None:
        _, info = hop("Qwen/Qwen3-4B", license="apache-2.0", license_link="https://huggingface.co/Qwen/Qwen3-4B/blob/main/LICENSE")
        self.assertFalse(catalog.restricted_license_reference(info))

    def test_reviewed_internvl3_card_covers_its_ancestors(self) -> None:
        chain = [
            hop("ggml-org/InternVL3-1B-Instruct-GGUF"),
            hop("OpenGVLab/InternVL3-1B-Instruct", license="apache-2.0", license_name="qwen"),
            hop("OpenGVLab/InternVL3-1B-Pretrained", license="apache-2.0", license_name="qwen"),
        ]
        self.assertFalse(any(catalog.restricted_license_reference(info) for _, info in catalog.license_chain(chain)))

    def test_denylist_covers_llama_distills_and_restricted_lines(self) -> None:
        for name in ["deepseek-ai/DeepSeek-R1-Distill-Llama-8B", "Qwen/Qwen2.5-3B-Instruct", "Qwen/Qwen2-VL-72B-Instruct",
                     "apple/FastVLM-1.5B", "LiquidAI/LFM2-VL-450M", "google/embeddinggemma-2",
                     "huihui-ai/Huihui-Qwen3.5-4B-Claude-4.6-Opus-abliterated", "OBLITERATUS/Qwen3.8-27B-OBLITERATED",
                     "cognitivecomputations/Dolphin-Mistral-24B-Venice-Edition", "yuxinlu1/gemma-4-12B-coder-fable5-composer2.5-v1"]:
            with self.subTest(name=name):
                self.assertIsNotNone(catalog.restricted_reason(name))
        for name in ["Qwen/Qwen2.5-32B-Instruct", "Qwen/Qwen2.5-VL-7B-Instruct", "HuggingFaceTB/SmolLM2-1.7B-Instruct",
                     "google/embeddinggemma-300m", "mistralai/Devstral-Small-2-24B-Instruct-2512",
                     "openai/gpt-oss-20b", "deepseek-ai/DeepSeek-R1-Distill-Qwen-14B", "NousResearch/Hermes-2-Pro-Mistral-7B"]:
            with self.subTest(name=name):
                self.assertIsNone(catalog.restricted_reason(name))


class LanguageModelTests(unittest.TestCase):
    def test_image_generators_and_tts_are_not_language_models(self) -> None:
        cases = [
            ({"id": "unsloth/Qwen-Image-GGUF", "gguf": {"architecture": "qwen_image"}}, "Qwen/Qwen-Image", {}),
            ({"id": "unsloth/ERNIE-Image-Turbo-GGUF", "gguf": {"architecture": "wan"}}, "baidu/ERNIE-Image-Turbo", {}),
            ({"id": "mlx-community/Qwen-Image-2512-8bit", "library_name": "mflux"}, "Qwen/Qwen-Image-2512", {}),
            ({"id": "mlx-community/VoxCPM2-8bit", "config": {"model_type": "voxcpm2"}}, "openbmb/VoxCPM2", {}),
            ({"id": "mlx-community/Qwen3-TTS-12Hz-1.7B-CustomVoice-8bit"}, "Qwen/Qwen3-TTS-12Hz-1.7B-CustomVoice",
             {"pipeline_tag": "text-to-speech"}),
        ]
        for info, base, base_info in cases:
            with self.subTest(repo=info["id"]):
                self.assertTrue(catalog.not_language_model(info, base, base_info))

    def test_language_and_speech_recognition_models_pass(self) -> None:
        for info, base in [
            ({"id": "ggml-org/Qwen3-VL-2B-Instruct-GGUF", "gguf": {"architecture": "qwen3vl"}}, "Qwen/Qwen3-VL-2B-Instruct"),
            ({"id": "ggml-org/Qwen3-ASR-0.6B-GGUF", "pipeline_tag": "automatic-speech-recognition"}, "Qwen/Qwen3-ASR-0.6B"),
        ]:
            with self.subTest(repo=info["id"]):
                self.assertFalse(catalog.not_language_model(info, base, {}))

    def test_frognano_is_never_offered_as_vision(self) -> None:
        self.assertEqual(catalog.ROLE_OVERRIDES["microsoft/FrogNano-4B-2609"], "text")


if __name__ == "__main__":
    unittest.main()
