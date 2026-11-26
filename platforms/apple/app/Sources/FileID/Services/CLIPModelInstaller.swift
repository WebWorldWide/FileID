// In-app downloader for the CLIP ViT-B/32 image and text encoders and BPE
// vocab, as pinned in shared/models/manifest.json — per-file streaming with
// up to 3 concurrent fetches.
import Foundation
import AppKit
import CryptoKit
import Darwin
import FileIDShared

@MainActor
@Observable
final class CLIPModelInstaller: ModelInstalling {

    static let shared = CLIPModelInstaller()

    typealias Status = ModelInstaller.Status

    private(set) var status: Status = .unknown
    /// On-disk presence of each required file, refreshed only on real disk
    /// transitions (appear / install / uninstall) via refreshStatus — never on
    /// download ticks. The Settings card reads this instead of calling
    /// FileManager.fileExists in its row builder, so an active download does not
    /// fire a stat() syscall on the main thread on every progress tick.
    private(set) var presentFilePaths: Set<String> = []
    /// True once the CLIP files are on disk, so the engine can embed search
    /// text — Library observes it to drop the keyword-only hint and re-run
    /// the active search.
    private(set) var textEncoderReady = false
    private var task: Task<Void, Never>?
    /// True only while a hub fetch is actively running. Progress ticks
    /// arrive as queued MainActor tasks and can be scheduled AFTER the
    /// catch arm wrote a terminal status — gate `publishFromTracker` on
    /// liveness so a stale tick can't resurrect a phantom "Downloading…"
    /// footer with a dead Cancel button.
    private var installing = false
    private var uninstalling = false

    private init() {}

    // MARK: - Required files

    static var requiredFiles: [URL] {
        let models = modelsRoot
        return [
            models.appendingPathComponent("mobileclip_image/clip_vitb32_image.onnx"),
            models.appendingPathComponent("clip_text/clip_text.onnx"),
            models.appendingPathComponent("clip_text/vocab.json"),
            models.appendingPathComponent("clip_text/merges.txt"),
        ]
    }

    static var modelsRoot: URL { AppSupportPath.models }

    static var modelDirectories: [URL] {
        [modelsRoot.appendingPathComponent("mobileclip_image", isDirectory: true),
         modelsRoot.appendingPathComponent("clip_text", isDirectory: true)]
    }

    static let approxDownloadBytes: Int64 = {
        let ids = Set(["clip_vitb32_image", "clip_vitb32_text", "clip_bpe_vocab", "clip_bpe_merges"])
        return ModelManifest.artifacts
            .filter { ids.contains($0.id) }
            .reduce(0) { $0 + $1.approxBytes }
    }()

    private static var fetchPlan: [(dest: URL, sha256: String?)] {
        let m = modelsRoot
        let txtDir = m.appendingPathComponent("clip_text")
        let plan: [(String, URL)] = [
            ("clip_vitb32_image", m.appendingPathComponent("mobileclip_image/clip_vitb32_image.onnx")),
            ("clip_vitb32_text", txtDir.appendingPathComponent("clip_text.onnx")),
            ("clip_bpe_vocab", txtDir.appendingPathComponent("vocab.json")),
            ("clip_bpe_merges", txtDir.appendingPathComponent("merges.txt")),
        ]
        return plan.compactMap { id, dest in
            guard let artifact = ModelManifest.artifact(id: id) else { return nil }
            return (dest: dest, sha256: artifact.sha256)
        }
    }

    // MARK: - Status

    func markTextEncoderReady() {
        textEncoderReady = true
    }

