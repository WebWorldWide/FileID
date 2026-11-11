// Stand-ins for the Swift engine types that Engine/DeepAnalyze.swift and its
// helpers use, so the same MLX code runs inside the Rust engine. The Rust host
// owns model choice, paths, licences, memory admission and downloads; these
// shims take its decisions instead of the engine's catalog, scheduler and
// downloader. Nothing here opens a URL, socket or HTTP client or launches a
// process.
import CryptoKit
import Darwin
import Foundation

/// What the Rust host tells the adapter about one model before loading it.
struct VLMModelSpec: Codable, Sendable, Equatable {
    let kind: String
    let repo: String
    /// Folder that holds `models/<repo>`, like the Swift engine's Hugging Face root.
    let huggingFaceRoot: String
    let revision: String?
    let displayName: String
    let vision: Bool
    let ramBudgetGB: Double
    let architecture: String?
    let licenseName: String?
    let licenseAccepted: Bool
}

final class VLMModelRegistry: @unchecked Sendable {
    static let shared = VLMModelRegistry()
    private let lock = NSLock()
    private var specs: [String: VLMModelSpec] = [:]
    private var root: URL?

    func register(_ spec: VLMModelSpec) {
        lock.lock(); defer { lock.unlock() }
        specs[spec.kind] = spec
        root = URL(fileURLWithPath: spec.huggingFaceRoot, isDirectory: true).standardizedFileURL
    }

    func spec(kind: String) -> VLMModelSpec? {
        lock.lock(); defer { lock.unlock() }
        return specs[kind]
    }

    func spec(repo: String) -> VLMModelSpec? {
        lock.lock(); defer { lock.unlock() }
        return specs.values.first { $0.repo == repo }
    }

    var huggingFaceRoot: URL {
        lock.lock(); defer { lock.unlock() }
        return root ?? URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
            .appendingPathComponent("fileid-vlm-unconfigured", isDirectory: true)
    }
}

public struct AIModelKind: RawRepresentable, Hashable, Sendable {
    public let rawValue: String
    public init(rawValue: String) { self.rawValue = rawValue }

    public static let qwen2VL7B = Self(rawValue: "qwen2_5_vl_7b")
    public static let qwen3VL2B = Self(rawValue: "qwen3_vl_2b")
    public static let qwen3VL4B = Self(rawValue: "qwen3_vl_4b")
    public static let qwen3VL8B = Self(rawValue: "qwen3_vl_8b")
    public static let qwen35_2B = Self(rawValue: "qwen3_5_2b")
    public static let gemma3_4B = Self(rawValue: "gemma_3_4b")
    public static let gemma3_12B = Self(rawValue: "gemma_3_12b")
    public static let mistralSmall32 = Self(rawValue: "mistral_small_3_2")
    public static let paligemma3B = Self(rawValue: "paligemma_3b")

    var spec: VLMModelSpec? { VLMModelRegistry.shared.spec(kind: rawValue) }
    public var sourceRepo: String { spec?.repo ?? rawValue }
    public var displayName: String { spec?.displayName ?? rawValue }
    public var supportsVision: Bool { spec?.vision ?? true }
    public var ramBudgetGB: Double { spec?.ramBudgetGB ?? 4 }
    public var licenseName: String { spec?.licenseName ?? "" }
}

struct VLMCatalogEntry: Sendable {
    let architecture: String
}

enum MLXModelCatalog {
    static func entry(forRepo repo: String) -> VLMCatalogEntry? {
        VLMModelRegistry.shared.spec(repo: repo)?.architecture.map(VLMCatalogEntry.init(architecture:))
    }
}

enum ModelLicenseAcceptance {
    static func isAccepted(for kind: AIModelKind) -> Bool { kind.spec?.licenseAccepted ?? false }
}

public enum ModelCachePaths {
    public static var huggingFaceRoot: URL { VLMModelRegistry.shared.huggingFaceRoot }
}

public enum DiscoveredFile {
    public typealias Kind = FileKind
}

/// The IPC `ModelBenchmark` DTO (FileIDShared) the benchmark runner returns.
struct ModelBenchmark: Codable, Sendable, Equatable {
    let measuredAt: String
    let hardware: String
    let loadSeconds: Double
    let promptTokensPerSecond: Double?
    let generatedTokensPerSecond: Double?
    let secondsPerImage: Double?
    let peakResidentGB: Double?
    let jsonValidRate: Double?
    let probeAccuracy: Double?
    let tunedArguments: [String]?
}

extension AIModelKind {
    /// The host only asks for installed models.
    func isInstalledOnDisk() -> Bool { spec != nil }
}

