import Foundation

public enum CatalogModelRole: String, Codable, Sendable { case vision, text }

public struct CatalogModel: Codable, Sendable, Equatable, Identifiable {
    public struct Origin: Codable, Sendable, Equatable {
        public let repo: String
        public let revision: String
        public let license: String
    }
    public struct Weight: Codable, Sendable, Equatable {
        public let path: String
        public let sha256: String
        public let size: Int64
    }
    public struct Quantization: Codable, Sendable, Equatable {
        public let bits: Int
        public let groupSize: Int
        public let mode: String
    }
    public let repo: String
    public let revision: String
    public let license: String
    public let architecture: String
    public let role: CatalogModelRole
    public let weightBytes: Int64
    public let bases: [Origin]
    public let files: [Weight]
    public let quantization: Quantization
    public var id: String { "hf:" + repo }
    public var displayName: String { String(repo.split(separator: "/").last ?? "Model").replacingOccurrences(of: "-", with: " ") }
    public var summary: String { "\(role == .vision ? "Vision + chat" : "Text + chat") · \(quantization.bits)-bit · Quality not evaluated" }
}

public enum MLXModelCatalog {
    public static let textArchitectures: Set<String> = ["acereason", "afmoe", "apertus", "baichuan_m1", "bailing_moe", "bitnet", "cohere", "deepseek_v2", "deepseek_v3", "ernie4_5", "exaone4", "falcon_h1", "gemma", "gemma2", "gemma3", "gemma3_text", "gemma3n", "gemma4", "gemma4_text", "gemma4_unified", "glm4", "glm4_moe", "glm4_moe_lite", "gpt_oss", "granite", "granitemoehybrid", "helium", "hunyuan_v1_dense", "internlm2", "jamba", "lfm2", "lfm2_moe", "lille-130m", "llama", "mamba2", "mimo", "mimo_v2_flash", "minicpm", "minimax", "mistral", "mistral3", "mixtral", "nanbeige", "nanochat", "nemotron_h", "nemotron_labs_diffusion", "olmo2", "olmo3", "olmoe", "openelm", "phi", "phi3", "phimoe", "qwen2", "qwen3", "qwen3_5", "qwen3_5_moe", "qwen3_5_text", "qwen3_moe", "qwen3_next", "smollm3", "starcoder2"]
    public static let visionArchitectures: Set<String> = ["fastvlm", "gemma3", "gemma4", "gemma4_unified", "glm_ocr", "idefics3", "lfm2-vl", "lfm2_vl", "llava_qwen2", "mistral3", "muse_glimmer", "paligemma", "pixtral", "qwen2_5_vl", "qwen2_vl", "qwen3_5", "qwen3_5_moe", "qwen3_vl", "qwen3_vl_moe", "smolvlm"]
    private struct Document: Decodable {
        let formatVersion: Int
        let auditedAt: String
        let runtime: String
        let entries: [CatalogModel]
    }
    public static let entries: [CatalogModel] = {
        let resourceName = "FileID_FileIDShared.bundle"
        let executableDirectory = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
        let candidates = [Bundle.main.resourceURL?.appendingPathComponent(resourceName),
                          Bundle.main.bundleURL.appendingPathComponent(resourceName),
                          executableDirectory.deletingLastPathComponent().appendingPathComponent("Resources").appendingPathComponent(resourceName)]
        let packaged = candidates.compactMap { $0.flatMap(Bundle.init(url:)) }.first
        let bundle = packaged ?? Bundle.module
        guard let url = bundle.url(forResource: "model-catalog", withExtension: "json"), url.isFileURL,
              let handle = try? FileHandle(forReadingFrom: url) else { return [] }
        defer { try? handle.close() }
        guard let data = try? handle.readToEnd(), let result = try? load(data: data) else { return [] }
        return result
    }()
    private static let byID = Dictionary(uniqueKeysWithValues: entries.map { ($0.id, $0) })
    private static let byRepo = Dictionary(uniqueKeysWithValues: entries.map { ($0.repo, $0) })
    public static func entry(forID id: String) -> CatalogModel? { byID[id] }
    public static func entry(forRepo repo: String) -> CatalogModel? { byRepo[repo] }
    public static func load(data: Data) throws -> [CatalogModel] {
        let document = try JSONDecoder().decode(Document.self, from: data)
        let policies: Set<String> = ["apache-2.0", "mit", "bsd-2-clause", "bsd-3-clause", "isc"]
        func hex(_ value: String, count: Int) -> Bool {
            value.count == count && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
        }
        func repo(_ value: String) -> Bool {
            value.range(of: "^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$", options: .regularExpression) != nil && !value.contains("..")
        }
        guard document.formatVersion == 1, document.runtime == "mlx-swift-lm 3.32.3",
              Set(document.entries.map(\.repo)).count == document.entries.count,
              document.entries.allSatisfy({ model in
                  repo(model.repo) && hex(model.revision, count: 40) && policies.contains(model.license) &&
                  (model.role == .vision ? visionArchitectures : textArchitectures).contains(model.architecture) &&
                  !model.bases.isEmpty && model.bases.count <= 20 && model.bases.allSatisfy {
                      repo($0.repo) && hex($0.revision, count: 40) && policies.contains($0.license)
                  } && [2,3,4,5,6,8].contains(model.quantization.bits) && [32,64,128].contains(model.quantization.groupSize) &&
                  model.quantization.mode == "affine" && !model.files.isEmpty && model.files.count <= 128 &&
                  Set(model.files.map(\.path)).count == model.files.count && model.files.allSatisfy {
                      !$0.path.hasPrefix("/") && !$0.path.split(separator: "/").contains("..") &&
                      $0.path.hasSuffix(".safetensors") && hex($0.sha256, count: 64) && $0.size > 0 && $0.size < 1_000_000_000_000
                  } && model.weightBytes == model.files.reduce(Int64(0)) { $0 + $1.size }
              }) else { throw CocoaError(.fileReadCorruptFile) }
        return document.entries
    }
}