    func refreshStatus() {
        recomputePresentFiles()
        var totalSize: Int64 = 0
        var firstMissing: String?
        for f in Self.requiredFiles where !presentFilePaths.contains(f.path) && firstMissing == nil {
            firstMissing = f.lastPathComponent
        }
        if firstMissing != nil {
            if status.inProgress, task != nil { return }
            if case .installFailed = status { return }
            status = .missing(reason: ModelInstaller.notDownloaded)
        } else {
            for dir in Self.modelDirectories { totalSize += ModelInstaller.diskSize(of: dir) }
            for file in Self.requiredFiles where !FileManager.default.fileExists(atPath: file.path) {
                if let other = ModelFileLayout.installedFile(file, in: Self.modelsRoot) { totalSize += ModelInstaller.diskSize(of: other) }
            }
            status = .installed(sizeBytes: totalSize)
        }
    }

    /// Refresh on-disk presence WITHOUT touching `status`. The partial-install
    /// failure paths (promote / verify / extract) leave some required files in
    /// the live tree but must keep their `.installFailed` status — calling
    /// refreshStatus() there would overwrite it with .missing/.installed, so they
    /// call this instead to keep the per-file Settings rows accurate.
    private func recomputePresentFiles() {
        var present: Set<String> = []
        for f in Self.requiredFiles where ModelFileLayout.installedFile(f, in: Self.modelsRoot) != nil {
            present.insert(f.path)
        }
        presentFilePaths = present
    }

    // MARK: - Install paths

    func install() {
        guard task == nil, !uninstalling else { return }
        task = Task { [weak self] in
            await AppSleepActivity.run(reason: "Install CLIP models") {
                await self?.runEngineInstall()
            }
            self?.task = nil
        }
    }

    /// Air-gapped fallback. Expects the same layout the hub fetch
    /// produces — mobileclip_image/… and clip_text/… at the top.
    func installFromLocalZip(_ zipURL: URL) {
        guard task == nil, !uninstalling else { return }
        task = Task { [weak self] in
            await AppSleepActivity.run(reason: "Install local CLIP models") {
                await self?.runExtract(zipAt: zipURL, deleteZipAfter: false)
            }
            self?.task = nil
        }
    }

    func cancel() {
        task?.cancel()
    }

    func uninstall() async {
        guard (try? ReadOnlyLocations.requireWritable(Self.modelsRoot)) != nil else { status = .installFailed(ModelInstaller.protectedMessage); return }
        guard !uninstalling else { return }
        uninstalling = true
        defer { uninstalling = false }
        let activeTask = task
        activeTask?.cancel()
        await activeTask?.value
        task = nil
        for d in Self.modelDirectories {
            try? FileManager.default.removeItem(at: d)
        }
        for f in Self.requiredFiles {
            if let other = ModelFileLayout.alternate(of: f, in: Self.modelsRoot) { try? FileManager.default.removeItem(at: other) }
        }
        // Drop the readiness flag so Library re-shows the "reinstall CLIP" hint
        // and falls back to keyword-only search. (Install→Remove leaked it true.)
        textEncoderReady = false
        status = .unknown
        refreshStatus()
    }

    // MARK: - Implementation

    private func runEngineInstall() async {
        guard EngineModelInstall.active else {
            status = .installFailed(EngineModelInstall.notResponding)
            return
        }
        installing = true
        defer { installing = false }
        status = .downloading(fraction: 0, message: "Connecting…", bytesPerSecond: 0, etaSeconds: 0)
        let outcome = await EngineModelInstall.install(kinds: ["mobileclip_s2", "clip_text"], files: Self.requiredFiles) { [weak self] fraction, message in
            guard let self, self.installing else { return }
            self.status = .downloading(fraction: fraction, message: message, bytesPerSecond: 0, etaSeconds: 0)
        }
        switch outcome {
        case .finished:
            markTextEncoderReady()
            refreshStatus()
        case .cancelled:
            status = .missing(reason: "Cancelled.")
        case .failed(let message):
            status = .installFailed(message)
            recomputePresentFiles()
        }
    }

