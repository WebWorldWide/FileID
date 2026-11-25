// AIModelKind: VLM choices for Deep Analyze and face clustering. The app renders the picker; the engine passes `rawValue` to MLX.
// rawValues must match the Windows engine's ids byte-for-byte so a shared DB round-trips the "analyzed" state.
import Foundation

public struct AIModelKind: RawRepresentable, CaseIterable, Hashable, Sendable, Codable {
    public let rawValue: String
    private init(registeredValue: String) { rawValue = registeredValue }
    public init?(rawValue: String) {
        guard Self.favorites.contains(where: { $0.rawValue == rawValue }) || MLXModelCatalog.entry(forID: rawValue) != nil else { return nil }
        self.rawValue = rawValue
    }
    public init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer().decode(String.self)
        guard let kind = Self(rawValue: value) else { throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Unregistered model")) }
        self = kind
    }
    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
    // Commercial-clean lineup: Apache-2.0 models plus Gemma/PaliGemma (Gemma Terms permit commercial use). Qwen3.5 122B-A10B tops the ladder on Macs with 96 GB or more; Mistral-Small-3.2 is the pick below that.
    // Mirrors the Windows VLM ladder.
    public static let qwen2VL7B = Self(registeredValue: "qwen2_5_vl_7b")
    public static let qwen3VL2B = Self(registeredValue: "qwen3_vl_2b")
    public static let qwen3VL4B = Self(registeredValue: "qwen3_vl_4b")
    public static let qwen3VL8B = Self(registeredValue: "qwen3_vl_8b")
    public static let qwen35_2B = Self(registeredValue: "qwen3_5_2b")
    public static let gemma3_4B = Self(registeredValue: "gemma_3_4b")
    public static let gemma3_12B = Self(registeredValue: "gemma_3_12b")
    public static let mistralSmall32 = Self(registeredValue: "mistral_small_3_2")
    public static let qwen35_122B = Self(registeredValue: "qwen3_5_122b")
    public static let paligemma3B = Self(registeredValue: "paligemma_3b")


    public static let favorites: [Self] = [.qwen2VL7B, .qwen3VL2B, .qwen3VL4B, .qwen3VL8B, .qwen35_2B, .gemma3_4B, .gemma3_12B, .mistralSmall32, .qwen35_122B, .paligemma3B]
    public static var allCases: [Self] { favorites + MLXModelCatalog.entries.filter { entry in !favorites.contains { $0.sourceRepo == entry.repo } }.map { Self(registeredValue: $0.id) } }
    public var catalogEntry: CatalogModel? { MLXModelCatalog.entry(forRepo: sourceRepo) }
    public var supportsVision: Bool { MLXModelCatalog.entry(forID: rawValue)?.role != .text }
    public static var visionModels: [Self] { allCases.filter(\.supportsVision) }
    public var displayName: String {
        switch self {
        case .qwen2VL7B:     return "Qwen2.5-VL 7B (4-bit)"
        case .qwen3VL2B:     return "Qwen3-VL 2B (4-bit)"
        case .qwen3VL4B:     return "Qwen3-VL 4B (4-bit)"
        case .qwen3VL8B:     return "Qwen3-VL 8B (4-bit)"
        case .qwen35_2B:     return "Qwen3.5 2B (4-bit)"
        case .gemma3_4B:     return "Gemma 3 4B (QAT 4-bit)"
        case .gemma3_12B:    return "Gemma 3 12B (QAT 4-bit)"
        case .mistralSmall32: return "Mistral Small 3.2 24B (4-bit)"
        case .qwen35_122B:   return "Qwen3.5 122B-A10B (4-bit)"
        case .paligemma3B:   return "PaliGemma 3B (8-bit)"
        default: return MLXModelCatalog.entry(forID: rawValue)?.displayName ?? rawValue
        }
    }

    public var subtitle: String {
        switch self {
        case .qwen2VL7B:     return "Good at reading text in pictures and describing scenes. Medium speed. Needs a Mac with 16 GB of memory."
        case .qwen3VL2B:     return "The fastest choice. Descriptions are a little less detailed. Works on Macs with 8 GB of memory."
        case .qwen3VL4B:     return "Best choice for Macs with 8 GB of memory. Accurate descriptions without using much memory."
        case .qwen3VL8B:     return "Best choice for Macs with 16 GB of memory. More detail, and better at reading text in pictures. Slower."
        case .qwen35_2B:     return "Small and light, for Macs with little memory. New, so results may vary."
        case .gemma3_4B:     return "Made by Google. Good at answering questions about a picture. Fast, and works on Macs with 8 GB of memory."
        case .gemma3_12B:    return "Very detailed descriptions. About three times slower, and needs a Mac with 16 GB of memory or more."
        case .mistralSmall32: return "The most detailed descriptions. The slowest choice. Works best on a Mac with 32 GB of memory."
        case .qwen35_122B:   return "The largest model, for Macs with 96 GB of memory or more. A 70 GB download."
        case .paligemma3B:   return "Good at finding things in a picture and reading text. Fast, and works on Macs with 8 GB of memory."
        default: return MLXModelCatalog.entry(forID: rawValue)?.summary ?? "Extra model"
        }
    }

