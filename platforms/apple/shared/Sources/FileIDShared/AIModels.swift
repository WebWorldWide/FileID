// AIModelKind — VLM choices for Deep Analyze + AI face clustering.
// The app uses this to render the model picker; the engine passes the
// `rawValue` to MLX. Both sides must agree on rawValues used in IPC.
import Foundation

public enum AIModelKind: String, CaseIterable, Sendable, Codable {
    case qwen2VL3B       = "qwen2_vl_3b"
    case qwen3VL4B       = "qwen3_vl_4b"
    case gemma3_4B       = "gemma3_4b"
    case gemma3_12B      = "gemma3_12b"
    case smolvlm         = "smolvlm"
    case paligemma3B     = "paligemma_3b"

    public var displayName: String {
        switch self {
        case .qwen2VL3B:    return "Qwen2.5-VL 3B (4-bit)"
        case .qwen3VL4B:    return "Qwen3-VL 4B (4-bit)"
        case .gemma3_4B:    return "Gemma 3 4B (QAT 4-bit)"
        case .gemma3_12B:   return "Gemma 3 12B (QAT 4-bit)"
        case .smolvlm:      return "SmolVLM Instruct (4-bit)"
        case .paligemma3B:  return "PaliGemma 3B (8-bit)"
        }
    }

    public var subtitle: String {
        switch self {
        case .qwen2VL3B:    return "Default. Strong all-rounder; solid OCR + scene understanding."
        case .qwen3VL4B:    return "Newer architecture. Better OCR + reasoning than Qwen2.5."
        case .gemma3_4B:    return "Google's open model. Strong on grounded VQA."
        case .gemma3_12B:   return "Highest quality. Heavy: ~9 GB resident, ~3× slower."
        case .smolvlm:      return "Tiniest + fastest. Use when battery / RAM matter."
        case .paligemma3B:  return "Strong on grounding, OCR, and visual question answering."
        }
    }

    public var sourceRepo: String {
        switch self {
        case .qwen2VL3B:    return "mlx-community/Qwen2.5-VL-3B-Instruct-4bit"
        case .qwen3VL4B:    return "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit"
        case .gemma3_4B:    return "mlx-community/gemma-3-4b-it-qat-4bit"
        case .gemma3_12B:   return "mlx-community/gemma-3-12b-it-qat-4bit"
        case .smolvlm:      return "mlx-community/SmolVLM-Instruct-4bit"
        case .paligemma3B:  return "mlx-community/paligemma-3b-mix-448-8bit"
        }
    }

    public var approxBytes: Int64 {
        switch self {
        case .qwen2VL3B:    return 3_146_000_000
        case .qwen3VL4B:    return 3_500_000_000
        case .gemma3_4B:    return 3_300_000_000
        case .gemma3_12B:   return 7_500_000_000
        case .smolvlm:      return 600_000_000
        case .paligemma3B:  return 3_300_000_000
        }
    }

    public var ramBudgetGB: Double {
        switch self {
        case .qwen2VL3B:    return 4.0
        case .qwen3VL4B:    return 5.0
        case .gemma3_4B:    return 4.5
        case .gemma3_12B:   return 9.0
        case .smolvlm:      return 1.5
        case .paligemma3B:  return 4.0
        }
    }

    /// Per-image inference cost (seconds). Used to estimate batch ETA.
    public var secondsPerImage: Double {
        switch self {
        case .qwen2VL3B:    return 1.5
        case .qwen3VL4B:    return 2.0
        case .gemma3_4B:    return 1.7
        case .gemma3_12B:   return 5.0
        case .smolvlm:      return 0.7
        case .paligemma3B:  return 1.6
        }
    }

    public var licenseName: String {
        switch self {
        case .qwen2VL3B, .qwen3VL4B, .smolvlm:      return "Apache License 2.0"
        case .gemma3_4B, .gemma3_12B, .paligemma3B: return "Gemma Terms of Use"
        }
    }