/// Only `ModelBenchmarkStore`, which the adapter never calls, reads this; the
/// Rust engine keeps benchmark results in its own state folder.
enum EngineStateRoot {
    static var directory: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("FileIDAppleVLM-unused", isDirectory: true)
    }
}

public struct CatalogChapter: Sendable, Equatable {
    public var id: String
    public var fileID: Int64
    public var startSeconds: Double
    public var endSeconds: Double
    public var title: String
    public var summary: String
    public var sourceRevision: String
    public var modelVersion: String
    public var confidence: Double
    public var userEdited: Bool
    public var stale: Bool
}

/// JSON lines on stderr, which the Rust host points at `logs/engine-stderr.log`.
public final class JSONLog: @unchecked Sendable {
    public static let shared = JSONLog()
    private let lock = NSLock()
    private let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        return encoder
    }()

    struct Entry: Encodable {
        let t: Date
        let src: String
        let lvl: String
        let ev: String
        let path: String?
        let error: String?
        let extra: [String: AnyCodable]?
    }

    public func info(ev: String, sess: String? = nil, path: String? = nil, extra: [String: AnyCodable]? = nil) {
        write(Entry(t: Date(), src: "FileIDAppleVLM", lvl: "info", ev: ev, path: path, error: nil, extra: extra))
    }
    public func warn(ev: String, sess: String? = nil, path: String? = nil, error: String? = nil) {
        write(Entry(t: Date(), src: "FileIDAppleVLM", lvl: "warn", ev: ev, path: path, error: error, extra: nil))
    }
    public func error(ev: String, sess: String? = nil, path: String? = nil, error: String? = nil) {
        write(Entry(t: Date(), src: "FileIDAppleVLM", lvl: "error", ev: ev, path: path, error: error, extra: nil))
    }
    public func flush() {}

    private func write(_ entry: Entry) {
        guard var line = try? encoder.encode(entry) else { return }
        line.append(0x0A)
        lock.lock(); defer { lock.unlock() }
        FileHandle.standardError.write(line)
    }
}

public struct AnyCodable: Encodable, Sendable {
    private let _encode: @Sendable (Encoder) throws -> Void
    public init<T: Encodable & Sendable>(_ value: T) {
        self._encode = { encoder in try value.encode(to: encoder) }
    }
    public func encode(to encoder: Encoder) throws { try _encode(encoder) }
}

/// The Rust engine's scheduler is the memory authority: it reserves the
/// model's budget before asking for a load. This keeps the engine's call
/// shape and records what DeepAnalyze holds so the host can read it back.
actor ResourceScheduler {
    static let shared = ResourceScheduler()

    struct MemoryLease: Hashable, Sendable {
        let id: UInt64
    }

    struct Demand: Sendable, Equatable {
        let memoryMB: UInt64
        let cpuUnits: Int
        let ioUnits: Int
        init(memoryMB: UInt64 = 0, cpuUnits: Int = 0, ioUnits: Int = 0) {
            self.memoryMB = memoryMB
            self.cpuUnits = cpuUnits
            self.ioUnits = ioUnits
        }
    }

    enum Priority: Int, Sendable {
        case background
        case interactive
    }

    private var next: UInt64 = 0
    private var reservations: [MemoryLease: UInt64] = [:]
    private var retained: Set<MemoryLease> = []

    func reserve(_ demand: Demand, priority: Priority) async throws -> MemoryLease {
        try Task.checkCancellation()
        next &+= 1
        let lease = MemoryLease(id: next)
        reservations[lease] = demand.memoryMB
        return lease
    }

    func retainMemory(_ lease: MemoryLease) {
        retained.insert(lease)
    }

    func release(_ lease: MemoryLease) {
        reservations[lease] = nil
        retained.remove(lease)
    }

    /// Memory DeepAnalyze holds for a loaded model, in MB.
    var retainedMB: UInt64 {
        retained.reduce(0) { $0 + (reservations[$1] ?? 0) }
    }
}

enum VLMDownloaderError: LocalizedError {
    case notInstalled(String)
    var errorDescription: String? {
        switch self {
        case .notInstalled(let reason): return reason
        }
    }
}

/// Offline only: the Rust engine installs models. A load succeeds when the
/// folder holds the pinned revision's `.fileid-verified-<revision>` manifest
/// and every listed file still matches it (same rules as the Swift engine's
/// `VLMDownloader.verifiedSentinelIsValid`), or, for a model the host marks
/// unpinned, when the folder has a config and weights.
final class VLMDownloader: Sendable {
    static let shared = VLMDownloader()

    struct VerifiedSentinel: Codable {
        struct File: Codable {
            let path: String
            let size: Int64
            let sha256: String
        }
        let version: Int
        let revision: String
        let files: [File]
    }