    private var manifestPin: VLMRepoPin? { ModelManifest.vlmPin(forKind: rawValue) }

    public var sourceRepo: String { manifestPin?.repo ?? MLXModelCatalog.entry(forID: rawValue)!.repo }

    public var approxBytes: Int64 { manifestPin?.approxBytes ?? MLXModelCatalog.entry(forID: rawValue)?.weightBytes ?? 0 }

    public var ramBudgetGB: Double {
        switch self {
        case .qwen2VL7B:     return 7.0
        case .qwen3VL2B:     return 3.0
        case .qwen3VL4B:     return 5.0
        case .qwen3VL8B:     return 8.5
        case .qwen35_2B:     return 3.0
        case .gemma3_4B:     return 4.5
        case .gemma3_12B:    return 9.0
        case .mistralSmall32: return 16.0
        case .qwen35_122B:   return 72.0
        case .paligemma3B:   return 4.0
        default: return Double(approxBytes) / 1_073_741_824 * 1.5 + 2
        }
    }

    /// Per-image inference cost (seconds). Used to estimate batch ETA.
    public var secondsPerImage: Double {
        switch self {
        case .qwen2VL7B:     return 2.5
        case .qwen3VL4B:     return 5.0
        case .qwen3VL8B:     return 7.0
        case .qwen35_2B:     return 3.0
        case .gemma3_4B:     return 1.7
        case .gemma3_12B:    return 5.0
        case .mistralSmall32: return 6.0
        case .paligemma3B:   return 1.6
        default: return 0
        }
    }

    public var licensePolicyKey: String {
        if let manifestPin { return manifestPin.license }
        return ["apache-2.0":"Apache-2.0", "mit":"MIT", "bsd-2-clause":"BSD-2-Clause", "bsd-3-clause":"BSD-3-Clause", "isc":"ISC"][MLXModelCatalog.entry(forID: rawValue)?.license ?? ""] ?? "Unknown"
    }

    public var licenseName: String {
        guard manifestPin != nil else { return licensePolicyKey }
        switch licensePolicyKey {
        case "Apache-2.0": return "Apache License 2.0"
        case "Gemma": return "Gemma Terms of Use"
        default: return licensePolicyKey
        }
    }

    public var licenseTermsURL: URL? {
        guard let policy = ModelManifest.licensePolicies[licensePolicyKey], policy.termsRequired else { return nil }
        return policy.licenseURL.flatMap { URL(string: $0) }
    }

    /// Picks for a given RAM tier, ranked best-first; the smaller Qwen3-VL models close each list so a busy Mac always has one that loads.
    public static func recommendedFor(ramGB: Double) -> [AIModelKind] {
        if ramGB >= 80       { return [.qwen35_122B, .mistralSmall32, .qwen3VL8B, .qwen3VL4B, .qwen3VL2B] }
        else if ramGB >= 30  { return [.mistralSmall32, .qwen3VL8B, .qwen3VL4B, .qwen3VL2B] }
        else if ramGB >= 16  { return [.qwen3VL8B, .qwen3VL4B, .qwen2VL7B, .qwen3VL2B] }
        else                 { return [.qwen3VL4B, .qwen3VL2B, .gemma3_4B, .paligemma3B] }
    }

    /// What the engine's memory admission asks for when it loads this model.
    public var requestedMB: UInt64 {
        UInt64((ramBudgetGB * 1024).rounded(.up))
    }

    private static func totalMB(ramGB: Double) -> UInt64 {
        UInt64(max(0, ramGB * 1024))
    }

    /// The engine's static rule: the model fits the installed memory after the system reserve.
    public func fits(ramGB: Double) -> Bool {
        ModelMemoryRule.fitsBudget(totalMB: Self.totalMB(ramGB: ramGB), requestedMB: requestedMB)
    }

    /// The engine's live rule: the model would load with `availableMB` free right now.
    public func fitsNow(ramGB: Double, availableMB: UInt64) -> Bool {
        ModelMemoryRule.fitsNow(totalMB: Self.totalMB(ramGB: ramGB), availableMB: availableMB, requestedMB: requestedMB)
    }

    public var requiredFreeBytes: Int64 {
        approxBytes * 2 + 512 * 1_048_576
    }

    public func fits(freeDiskBytes: Int64?) -> Bool {
        guard let freeDiskBytes else { return true }
        return freeDiskBytes >= requiredFreeBytes
    }

