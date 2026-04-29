import Foundation

// MARK: - AIModelRegistry

// Catalogue of optional on-device models. Weights live in
// ~/Library/Application Support/FileID/Models/<slug>/ and are fetched only
// after the user accepts each model's license — we never bundle weights.

enum AIModelKind: String, CaseIterable, Identifiable, Sendable {
    case mobileCLIPImage = "mobileclip_image"
    case mobileCLIPText  = "mobileclip_text"
    // VLMs for Deep Analyze. The `qwen2VL2B` rawValue is historical — it
    // actually points at the 3B Qwen2.5-VL model now (DECISIONS 2026-04-23).
    // New cases use clean rawValues.
    case qwen2VL2B       = "qwen2_vl_2b"
    case qwen3VL4B       = "qwen3_vl_4b"
    case gemma3_4B       = "gemma3_4b"
    case gemma3_12B      = "gemma3_12b"
    case smolvlm         = "smolvlm"
    case paligemma3B     = "paligemma_3b"

    var id: String { rawValue }

    // A "VLM" here means any vision-language model managed via MLX. They live
    // in MLX's hub cache (`~/Documents/huggingface/models/<repo>/`) and are
    // downloaded via VLMModelFactory rather than our own HTTP path. The
    // registry treats them uniformly: relativePaths stays empty, primaryFile
    // points at config.json so isInstalled has something to check.
    var isVLM: Bool {
        switch self {
        case .qwen2VL2B, .qwen3VL4B, .gemma3_4B, .gemma3_12B, .smolvlm, .paligemma3B:
            return true
        case .mobileCLIPImage, .mobileCLIPText:
            return false
        }
    }

