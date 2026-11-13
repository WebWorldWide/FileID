import Foundation
import FileIDShared

/// Byte-identical images share one analysis within a scan: the first copy is
/// decoded and run through Vision, RAM++, CLIP and OCR; later copies wait for
/// it and take its model output. Mirrors the shared engine's
/// `pipeline/twin_reuse.rs`, except that results are held in memory here
/// (the writer does not copy rows), so nothing is seeded from earlier scans.
struct TwinKey: Hashable, Sendable {
    let hash: Data
    let size: Int64
}

struct TwinAnalysis: Sendable {
    let tagged: TaggedFile
    /// Tags before the per-file Year and camera-family tags were appended.
    let modelTags: [String]
    /// Which models produced it; a copy made after a model loads analyzes itself.
    let models: String

    var approximateBytes: Int {
        tagged.facePrints.reduce(512) { $0 + $1.count }
            + (tagged.clipEmbeddingBlob?.count ?? 0)
            + (tagged.ocrText?.utf8.count ?? 0)
            + tagged.visionTags.reduce(0) { $0 + $1.utf8.count + 48 }
            + tagged.faceBBoxes.count * 96
    }
}

final class TwinRegistry: @unchecked Sendable {
    enum Claim {
        case owner
        case ready(TwinAnalysis)
        case pending
    }

    private enum State {
        case inFlight([UUID: CheckedContinuation<TwinAnalysis?, Never>])
        case done(TwinAnalysis, bytes: Int)
    }

    private let lock = NSLock()
    private var map: [TwinKey: State] = [:]
    private var finished: [TwinKey] = []
    private var finishedHead = 0
    private var heldBytes = 0
    private var analyzed = 0
    private var reused = 0
    private var lastUse = Date.distantPast
    private var sweepScheduled = false
    private let budgetBytes: Int
    private let logEvery: Int
    private let idleSeconds: TimeInterval
    private let timers = DispatchQueue(label: "com.fileid.v2.twin-reuse", qos: .utility)

    /// Results are dropped after `idleSeconds` without a claim, so a finished
    /// scan's results neither stay in memory nor carry into the next scan even
    /// where nothing calls `reset()`.
    init(budgetBytes: Int = 128 << 20, logEvery: Int = 500, idleSeconds: TimeInterval = 120) {
        self.budgetBytes = budgetBytes
        self.logEvery = logEvery
        self.idleSeconds = idleSeconds
    }

    private func scheduleSweepLocked() {
        guard !sweepScheduled else { return }
        sweepScheduled = true
        timers.asyncAfter(deadline: .now() + idleSeconds) { [weak self] in self?.sweepIfIdle() }
    }

    private func sweepIfIdle() {
        lock.lock()
        sweepScheduled = false
        guard Date().timeIntervalSince(lastUse) >= idleSeconds else {
            if heldBytes > 0 { scheduleSweepLocked() }
            lock.unlock()
            return
        }
        let snapshot = (analyzed, reused, heldBytes)
        for (key, state) in map {
            if case .done = state { map[key] = nil }
        }
        finished.removeAll()
        finishedHead = 0
        heldBytes = 0
        analyzed = 0
        reused = 0
        lock.unlock()
        if snapshot.1 > 0 { log(snapshot) }
    }

    var counts: (analyzed: Int, reused: Int, held: Int) {
        lock.lock(); defer { lock.unlock() }
        return (analyzed, reused, heldBytes)
    }

    func claim(_ key: TwinKey, models: String) -> Claim {
        lock.lock(); defer { lock.unlock() }
        lastUse = Date()
        switch map[key] {
        case .inFlight:
            return .pending
        case .done(let analysis, let bytes):
            if analysis.models == models { return .ready(analysis) }
            heldBytes -= bytes
            map[key] = .inFlight([:])
            return .owner
        case nil:
            map[key] = .inFlight([:])
            return .owner
        }
    }

    /// Records the owner's outcome and wakes its waiters. nil (a failed or
    /// timed-out analysis) is never kept: the next copy analyzes itself.
    func publish(_ key: TwinKey, _ analysis: TwinAnalysis?) {
        lock.lock()
        guard case .inFlight(let waiters) = map[key] else {
            lock.unlock()
            return
        }
        if let analysis {
            let bytes = analysis.approximateBytes
            map[key] = .done(analysis, bytes: bytes)
            heldBytes += bytes
            analyzed += 1
            finished.append(key)
            evictOverBudget(keeping: key)
            scheduleSweepLocked()
        } else {
            map[key] = nil
        }
        lock.unlock()
        for waiter in waiters.values { waiter.resume(returning: analysis) }
    }

    private func evictOverBudget(keeping kept: TwinKey) {
        while heldBytes > budgetBytes, finishedHead < finished.count {
            let key = finished[finishedHead]
            finishedHead += 1
            guard key != kept, case .done(_, let bytes) = map[key] else { continue }
            heldBytes -= bytes
            map[key] = nil
        }
        if finishedHead > 4096, finishedHead * 2 > finished.count {
            finished.removeFirst(finishedHead)
            finishedHead = 0
        }
    }

