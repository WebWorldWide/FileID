// Compiled mirror of the macOS rows in shared/models/manifest.json —
// the canonical cross-platform artifact manifest. Hand-written (no
// build-time codegen) and locked to the JSON by
// SharedTests/ModelManifestTests.swift, same approach as the Windows
// engine's models/registry.rs + manifest_consistency.rs.
import Foundation

public struct ModelArtifact: Sendable, Equatable {
    public let id: String
    public let url: String
    public let sha256: String
    public let approxBytes: Int64

    public init(id: String, url: String, sha256: String, approxBytes: Int64) {
        self.id = id
        self.url = url
        self.sha256 = sha256
        self.approxBytes = approxBytes
    }
}

public struct VLMRepoPin: Sendable, Equatable {
    public let repo: String
    public let kind: String
    public let revision: String
    public let approxBytes: Int64

    public init(repo: String, kind: String, revision: String, approxBytes: Int64) {
        self.repo = repo
        self.kind = kind
        self.revision = revision
        self.approxBytes = approxBytes
    }
}

public enum ModelManifest {

    public static let artifacts: [ModelArtifact] = [
        ModelArtifact(
            id: "sface_embedder",
            url: "https://huggingface.co/opencv/face_recognition_sface/resolve/main/face_recognition_sface_2021dec.onnx",
            sha256: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
            approxBytes: 38_696_353),
        ModelArtifact(
            id: "clip_vitb32_image",
            url: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model.onnx",
            sha256: "fd6e1402a588279d1723c7534d4bcba5bc0b14b47dfab0e46f8c47b8270d7d40",
            approxBytes: 351_685_709),
        ModelArtifact(
            id: "clip_vitb32_text",
            url: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/text_model.onnx",
            sha256: "3f6571f5bad13a97c469c1622e1cfc4d9aef78b79fdbfcff804ca357bfada8cc",
            approxBytes: 254_058_553),
        ModelArtifact(
            id: "clip_bpe_vocab",
            url: "https://huggingface.co/openai/clip-vit-base-patch32/resolve/main/vocab.json",
            sha256: "5047b556ce86ccaf6aa22b3ffccfc52d391ea4accdab9c2f2407da5b742d4363",
            approxBytes: 1_000_000),
        ModelArtifact(
            id: "clip_bpe_merges",
            url: "https://huggingface.co/openai/clip-vit-base-patch32/resolve/main/merges.txt",
            sha256: "f526393189112391ce6f9795d4695f704121ce452c3aad1f5335cc41337eba85",
            approxBytes: 525_000),
    ]

    public static let vlmRepos: [VLMRepoPin] = [
        VLMRepoPin(
            repo: "mlx-community/Qwen2.5-VL-7B-Instruct-4bit",
            kind: "qwen2_5_vl_7b",
            revision: "fdcc572e8b05ba9daeaf71be8c9e4267c826ff9b",
            approxBytes: 4_300_000_000),
        VLMRepoPin(
            repo: "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit",
            kind: "qwen3_vl_4b",
            revision: "552af30c9952c44f1e1a27c7c5810ded58e892bc",
            approxBytes: 3_500_000_000),
        VLMRepoPin(
            repo: "mlx-community/gemma-3-4b-it-qat-4bit",
            kind: "gemma_3_4b",
            revision: "3d9ef289111449933c22761961f16a5df237ce2a",
            approxBytes: 3_300_000_000),
        VLMRepoPin(
            repo: "mlx-community/gemma-3-12b-it-qat-4bit",
            kind: "gemma_3_12b",
            revision: "66fc51ef25778c03d33c4c8bc446973d062e73f4",
            approxBytes: 7_500_000_000),
        VLMRepoPin(
            repo: "mlx-community/Mistral-Small-3.2-24B-Instruct-2506-4bit",
            kind: "mistral_small_3_2",
            revision: "2a1d5eabfc504747bdc24178394821a1efc0edde",
            approxBytes: 13_500_000_000),
        VLMRepoPin(
            repo: "mlx-community/paligemma-3b-mix-448-8bit",
            kind: "paligemma_3b",
            revision: "ce201f8b4b2c2793d4e18d3d44355b49ddff257c",
            approxBytes: 3_300_000_000),
    ]

    public static func sha256(forURL url: URL) -> String? {
        artifacts.first { $0.url == url.absoluteString }?.sha256
    }

    public static func vlmPin(forRepo repo: String) -> VLMRepoPin? {
        vlmRepos.first { $0.repo == repo }
    }
}