    /// The recommendation the engine's rule accepts: the first that loads with `availableMB` free, else the smallest this Mac could ever run, falling back to Gemma 3 4B. `availableMB` nil asks only about the installed memory.
    public static func safeDefaultFor(ramGB: Double,
                                      availableMB: UInt64? = nil,
                                      freeDiskBytes: Int64? = nil) -> AIModelKind {
        let ranked = recommendedFor(ramGB: ramGB).filter { $0.fits(ramGB: ramGB) }
        let diskOK = ranked.filter { $0.fits(freeDiskBytes: freeDiskBytes) }
        for pool in [diskOK, ranked] where !pool.isEmpty {
            if let availableMB {
                let ladder = pool.map { (id: $0.rawValue, requestedMB: $0.requestedMB) }
                if let id = ModelMemoryRule.recommend(ladder: ladder, totalMB: totalMB(ramGB: ramGB), availableMB: availableMB) {
                    return pool.first { $0.rawValue == id } ?? pool[0]
                }
            }
            return pool[0]
        }
        return .gemma3_4B
    }

    public static func onboardingSelection(persistedRawValue: String?,
                                           ramGB: Double,
                                           availableMB: UInt64? = nil,
                                           freeDiskBytes: Int64? = nil) -> AIModelKind {
        if let persistedRawValue, !persistedRawValue.isEmpty {
            return migrated(rawValue: persistedRawValue)
        }
        return safeDefaultFor(ramGB: ramGB, availableMB: availableMB, freeDiskBytes: freeDiskBytes)
    }

    /// Migrates a persisted rawValue that predates the commercial-clean lineup or the Windows-canonical token rename, so an old value maps to a supported model instead of failing to decode.
    public static func migrated(rawValue: String) -> AIModelKind {
        if let kind = AIModelKind(rawValue: rawValue) { return kind }
        switch rawValue {
        // Tokens persisted before the Windows-canonical rename.
        case "qwen2_vl_7b":                  return .qwen2VL7B
        case "gemma3_4b":                    return .gemma3_4B
        case "gemma3_12b":                   return .gemma3_12B
        // Dropped non-commercial Qwen2.5-VL-3B (Qwen Research License).
        case "qwen2_vl_3b", "qwen2_5_vl_3b": return fallbackForThisMac
        default:                              return fallbackForThisMac
        }
    }

    /// An unknown or dropped saved choice becomes the pick that fits this Mac,
    /// never a model that needs more memory than it has.
    private static var fallbackForThisMac: AIModelKind {
        safeDefaultFor(ramGB: Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824)
    }
}


/// Per-face embedder for clustering: SFace (OpenCV Zoo, Apache-2.0, 128-d), the commercial-clean replacement for InsightFace ArcFace. Runs as ONNX via ONNX Runtime + CoreML EP through `ArcFaceService`.
/// SFace takes raw [0,255] RGB and bakes in its own normalization, unlike ArcFace's (px − 127.5) / 127.5.
public enum FaceEmbedderKind: String, CaseIterable, Sendable, Codable {
    case sface = "sface"

    public var displayName: String { "SFace (OpenCV Zoo)" }

    public var subtitle: String {
        "Tells which faces belong to the same person. Small and fast."
    }

    /// ONNX model filename on disk. Pulled from OpenCV Zoo's HF mirror at
    /// runtime — no redistribution, no on-device conversion.
    public var modelFileName: String { "face_recognition_sface_2021dec.onnx" }

    /// Source HF repo containing the original ONNX.
    public var sourceRepo: String { "opencv/face_recognition_sface" }

    public var approxBytes: Int64 { 38_700_000 }   // ~37 MB ONNX

    /// L2-normalized 128-d float32 (512-byte DB blob). ArcFace's 512-d prints are not compatible.
    public var embeddingDim: Int { 128 }

    /// Approximate per-face embedding cost on M1 Pro ANE (ms).
    public var msPerFace: Double { 8.0 }

    public var licenseName: String { "Apache License 2.0 (OpenCV Zoo)" }

    /// SFace is the only embedder; `ramGB` is kept for call-site compatibility.
    public static func defaultFor(ramGB _: Double) -> FaceEmbedderKind { .sface }

    /// Installation directory shared between engine + app. Both check
    /// here when deciding whether the upgrade banner should show.
    public static var modelsDirectory: URL {
        (FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? UserHome.url.appendingPathComponent("Library/Application Support", isDirectory: true))
            .appendingPathComponent("FileID/Models", isDirectory: true)
    }

    /// The engine keeps the weights under `sface/`; the app's own installer used the top level. Either counts.
    public func installedFile(in root: URL = FaceEmbedderKind.modelsDirectory) -> URL? {
        let candidates = [
            root.appendingPathComponent(modelFileName),
            root.appendingPathComponent("sface", isDirectory: true).appendingPathComponent(modelFileName),
        ]
        return candidates.first { FileManager.default.fileExists(atPath: $0.path) }
    }

    public func isInstalled(in root: URL = FaceEmbedderKind.modelsDirectory) -> Bool {
        installedFile(in: root) != nil
    }

    /// All variants the user has installed.
    public static func installedKinds() -> [FaceEmbedderKind] {
        allCases.filter { $0.isInstalled() }
    }
}