    /// The owner's analysis once it publishes; nil when it failed, when the
    /// wait timed out or was cancelled, or when the registry was reset. A
    /// timeout releases the key, so copies stop queueing behind a stuck owner.
    func wait(_ key: TwinKey, timeout: TimeInterval) async -> TwinAnalysis? {
        let id = UUID()
        let giveUp = DispatchWorkItem { [weak self] in self?.release(key, ifWaiting: id) }
        let result = await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<TwinAnalysis?, Never>) in
                lock.lock()
                switch map[key] {
                case .inFlight(var waiters):
                    if Task.isCancelled {
                        lock.unlock()
                        continuation.resume(returning: nil)
                        return
                    }
                    waiters[id] = continuation
                    map[key] = .inFlight(waiters)
                    lock.unlock()
                    timers.asyncAfter(deadline: .now() + timeout, execute: giveUp)
                case .done(let analysis, _):
                    lock.unlock()
                    continuation.resume(returning: analysis)
                case nil:
                    lock.unlock()
                    continuation.resume(returning: nil)
                }
            }
        } onCancel: {
            self.drop(key, waiter: id)
        }
        giveUp.cancel()
        return result
    }

    private func drop(_ key: TwinKey, waiter id: UUID) {
        lock.lock()
        guard case .inFlight(var waiters) = map[key], let continuation = waiters.removeValue(forKey: id) else {
            lock.unlock()
            return
        }
        map[key] = .inFlight(waiters)
        lock.unlock()
        continuation.resume(returning: nil)
    }

    private func release(_ key: TwinKey, ifWaiting id: UUID) {
        lock.lock()
        guard case .inFlight(let waiters) = map[key], waiters[id] != nil else {
            lock.unlock()
            return
        }
        map[key] = nil
        lock.unlock()
        for waiter in waiters.values { waiter.resume(returning: nil) }
    }

    func noteReuse() {
        lock.lock()
        reused += 1
        let snapshot = (analyzed, reused, heldBytes)
        lock.unlock()
        if snapshot.1 % logEvery == 0 { log(snapshot) }
    }

    /// Forgets everything; call when a scan starts and when it ends.
    func reset() {
        lock.lock()
        let snapshot = (analyzed, reused, heldBytes)
        var waiting: [CheckedContinuation<TwinAnalysis?, Never>] = []
        for state in map.values {
            if case .inFlight(let waiters) = state { waiting.append(contentsOf: waiters.values) }
        }
        map.removeAll()
        finished.removeAll()
        finishedHead = 0
        heldBytes = 0
        analyzed = 0
        reused = 0
        lock.unlock()
        for waiter in waiting { waiter.resume(returning: nil) }
        if snapshot.1 > 0 { log(snapshot) }
    }

    private func log(_ snapshot: (Int, Int, Int)) {
        JSONLog.shared.info(ev: "twin_reuse", extra: [
            "analyzed": AnyCodable(snapshot.0),
            "reused": AnyCodable(snapshot.1),
            "held_mb": AnyCodable(snapshot.2 >> 20)
        ])
    }
}

enum TwinReuse {
    static let shared = TwinRegistry()

    /// `FILEID_TWIN_REUSE=0` turns reuse off (same switch as the shared engine).
    static let enabled: Bool = {
        guard let raw = ProcessInfo.processInfo.environment["FILEID_TWIN_REUSE"]?
            .trimmingCharacters(in: .whitespaces).lowercased() else { return true }
        return !["0", "false", "off", "no"].contains(raw)
    }()

    static let minBytes: Int64 = 4096

    /// Longer than a file's Vision passes (primary, fast and accurate text) can take.
    static var waitSeconds: TimeInterval { max(60, visionPerformTimeoutSeconds * 6) }

    /// Only files whose stored hash is the full-file SHA-256 take part.
    static func eligible(sizeBytes: Int64) -> Bool {
        sizeBytes >= minBytes && UInt64(sizeBytes) <= ContentHash.fullHashMaxBytes
    }

    static func reset() { shared.reset() }

    static func modelFingerprint() -> String {
        let flags = [RamPlusService.shared.isReady, MobileCLIPService.shared.canEmbed, ArcFaceService.shared.isReady]
        return flags.map { $0 ? "1" : "0" }.joined()
    }
}

extension Tagging {
    /// A copy's row: the twin's model output with this file's own path, dates,
    /// inode and the Year tag from its own date. EXIF is part of the bytes, so
    /// the twin's camera, GPS and capture time are this file's too.
    static func twinCopy(
        of twin: TwinAnalysis, for discovered: DiscoveredFile, contentHash: Data, elapsedMs: Double
    ) -> TaggedFile {
        let source = twin.tagged
        let createdAt = source.capturedFromEXIF ? source.createdAt : discovered.creationDate
        let tags = cappedTags(twin.modelTags + extraTags(cameraModel: source.cameraModel, creationDate: createdAt))
        var copy = TaggedFile(
            url: discovered.url, kind: "image", extension: discovered.url.pathExtension.lowercased(),
            sizeBytes: discovered.sizeBytes,
            createdAt: createdAt,
            modifiedAt: discovered.modificationDate,
            fileRef: discovered.fileRef,
            visionTags: tags,
            tagScores: source.tagScores,
            phash: source.phash,
            contentHash: contentHash,
            aestheticScore: source.aestheticScore,
            hasFaces: source.hasFaces,
            facePrints: source.facePrints,
            faceBBoxes: source.faceBBoxes,
            faceQualities: source.faceQualities,
            faceYaws: source.faceYaws,
            facePitches: source.facePitches,
            faceMinDimPx: source.faceMinDimPx,
            ocrText: source.ocrText,
            cameraModel: source.cameraModel,
            locationLat: source.locationLat,
            locationLon: source.locationLon,
            perFileTotalMs: elapsedMs,
            clipEmbeddingBlob: source.clipEmbeddingBlob,
            tagsEvaluated: source.tagsEvaluated,
            facesEvaluated: source.facesEvaluated,
            ocrStageRan: source.ocrStageRan
        )
        copy.ocrHandwritten = source.ocrHandwritten
        copy.capturedFromEXIF = source.capturedFromEXIF
        return copy
    }
}