    var descriptor: AIModelDescriptor {
        switch self {
        case .mobileCLIPImage:
            return AIModelDescriptor(
                id: .mobileCLIPImage,
                displayName: "MobileCLIP S2 — Image Encoder",
                subtitle: "Semantic image understanding (Apple)",
                reason: "Used to generate 512-d embeddings during scan. Enables zero-shot tagging and \"find similar\" search. ~50 ms per image on the Neural Engine.",
                approxBytes: 88 * 1024 * 1024,
                licenseName: "Apple Sample Code License",
                licenseURL: URL(string: "https://github.com/apple/ml-mobileclip/blob/main/LICENSE_weights_data")!,
                attribution: "Model by Apple. Distributed under the Apple Sample Code License.",
                sourceRepo: "apple/coreml-mobileclip",
                relativePaths: [
                    "mobileclip_s2_image.mlpackage/Manifest.json",
                    "mobileclip_s2_image.mlpackage/Data/com.apple.CoreML/model.mlmodel",
                    "mobileclip_s2_image.mlpackage/Data/com.apple.CoreML/weights/weight.bin",
                ],
                primaryFile: "mobileclip_s2_image.mlpackage"
            )
        case .mobileCLIPText:
            return AIModelDescriptor(
                id: .mobileCLIPText,
                displayName: "MobileCLIP S2 — Text Encoder",
                subtitle: "Zero-shot text prompts (Apple)",
                reason: "Matches image embeddings against natural-language labels (\"sunset\", \"wedding\"). Required for semantic tags.",
                approxBytes: 73 * 1024 * 1024,
                licenseName: "Apple Sample Code License",
                licenseURL: URL(string: "https://github.com/apple/ml-mobileclip/blob/main/LICENSE_weights_data")!,
                attribution: "Model by Apple. Distributed under the Apple Sample Code License.",
                sourceRepo: "apple/coreml-mobileclip",
                relativePaths: [
                    "mobileclip_s2_text.mlpackage/Manifest.json",
                    "mobileclip_s2_text.mlpackage/Data/com.apple.CoreML/model.mlmodel",
                    "mobileclip_s2_text.mlpackage/Data/com.apple.CoreML/weights/weight.bin",
                ],
                primaryFile: "mobileclip_s2_text.mlpackage"
            )
        case .qwen2VL2B:
            return AIModelDescriptor(
                id: .qwen2VL2B,
                displayName: "Qwen2.5-VL 3B (4-bit)",
                subtitle: "Default. Strong general captions + document handling.",
                reason: "Default Deep Analyze model. Apache 2.0, ~3 GB on disk, ~1–2 s per image on M1. The all-rounder.",
                approxBytes: 3_146_000_000,
                licenseName: "Apache License 2.0",
                licenseURL: URL(string: "https://www.apache.org/licenses/LICENSE-2.0")!,
                attribution: "Qwen2.5-VL by Alibaba Cloud. MLX quantisation by the MLX community. Apache 2.0.",
                sourceRepo: "mlx-community/Qwen2.5-VL-3B-Instruct-4bit",
                relativePaths: [
                    "config.json",
                    "tokenizer.json",
                    "tokenizer_config.json",
                    "special_tokens_map.json",
                    "added_tokens.json",
                    "merges.txt",
                    "vocab.json",
                    "preprocessor_config.json",
                    "chat_template.json",
                    "model.safetensors",
                    "model.safetensors.index.json",
                ],
                primaryFile: "model.safetensors"
            )
        case .qwen3VL4B:
            return AIModelDescriptor(
                id: .qwen3VL4B,
                displayName: "Qwen3-VL 4B (4-bit)",
                subtitle: "Newer architecture. Better OCR + reasoning than Qwen2.5.",
                reason: "Apache 2.0. Strongest accuracy among the 4-bit options at similar size to Qwen2.5-VL 3B. Slightly slower. Downloaded by MLX on first use.",
                approxBytes: 3_500_000_000,
                licenseName: "Apache License 2.0",
                licenseURL: URL(string: "https://www.apache.org/licenses/LICENSE-2.0")!,
                attribution: "Qwen3-VL by Alibaba Cloud. MLX quantisation via lmstudio-community. Apache 2.0.",
                sourceRepo: "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit",
                relativePaths: [],
                primaryFile: "config.json"
            )
        case .gemma3_4B:
            return AIModelDescriptor(
                id: .gemma3_4B,
                displayName: "Gemma 3 4B (QAT 4-bit)",
                subtitle: "Closest live equivalent to 'Gemma 4'. Strong on grounded VQA.",
                reason: "Google's current open vision-language model. Gemma 4's Swift bridge isn't shipped yet — Gemma 3 4B QAT is the best stand-in. Gemma license. Downloaded by MLX on first use.",
                approxBytes: 3_300_000_000,
                licenseName: "Gemma Terms of Use",
                licenseURL: URL(string: "https://ai.google.dev/gemma/terms")!,
                attribution: "Gemma 3 by Google DeepMind. MLX QAT quantisation by the MLX community.",
                sourceRepo: "mlx-community/gemma-3-4b-it-qat-4bit",
                relativePaths: [],
                primaryFile: "config.json"
            )
        case .gemma3_12B:
            return AIModelDescriptor(
                id: .gemma3_12B,
                displayName: "Gemma 3 12B (QAT 4-bit)",
                subtitle: "Highest accuracy. Heavy: ~7 GB on disk, ~3× slower.",
                reason: "Google's larger vision-language model. Best caption quality of the lineup. Tight on 16 GB Macs (~9 GB resident with weights + cache). Use for batch passes when accuracy matters more than throughput.",
                approxBytes: 7_500_000_000,
                licenseName: "Gemma Terms of Use",
                licenseURL: URL(string: "https://ai.google.dev/gemma/terms")!,
                attribution: "Gemma 3 by Google DeepMind. MLX QAT quantisation by the MLX community.",
                sourceRepo: "mlx-community/gemma-3-12b-it-qat-4bit",
                relativePaths: [],
                primaryFile: "config.json"
            )
        case .smolvlm:
            return AIModelDescriptor(
                id: .smolvlm,
                displayName: "SmolVLM Instruct (4-bit)",
                subtitle: "Smallest + fastest. Use when battery / RAM matter.",
                reason: "Hugging Face's compact VLM (~600 MB on disk). 2× faster than Qwen2.5-VL 3B but caption quality is noticeably weaker. Good for triage passes on huge libraries.",
                approxBytes: 600_000_000,
                licenseName: "Apache License 2.0",
                licenseURL: URL(string: "https://www.apache.org/licenses/LICENSE-2.0")!,
                attribution: "SmolVLM by Hugging Face. MLX quantisation by the MLX community. Apache 2.0.",
                sourceRepo: "mlx-community/SmolVLM-Instruct-4bit",
                relativePaths: [],
                primaryFile: "config.json"
            )
        case .paligemma3B:
            return AIModelDescriptor(
                id: .paligemma3B,
                displayName: "PaliGemma 3B (8-bit)",
                subtitle: "Strong on grounding, OCR, and visual question answering.",
                reason: "Google's PaliGemma 3B at 8-bit precision. Stronger OCR than the smaller 4-bit options at similar disk cost. Older but well-tested. Gemma license.",
                approxBytes: 3_300_000_000,
                licenseName: "Gemma Terms of Use",
                licenseURL: URL(string: "https://ai.google.dev/gemma/terms")!,
                attribution: "PaliGemma by Google. MLX 8-bit quantisation by the MLX community.",
                sourceRepo: "mlx-community/paligemma-3b-mix-448-8bit",
                relativePaths: [],
                primaryFile: "config.json"
            )
        }
    }
}