    /// `.clip-staging-<UUID>` roots from a process killed mid-install
    /// (the cleanup `defer` above dies with the process) strand ~250 MB
    /// each and eat into the free-space preflight. Wholesale removal is
    /// safe: only this installer creates them, one install task runs at
    /// a time (`guard task == nil`), and none can be in flight here.
    private static func sweepOrphanedStagingRoots() {
        guard (try? ReadOnlyLocations.requireWritable(modelsRoot)) != nil else { return }
        let fm = FileManager.default
        guard let entries = try? fm.contentsOfDirectory(
            at: modelsRoot, includingPropertiesForKeys: nil) else { return }
        for entry in entries where entry.lastPathComponent.hasPrefix(".clip-staging-") {
            try? fm.removeItem(at: entry)
        }
    }

    private func runExtract(zipAt zipURL: URL, deleteZipAfter: Bool) async {
        guard (try? ReadOnlyLocations.requireWritable(Self.modelsRoot)) != nil else { status = .installFailed(ModelInstaller.protectedMessage); return }
        status = .extracting
        let modelsRoot = Self.modelsRoot
        try? FileManager.default.createDirectory(at: modelsRoot, withIntermediateDirectories: true)
        Self.sweepOrphanedStagingRoots()

        var isDir: ObjCBool = false
        guard FileManager.default.fileExists(atPath: zipURL.path, isDirectory: &isDir),
              !isDir.boolValue else {
            status = .installFailed("Zip file not found at \(zipURL.lastPathComponent).")
            return
        }
        guard zipURL.pathExtension.lowercased() == "zip" else {
            status = .installFailed("That isn’t a .zip file. Choose the model archive you downloaded.")
            return
        }
        if let attrs = try? FileManager.default.attributesOfItem(atPath: zipURL.path),
           let type = attrs[.type] as? FileAttributeType, type != .typeRegular {
            status = .installFailed("That file can’t be used. Choose the model archive itself, not a link to it.")
            return
        }

        let minFreeBytes: Int64 = 1_073_741_824
        if let fsAttrs = try? FileManager.default.attributesOfFileSystem(forPath: modelsRoot.path),
           let free = (fsAttrs[.systemFreeSize] as? NSNumber)?.int64Value,
           free < minFreeBytes {
            status = .installFailed("Not enough free space to unpack the model archive: it needs about 1 GB and only \(ModelSize.label(free)) is free.")
            return
        }

        let stagingRoot = modelsRoot.appendingPathComponent(
            ".clip-staging-\(UUID().uuidString)", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: stagingRoot, withIntermediateDirectories: false)
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: stagingRoot.path)
        } catch {
            status = .installFailed("Couldn’t prepare space to unpack the model archive. Check that there’s free disk space, then try again.")
            return
        }
        defer { try? FileManager.default.removeItem(at: stagingRoot) }

        do {
            try Task.checkCancellation()
            let expandedBytes = try await preflightArchive(zipURL, workRoot: stagingRoot)
            let requiredFree = expandedBytes * 2 + 256 * 1_024 * 1_024
            if let fsAttrs = try? FileManager.default.attributesOfFileSystem(forPath: modelsRoot.path),
               let free = (fsAttrs[.systemFreeSize] as? NSNumber)?.uint64Value,
               free < requiredFree {
                status = .installFailed("Not enough free space to unpack the model archive: it needs about \(ModelSize.label(Int64(clamping: requiredFree))).")
                recomputePresentFiles()
                return
            }
            let result = try await runUnzip(
                ["-q", zipURL.path, "-d", stagingRoot.path],
                workRoot: stagingRoot,
                timeoutSeconds: 300,
                label: "extract")
            guard result.status == 0 else {
                throw LocalArchiveError("unzip failed (\(result.status)): \(result.error)")
            }
            try Task.checkCancellation()

            let plan = Self.fetchPlan.map { item -> (staged: URL, finalDest: URL, sha256: String?) in
                let relative = item.dest.path.dropFirst(modelsRoot.path.count + 1)
                return (
                    stagingRoot.appendingPathComponent(String(relative)),
                    item.dest,
                    item.sha256
                )
            }
            guard plan.count == Self.requiredFiles.count else {
                throw LocalArchiveError("the canonical CLIP manifest is incomplete")
            }
            for item in plan {
                try Task.checkCancellation()
                let values = try item.staged.resourceValues(forKeys: [
                    .isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey,
                ])
                guard values.isRegularFile == true, values.isSymbolicLink != true else {
                    throw LocalArchiveError("\(item.staged.lastPathComponent) isn’t a regular file")
                }
                guard let expected = item.sha256 else {
                    throw LocalArchiveError("\(item.staged.lastPathComponent) has no canonical SHA-256 pin")
                }
                let actual = try sha256File(item.staged)
                guard actual == expected.lowercased() else {
                    throw LocalArchiveError(
                        "\(item.staged.lastPathComponent) failed integrity verification")
                }
            }
            try rejectSpecialFiles(in: stagingRoot)
            try Task.checkCancellation()
            try promoteStaged(plan.map { (staged: $0.staged, finalDest: $0.finalDest) })
        } catch is CancellationError {
            status = .missing(reason: "Cancelled.")
            recomputePresentFiles()
            return
        } catch {
            NSLog("FileID model archive rejected: \(error.localizedDescription)")
            status = .installFailed("That model archive isn’t valid, so nothing was installed. Download it again, then choose it again.")
            recomputePresentFiles()
            return
        }

        if deleteZipAfter {
            try? FileManager.default.removeItem(at: zipURL)
        }
        markTextEncoderReady()
        refreshStatus()
    }

    func preflightArchive(_ zipURL: URL, workRoot: URL) async throws -> UInt64 {
        let namesResult = try await runUnzip(
            ["-Z1", zipURL.path], workRoot: workRoot,
            timeoutSeconds: 30, label: "names")
        guard namesResult.status == 0 else {
            throw LocalArchiveError("couldn’t list archive entries: \(namesResult.error)")
        }
        let names = namesResult.output.split(whereSeparator: \.isNewline).map(String.init)
        guard names.count <= 10_000 else {
            throw LocalArchiveError("archive has more than 10,000 entries")
        }
        for name in names where !LocalArchiveSafety.entryNameIsSafe(name) {
            throw LocalArchiveError("archive contains an unsafe entry name")
        }

        let verbose = try await runUnzip(
            ["-Z", "-v", zipURL.path], workRoot: workRoot,
            timeoutSeconds: 30, label: "metadata")
        guard verbose.status == 0 else {
            throw LocalArchiveError("couldn’t inspect archive metadata: \(verbose.error)")
        }
        var total: UInt64 = 0
        for rawLine in verbose.output.split(whereSeparator: \.isNewline) {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("uncompressed size:") {
                let fields = line.split(whereSeparator: \.isWhitespace)
                guard fields.count >= 3, let size = UInt64(fields[2]), size <= 1_073_741_824 else {
                    throw LocalArchiveError("archive contains an oversized entry")
                }
                total = total.addingReportingOverflow(size).overflow ? UInt64.max : total + size
                if total > 2_147_483_648 {
                    throw LocalArchiveError("archive expands past the 2 GB safety cap")
                }
            }
            if line.hasPrefix("file security status:"),
               let value = line.split(separator: ":", maxSplits: 1).last,
               !LocalArchiveSafety.fileSecurityStatusIsUnencrypted(String(value)) {
                throw LocalArchiveError("encrypted archives aren’t supported")
            }
            if line.hasPrefix("Unix file attributes"),
               let permissions = line.split(separator: ":", maxSplits: 1).last?
                    .trimmingCharacters(in: .whitespaces),
               !LocalArchiveSafety.unixEntryTypeIsSafe(permissions) {
                throw LocalArchiveError("archive contains a symlink or special entry")
            }
        }
        return total
    }

    private func runUnzip(
        _ arguments: [String],
        workRoot: URL,
        timeoutSeconds: UInt64,
        label: String
    ) async throws -> (status: Int32, output: String, error: String) {
        let outputURL = workRoot.appendingPathComponent(".\(label)-stdout")
        let errorURL = workRoot.appendingPathComponent(".\(label)-stderr")
        FileManager.default.createFile(atPath: outputURL.path, contents: nil)
        FileManager.default.createFile(atPath: errorURL.path, contents: nil)
        let outputHandle = try FileHandle(forWritingTo: outputURL)
        let errorHandle = try FileHandle(forWritingTo: errorURL)
        defer {
            try? outputHandle.close()
            try? errorHandle.close()
            try? FileManager.default.removeItem(at: outputURL)
            try? FileManager.default.removeItem(at: errorURL)
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/unzip")
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = outputHandle
        process.standardError = errorHandle
        let timedOut = MutexBox(false)
        let cancelled = MutexBox(false)
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                process.terminationHandler = { _ in continuation.resume() }
                do {
                    try process.run()
                } catch {
                    continuation.resume(throwing: error)
                    return
                }
                if Task.isCancelled || cancelled.withLock({ $0 }) {
                    cancelled.withLock { $0 = true }
                    process.terminate()
                }
                Task.detached {
                    try? await Task.sleep(nanoseconds: timeoutSeconds * 1_000_000_000)
                    if process.isRunning {
                        timedOut.withLock { $0 = true }
                        process.terminate()
                        try? await Task.sleep(nanoseconds: 2_000_000_000)
                        if process.isRunning {
                            Darwin.kill(process.processIdentifier, SIGKILL)
                        }
                    }
                }
            }
        } onCancel: {
            cancelled.withLock { $0 = true }
            if process.isRunning {
                process.terminate()
                Task.detached {
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                    if process.isRunning {
                        Darwin.kill(process.processIdentifier, SIGKILL)
                    }
                }
            }
        }
        try outputHandle.synchronize()
        try errorHandle.synchronize()
        if cancelled.withLock({ $0 }) { throw CancellationError() }
        if timedOut.withLock({ $0 }) {
            throw LocalArchiveError("unzip \(label) timed out")
        }
        let output = try readSmallText(outputURL, cap: 4 * 1_024 * 1_024)
        let error = try readSmallText(errorURL, cap: 256 * 1_024)
        return (process.terminationStatus, output, error)
    }

    private func readSmallText(_ url: URL, cap: Int) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        let data = try handle.read(upToCount: cap + 1) ?? Data()
        guard data.count <= cap else { throw LocalArchiveError("archive listing was too large") }
        guard let text = String(data: data, encoding: .utf8) else {
            throw LocalArchiveError("archive listing wasn’t UTF-8")
        }
        return text
    }

    private func sha256File(_ url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let data = try handle.read(upToCount: 1_024 * 1_024), !data.isEmpty {
            hasher.update(data: data)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    private func rejectSpecialFiles(in root: URL) throws {
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: [.isDirectoryKey, .isRegularFileKey, .isSymbolicLinkKey],
            options: []) else {
            throw LocalArchiveError("couldn’t inspect extracted files")
        }
        var count = 0
        for case let url as URL in enumerator {
            count += 1
            guard count <= 10_000 else { throw LocalArchiveError("too many extracted entries") }
            let values = try url.resourceValues(forKeys: [
                .isDirectoryKey, .isRegularFileKey, .isSymbolicLinkKey,
            ])
            guard values.isSymbolicLink != true,
                  values.isDirectory == true || values.isRegularFile == true else {
                throw LocalArchiveError("archive extracted a special filesystem object")
            }
        }
    }

    private func promoteStaged(_ plan: [(staged: URL, finalDest: URL)]) throws {
        try Self.promoteStaged(plan, modelsRoot: Self.modelsRoot)
    }

    static func promoteStaged(
        _ plan: [(staged: URL, finalDest: URL)],
        modelsRoot: URL,
        moveItem: (URL, URL) throws -> Void = { source, destination in
            try FileManager.default.moveItem(at: source, to: destination)
        },
        removeItem: (URL) throws -> Void = { url in
            try FileManager.default.removeItem(at: url)
        }
    ) throws {
        let backupRoot = modelsRoot.appendingPathComponent(
            ".clip-backup-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: backupRoot, withIntermediateDirectories: false)
        var backedUp: [(backup: URL, finalDest: URL)] = []
        var promoted: [(staged: URL, finalDest: URL)] = []
        do {
            for item in plan {
                try prepareSafeDestination(item.finalDest, modelsRoot: modelsRoot)
                if FileManager.default.fileExists(atPath: item.finalDest.path) {
                    let relative = item.finalDest.path.dropFirst(modelsRoot.path.count + 1)
                    let backup = backupRoot.appendingPathComponent(String(relative))
                    try FileManager.default.createDirectory(
                        at: backup.deletingLastPathComponent(), withIntermediateDirectories: true)
                    try moveItem(item.finalDest, backup)
                    backedUp.append((backup, item.finalDest))
                }
            }
            for item in plan {
                try prepareSafeDestination(item.finalDest, modelsRoot: modelsRoot)
                try moveItem(item.staged, item.finalDest)
                promoted.append(item)
            }
            try removeItem(backupRoot)
        } catch {
            var rollbackErrors: [String] = []
            for item in promoted.reversed() {
                do { try moveItem(item.finalDest, item.staged) }
                catch { rollbackErrors.append(error.localizedDescription) }
            }
            for item in backedUp.reversed() {
                do { try moveItem(item.backup, item.finalDest) }
                catch { rollbackErrors.append(error.localizedDescription) }
            }
            if rollbackErrors.isEmpty {
                try? FileManager.default.removeItem(at: backupRoot)
                throw error
            }
            throw LocalArchiveError(
                "promotion failed and rollback was incomplete; originals remain at \(backupRoot.path): \(rollbackErrors.joined(separator: "; "))")
        }
    }

    private static func prepareSafeDestination(_ destination: URL, modelsRoot: URL) throws {
        try ReadOnlyLocations.requireWritable(destination)
        let fileManager = FileManager.default
        let root = modelsRoot.standardizedFileURL
        let destination = destination.standardizedFileURL
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        guard destination.path.hasPrefix(prefix) else {
            throw LocalArchiveError("model destination escaped the models directory")
        }
        let rootAttributes = try fileManager.attributesOfItem(atPath: root.path)
        guard rootAttributes[.type] as? FileAttributeType == .typeDirectory else {
            throw LocalArchiveError("models directory is a symlink or special object")
        }

        let relative = destination.path.dropFirst(prefix.count)
        let components = relative.split(separator: "/").map(String.init)
        var current = root
        for component in components.dropLast() {
            guard component != "." && component != ".." else {
                throw LocalArchiveError("invalid model destination component")
            }
            current.appendPathComponent(component, isDirectory: true)
            if fileManager.fileExists(atPath: current.path) {
                let attributes = try fileManager.attributesOfItem(atPath: current.path)
                guard attributes[.type] as? FileAttributeType == .typeDirectory else {
                    throw LocalArchiveError("model destination parent is a symlink or special object")
                }
            } else {
                try fileManager.createDirectory(at: current, withIntermediateDirectories: false)
            }
        }
        if fileManager.fileExists(atPath: destination.path) {
            let attributes = try fileManager.attributesOfItem(atPath: destination.path)
            guard attributes[.type] as? FileAttributeType == .typeRegular else {
                throw LocalArchiveError("model destination is a symlink or special object")
            }
        }
    }

    private struct LocalArchiveError: LocalizedError {
        let message: String
        init(_ message: String) { self.message = message }
        var errorDescription: String? { message }
    }
}