    struct FileIdentity: Codable, Equatable {
        let path: String
        let size: Int64
        let device: Int64
        let inode: UInt64
        let modifiedSeconds: Int64
        let modifiedNanoseconds: Int64
        let changedSeconds: Int64
        let changedNanoseconds: Int64
    }

    struct IdentitySentinel: Codable {
        let version: Int
        let revision: String
        let manifestSHA256: String
        let files: [FileIdentity]
    }

    func fetchRepo(
        repo: String,
        documentsHF: URL,
        progress: @escaping @Sendable (Double, Int64, Int64) -> Void
    ) async throws {
        let modelDir = documentsHF.appending(component: "models").appending(component: repo)
        guard let spec = VLMModelRegistry.shared.spec(repo: repo) else {
            throw VLMDownloaderError.notInstalled("Unregistered model repository.")
        }
        try Task.checkCancellation()
        let valid: Bool
        if let revision = spec.revision {
            valid = await Task.detached(priority: .utility) {
                Self.verified(modelDir: modelDir, revision: revision)
            }.value
        } else {
            valid = Self.looksComplete(modelDir)
        }
        try Task.checkCancellation()
        guard valid else {
            throw VLMDownloaderError.notInstalled("\(spec.displayName) isn't installed or its files changed. Install it again from Settings.")
        }
        progress(1.0, 0, 0)
    }

    static func looksComplete(_ modelDir: URL) -> Bool {
        let manager = FileManager.default
        guard manager.fileExists(atPath: modelDir.appendingPathComponent("config.json").path),
              let names = try? manager.contentsOfDirectory(atPath: modelDir.path) else { return false }
        return names.contains { $0.hasSuffix(".safetensors") }
    }

    static func verified(modelDir: URL, revision: String) -> Bool {
        let sentinel = modelDir.appendingPathComponent(".fileid-verified-\(revision)")
        guard let data = try? Data(contentsOf: sentinel),
              let manifest = try? JSONDecoder().decode(VerifiedSentinel.self, from: data),
              manifest.version == 1,
              manifest.revision == revision,
              !manifest.files.isEmpty else { return false }
        let paths = manifest.files.map(\.path)
        let identityURL = modelDir.appendingPathComponent(sentinel.lastPathComponent + "-v2")
        let digest = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        if let recorded = try? Data(contentsOf: identityURL),
           let record = try? JSONDecoder().decode(IdentitySentinel.self, from: recorded),
           record.version == 2, record.revision == revision, record.manifestSHA256 == digest,
           identities(paths, modelDir: modelDir) == record.files {
            return true
        }
        guard let before = identities(paths, modelDir: modelDir) else { return false }
        for (file, identity) in zip(manifest.files, before) {
            guard identity.size == file.size,
                  let url = safeModelFileURL(file.path, modelDir: modelDir),
                  let actual = try? sha256Hex(url),
                  actual == file.sha256.lowercased() else { return false }
        }
        if identities(paths, modelDir: modelDir) == before,
           (try? ReadOnlyLocations.requireWritable(identityURL)) != nil,
           let encoded = try? JSONEncoder().encode(IdentitySentinel(version: 2, revision: revision, manifestSHA256: digest, files: before)) {
            try? encoded.write(to: identityURL, options: .atomic)
        }
        return true
    }

    static func identities(_ paths: [String], modelDir: URL) -> [FileIdentity]? {
        var result: [FileIdentity] = []
        for path in paths {
            guard let url = safeModelFileURL(path, modelDir: modelDir) else { return nil }
            var info = stat()
            guard lstat(url.path, &info) == 0, info.st_mode & S_IFMT == S_IFREG else { return nil }
            result.append(FileIdentity(
                path: path, size: Int64(info.st_size), device: Int64(info.st_dev), inode: UInt64(info.st_ino),
                modifiedSeconds: Int64(info.st_mtimespec.tv_sec), modifiedNanoseconds: Int64(info.st_mtimespec.tv_nsec),
                changedSeconds: Int64(info.st_ctimespec.tv_sec), changedNanoseconds: Int64(info.st_ctimespec.tv_nsec)
            ))
        }
        return result
    }

    static func safeModelFileURL(_ path: String, modelDir: URL) -> URL? {
        guard !path.hasPrefix("/"), !path.split(separator: "/").contains("..") else { return nil }
        let root = modelDir.standardizedFileURL.path
        let file = modelDir.appendingPathComponent(path).standardizedFileURL
        guard file.path.hasPrefix(root + "/") else { return nil }
        return file
    }

    static func sha256Hex(_ url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: 8 << 20), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}