struct AIModelDescriptor: Sendable {
    let id: AIModelKind
    let displayName: String
    let subtitle: String
    let reason: String
    let approxBytes: Int64
    let licenseName: String
    let licenseURL: URL
    let attribution: String
    let sourceRepo: String
    let relativePaths: [String]
    let primaryFile: String

    var localDir: URL {
        AIModelRegistry.baseDirectory.appendingPathComponent(id.rawValue, isDirectory: true)
    }

    var primaryFileURL: URL {
        localDir.appendingPathComponent(primaryFile)
    }

    func remoteURL(for relativePath: String) -> URL {
        URL(string: "https://huggingface.co/\(sourceRepo)/resolve/main/\(relativePath)")!
    }

    var isInstalled: Bool {
        // VLMs live in MLX's hub cache under ~/Documents/huggingface/models/<repo>/.
        // Treat the presence of the primaryFile (config.json for new VLMs,
        // model.safetensors for legacy Qwen) as "installed".
        if id.isVLM {
            return Self.vlmCacheURL(forRepo: sourceRepo)
                .map { FileManager.default.fileExists(atPath: $0.appendingPathComponent(primaryFile).path) }
                ?? false
        }
        return FileManager.default.fileExists(atPath: primaryFileURL.path)
    }

    var approxSizeString: String {
        ByteCountFormatter.string(fromByteCount: approxBytes, countStyle: .file)
    }

    // MLX hub cache root for any HuggingFace repo. MLX downloads into this
    // path on first `loadContainer` call, so we both check installation and
    // delete cached weights via this path.
    static func vlmCacheURL(forRepo repo: String) -> URL? {
        guard let docs = FileManager.default.urls(
            for: .documentDirectory, in: .userDomainMask
        ).first else { return nil }
        return docs.appendingPathComponent("huggingface/models/\(repo)", isDirectory: true)
    }
}

enum AIModelRegistry {
    static let baseDirectory: URL = {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        let dir = support.appendingPathComponent("FileID/Models", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }()

    static func removeAll() {
        guard let entries = try? FileManager.default.contentsOfDirectory(
            at: baseDirectory, includingPropertiesForKeys: nil
        ) else { return }
        for url in entries { try? FileManager.default.removeItem(at: url) }
    }

    static func remove(_ kind: AIModelKind) {
        if kind.isVLM {
            // VLMs are MLX-managed: nuke the hub cache subdirectory.
            if let cache = AIModelDescriptor.vlmCacheURL(forRepo: kind.descriptor.sourceRepo) {
                try? FileManager.default.removeItem(at: cache)
            }
            // If this was the active model, drop the user setting so
            // DeepAnalyzeService falls back to the default Qwen.
            if UserDefaults.standard.string(forKey: "deepAnalyzeActiveModel") == kind.rawValue {
                UserDefaults.standard.removeObject(forKey: "deepAnalyzeActiveModel")
            }
        } else {
            try? FileManager.default.removeItem(at: kind.descriptor.localDir)
        }
    }
}
