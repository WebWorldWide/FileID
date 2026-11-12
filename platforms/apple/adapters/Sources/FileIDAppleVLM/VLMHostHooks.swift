// What FileIDVLMCore needs from the Rust host. Model facts (repository,
// display name, memory budget, architecture) come from the shared catalog in
// FileIDModelInfo, as in the Swift engine; the host's spec for each load must
// agree with it, or the load is refused. Models are verified offline, never
// downloaded. Nothing here opens a URL, socket or HTTP client or launches a
// process.
import CryptoKit
import Darwin
import FileIDModelInfo
import FileIDVLMCore
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

    /// Checks `spec` against the shared catalog and records it. The Rust host
    /// and this library must name the same repository, pinned revision and
    /// model folder.
    func register(_ spec: VLMModelSpec) throws -> AIModelKind {
        if let problem = Self.disagreement(spec) { throw VLMFailure(message: problem) }
        lock.lock(); defer { lock.unlock() }
        specs[spec.kind] = spec
        return AIModelKind(rawValue: spec.kind)!
    }

    func spec(repo: String) -> VLMModelSpec? {
        lock.lock(); defer { lock.unlock() }
        return specs.values.first { $0.repo == repo }
    }

    /// Why the host's spec doesn't match the catalog this library reads, or nil.
    static func disagreement(_ spec: VLMModelSpec, huggingFaceRoot: URL = ModelCachePaths.huggingFaceRoot) -> String? {
        guard let kind = AIModelKind(rawValue: spec.kind) else {
            return "\(spec.displayName) isn't in this build's model catalog."
        }
        if kind.sourceRepo != spec.repo {
            return "The engine and the model catalog name different repositories for \(spec.displayName)."
        }
        if let pin = ModelManifest.vlmPin(forKind: spec.kind), pin.revision != spec.revision {
            return "The engine and the model catalog pin different revisions of \(spec.displayName)."
        }
        let root = URL(fileURLWithPath: spec.huggingFaceRoot, isDirectory: true).standardizedFileURL.path
        if root != huggingFaceRoot.standardizedFileURL.path {
            return "The engine and the model catalog use different model folders."
        }
        return nil
    }

    /// The Rust host's offline check as FileIDVLMCore's model fetch.
    static func installHostHooks() {
        VLMCoreHost.install(fetchRepo: { repo, documentsHF, progress in
            try await VLMDownloader.shared.fetchRepo(repo: repo, documentsHF: documentsHF, progress: progress)
        })
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

    static func read(_ url: URL) -> Data? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        return try? handle.readToEnd()
    }

    static func verified(modelDir: URL, revision: String) -> Bool {
        let sentinel = modelDir.appendingPathComponent(".fileid-verified-\(revision)")
        guard let data = read(sentinel),
              let manifest = try? JSONDecoder().decode(VerifiedSentinel.self, from: data),
              manifest.version == 1,
              manifest.revision == revision,
              !manifest.files.isEmpty else { return false }
        let paths = manifest.files.map(\.path)
        let identityURL = modelDir.appendingPathComponent(sentinel.lastPathComponent + "-v2")
        let digest = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        if let recorded = read(identityURL),
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
