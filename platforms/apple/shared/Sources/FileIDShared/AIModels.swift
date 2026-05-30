// AIModelKind — VLM choices for Deep Analyze + AI face clustering.
// The app uses this to render the model picker; the engine passes the
// `rawValue` to MLX. Both sides must agree on rawValues used in IPC.
import Foundation

public enum AIModelKind: String, CaseIterable, Sendable, Codable {
    // Commercial-clean lineup (2026-05): the non-commercial Qwen2.5-VL-3B
    // (Qwen Research license) was dropped for the Apache-2.0 7B. Gemma /
    // PaliGemma stay (Gemma Terms permit commercial use); Mistral-Small-3.2
    // (Apache-2.0) is the max-quality pick. Mirrors the Windows VLM ladder.
    case qwen2VL7B       = "qwen2_vl_7b"
    case qwen3VL4B       = "qwen3_vl_4b"
    case gemma3_4B       = "gemma3_4b"
    case gemma3_12B      = "gemma3_12b"
    case mistralSmall32  = "mistral_small_3_2"
    case paligemma3B     = "paligemma_3b"

    public var displayName: String {
        switch self {
        case .qwen2VL7B:     return "Qwen2.5-VL 7B (4-bit)"
        case .qwen3VL4B:     return "Qwen3-VL 4B (4-bit)"
        case .gemma3_4B:     return "Gemma 3 4B (QAT 4-bit)"
        case .gemma3_12B:    return "Gemma 3 12B (QAT 4-bit)"
        case .mistralSmall32: return "Mistral Small 3.2 24B (4-bit)"
        case .paligemma3B:   return "PaliGemma 3B (8-bit)"
        }
    }

    public var subtitle: String {
        switch self {
        case .qwen2VL7B:     return "Recommended. Apache-2.0; strong OCR + scene understanding."
        case .qwen3VL4B:     return "Newer architecture. Better OCR + reasoning; lighter than 7B."
        case .gemma3_4B:     return "Google's open model. Strong on grounded VQA. Lightest pick."
        case .gemma3_12B:    return "High quality. Heavy: ~9 GB resident, ~3× slower."
        case .mistralSmall32: return "Max quality. Apache-2.0; ~14 GB, slowest. 32 GB Macs."
        case .paligemma3B:   return "Strong on grounding, OCR, and visual question answering."
        }
    }

    public var sourceRepo: String {
        switch self {
        case .qwen2VL7B:     return "mlx-community/Qwen2.5-VL-7B-Instruct-4bit"
        case .qwen3VL4B:     return "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit"
        case .gemma3_4B:     return "mlx-community/gemma-3-4b-it-qat-4bit"
        case .gemma3_12B:    return "mlx-community/gemma-3-12b-it-qat-4bit"
        case .mistralSmall32: return "mlx-community/Mistral-Small-3.2-24B-Instruct-2506-4bit"
        case .paligemma3B:   return "mlx-community/paligemma-3b-mix-448-8bit"
        }
    }

    public var approxBytes: Int64 {
        switch self {
        case .qwen2VL7B:     return 4_300_000_000
        case .qwen3VL4B:     return 3_500_000_000
        case .gemma3_4B:     return 3_300_000_000
        case .gemma3_12B:    return 7_500_000_000
        case .mistralSmall32: return 13_500_000_000
        case .paligemma3B:   return 3_300_000_000
        }
    }

    public var ramBudgetGB: Double {
        switch self {
        case .qwen2VL7B:     return 7.0
        case .qwen3VL4B:     return 5.0
        case .gemma3_4B:     return 4.5
        case .gemma3_12B:    return 9.0
        case .mistralSmall32: return 16.0
        case .paligemma3B:   return 4.0
        }
    }

    /// Per-image inference cost (seconds). Used to estimate batch ETA.
    public var secondsPerImage: Double {
        switch self {
        case .qwen2VL7B:     return 2.5
        case .qwen3VL4B:     return 2.0
        case .gemma3_4B:     return 1.7
        case .gemma3_12B:    return 5.0
        case .mistralSmall32: return 6.0
        case .paligemma3B:   return 1.6
        }
    }

    public var licenseName: String {
        switch self {
        case .qwen2VL7B, .qwen3VL4B, .mistralSmall32: return "Apache License 2.0"
        case .gemma3_4B, .gemma3_12B, .paligemma3B:   return "Gemma Terms of Use"
        }
    }