    /// Top three picks for a given RAM tier, ranked best-first.
    public static func recommendedFor(ramGB: Double) -> [AIModelKind] {
        if ramGB >= 32       { return [.gemma3_12B, .qwen3VL4B, .qwen2VL3B] }
        else if ramGB >= 16  { return [.qwen3VL4B, .qwen2VL3B, .gemma3_4B] }
        else if ramGB >= 8   { return [.qwen2VL3B, .smolvlm, .paligemma3B] }
        else                 { return [.smolvlm] }
    }

    /// Reserves ~8 GB for system + scan engine + DB cache. A model that
    /// would push past `ramGB - 8` is rejected to avoid OOM-killing the
    /// engine when MLX loads weights.
    public func fits(ramGB: Double) -> Bool {
        let headroom = max(0, ramGB - 8.0)
        return ramBudgetGB <= headroom
    }

    /// First recommendation that fits, falling back to SmolVLM.
    public static func safeDefaultFor(ramGB: Double) -> AIModelKind {
        recommendedFor(ramGB: ramGB).first { $0.fits(ramGB: ramGB) } ?? .smolvlm
    }
}

// MARK: - FaceEmbedderKind

/// Per-face embedder used for clustering. Distinct from `AIModelKind`
/// (which lists VLMs for Deep Analyze) because face embedders have a
/// different shape: small (~13–166 MB), fast (<50 ms per face on ANE),
/// invoked inside the scan loop, and have their own .mlpackage cache
/// path separate from MLX's HF cache.
public enum FaceEmbedderKind: String, CaseIterable, Sendable, Codable {
    case arcfaceIResNet50  = "arcface_iresnet50"
    case arcfaceMobileFace = "arcface_mobileface"

    public var displayName: String {
        switch self {
        case .arcfaceIResNet50:  return "ArcFace iResNet50 (Buffalo-L)"
        case .arcfaceMobileFace: return "ArcFace MobileFace (Buffalo-S)"
        }
    }

    public var subtitle: String {
        switch self {
        case .arcfaceIResNet50:
            return "Industry standard. Same model Immich uses; tightest same-person clusters across age, lighting, and pose."
        case .arcfaceMobileFace:
            return "Compact (~13 MB) alternative — small by design, not by mistake. Near-equal accuracy, picked automatically on 8 GB Macs."
        }
    }

    /// Where the ONNX model file lives on disk. We pull the original
    /// Buffalo ONNX from Immich's HF mirror at runtime — no on-device
    /// conversion, no redistribution of the InsightFace pre-trained
    /// weights on our part.
    public var modelFileName: String {
        switch self {
        case .arcfaceIResNet50:  return "arcface_iresnet50.onnx"
        case .arcfaceMobileFace: return "arcface_mobileface.onnx"
        }
    }

    /// Source HF repo containing the original ONNX. Used by the
    /// conversion script and by the engine's status-check on first run.
    public var sourceRepo: String {
        switch self {
        case .arcfaceIResNet50:  return "immich-app/buffalo_l"
        case .arcfaceMobileFace: return "immich-app/buffalo_s"
        }
    }

    public var approxBytes: Int64 {
        switch self {
        case .arcfaceIResNet50:  return 175_000_000   // ~166 MB ONNX, similar after CoreML pack
        case .arcfaceMobileFace: return 14_000_000    // ~13 MB
        }
    }

    /// L2-normalized 512-d float32. Same for both variants — ArcFace
    /// trains all heads to the same embedding dimension.
    public var embeddingDim: Int { 512 }

    /// Approximate per-face embedding cost on M1 Pro ANE (ms). Used for
    /// migration ETA and progress estimates.
    public var msPerFace: Double {
        switch self {
        case .arcfaceIResNet50:  return 25.0
        case .arcfaceMobileFace: return 10.0
        }
    }

    public var licenseName: String { "MIT (InsightFace) / Apache 2.0 (Immich packaging)" }

    /// Default for the user's Mac. iResNet50 above 16 GB; MobileFace
    /// below. The user can override in Settings.
    public static func defaultFor(ramGB: Double) -> FaceEmbedderKind {
        ramGB >= 16 ? .arcfaceIResNet50 : .arcfaceMobileFace
    }

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