    /// Top three picks for a given RAM tier, ranked best-first.
    public static func recommendedFor(ramGB: Double) -> [AIModelKind] {
        if ramGB >= 32       { return [.mistralSmall32, .gemma3_12B, .qwen2VL7B] }
        else if ramGB >= 16  { return [.qwen2VL7B, .qwen3VL4B, .gemma3_4B] }
        else                 { return [.gemma3_4B, .qwen3VL4B, .paligemma3B] }
    }

    /// Reserves ~8 GB for system + scan engine + DB cache. A model that
    /// would push past `ramGB - 8` is rejected to avoid OOM-killing the
    /// engine when MLX loads weights.
    public func fits(ramGB: Double) -> Bool {
        let headroom = max(0, ramGB - 8.0)
        return ramBudgetGB <= headroom
    }

    /// First recommendation that fits, falling back to the lightest
    /// commercial-clean pick (Gemma 3 4B).
    public static func safeDefaultFor(ramGB: Double) -> AIModelKind {
        recommendedFor(ramGB: ramGB).first { $0.fits(ramGB: ramGB) } ?? .gemma3_4B
    }

    /// Migrate a persisted rawValue that may predate the commercial-clean
    /// lineup (e.g. the dropped "qwen2_vl_3b"). Call this when decoding a
    /// stored selection so an old value maps to a supported model instead
    /// of failing to decode. Mirrors the Windows AppSettings v5 migration.
    public static func migrated(rawValue: String) -> AIModelKind {
        if let kind = AIModelKind(rawValue: rawValue) { return kind }
        switch rawValue {
        case "qwen2_vl_3b", "qwen2_5_vl_3b": return .qwen2VL7B
        default:                              return .qwen2VL7B
        }
    }
}

// MARK: - FaceEmbedderKind

/// Per-face embedder used for clustering. SFace (OpenCV Zoo, Apache-2.0,
/// 128-d) — the commercial-clean replacement for the non-commercial
/// InsightFace ArcFace. Still run as ONNX via ONNX Runtime + CoreML EP
/// (same `ArcFaceService` path); only the model file, the embedding
/// dimension (512 → 128), and the input preprocessing changed — SFace
/// takes RAW [0,255] RGB (it bakes its own normalization) rather than
/// ArcFace's `(px − 127.5) / 127.5`. See `ArcFaceService`.
public enum FaceEmbedderKind: String, CaseIterable, Sendable, Codable {
    case sface = "sface"

    public var displayName: String { "SFace (OpenCV Zoo)" }

    public var subtitle: String {
        "Commercial-clean (Apache-2.0) face recognition — 128-d embeddings. Replaces the non-commercial InsightFace ArcFace."
    }

    /// ONNX model filename on disk. Pulled from OpenCV Zoo's HF mirror at
    /// runtime — no redistribution, no on-device conversion.
    public var modelFileName: String { "face_recognition_sface_2021dec.onnx" }

    /// Source HF repo containing the original ONNX.
    public var sourceRepo: String { "opencv/face_recognition_sface" }

    public var approxBytes: Int64 { 38_700_000 }   // ~37 MB ONNX

    /// L2-normalized 128-d float32 (= 512-byte DB blob). Replaces ArcFace's
    /// 512-d (2048-byte) — old prints are wiped by migration v12.
    public var embeddingDim: Int { 128 }

    /// Approximate per-face embedding cost on M1 Pro ANE (ms).
    public var msPerFace: Double { 8.0 }

    public var licenseName: String { "Apache License 2.0 (OpenCV Zoo)" }

    /// SFace is the only (commercial-clean) embedder now; the `ramGB`
    /// parameter is retained for call-site compatibility.
    public static func defaultFor(ramGB _: Double) -> FaceEmbedderKind { .sface }

    /// Installation directory shared between engine + app. Both check
    /// here when deciding whether the upgrade banner should show.
    public static var modelsDirectory: URL {
        FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!
            .appendingPathComponent("FileID/Models", isDirectory: true)
    }

    /// Has the user converted at least one ArcFace .mlpackage to disk?
    public func isInstalled() -> Bool {
        let url = Self.modelsDirectory.appendingPathComponent(modelFileName)
        return FileManager.default.fileExists(atPath: url.path)
    }

    /// All variants the user has installed.
    public static func installedKinds() -> [FaceEmbedderKind] {
        allCases.filter { $0.isInstalled() }
    }
}
