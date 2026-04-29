import Foundation
import AVFoundation
import Vision
import AppKit
import PDFKit
import CoreLocation
import SwiftData

// MARK: - Streaming File Scanner

struct DiscoveredFile: Sendable {
    let url: URL
    let creationDate: Date?
    let fileSizeBytes: Int?
}

// Discovery is single-threaded by ownership (only the scan task touches it),
// so a plain class is fine — `actor` would force an executor hop on every
// call to next(), which at 58K files compounds into seconds of pure overhead
// on top of the per-file enumerator latency.
private final class FileStream: @unchecked Sendable {
    private let enumerator: FileManager.DirectoryEnumerator
    private let validExtensions: Set<String> = FileTypes.all
    private let skipPaths: Set<String>

    init?(url: URL, skipPaths: Set<String> = []) {
        // Critical: do NOT request `.contentTypeKey` here. On network volumes
        // (TrueNAS, SMB) it triggers a UTType / Spotlight metadata lookup
        // per file that can cost 50-200 ms each — turning a 30 s discovery
        // into a 15-minute one. Creation date + file size are fetched
        // lazily inside the worker only for files we actually keep.
        guard let e = FileManager.default.enumerator(
            at: url,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles, .skipsPackageDescendants]
        ) else { return nil }
        self.enumerator = e
        self.skipPaths  = skipPaths
    }

    /// Pull a batch of up to `count` discovered files. Batching amortizes any
    /// scheduler/queue overhead across the batch and lets the discovery loop
    /// check cancellation once per batch instead of once per file (huge win
    /// on libraries with tens of thousands of files where the per-file
    /// MainActor hop was the bottleneck).
    ///
    /// Resource values (creation date, file size) are NOT fetched here —
    /// they're a per-file syscall (or network round-trip) and the FileRecord
    /// init already reads them lazily on insert. Discovery just enumerates
    /// and filters by extension.
    func nextBatch(count: Int = 1024) -> [DiscoveredFile] {
        var out: [DiscoveredFile] = []
        out.reserveCapacity(count)
        while out.count < count, let obj = enumerator.nextObject() as? URL {
            guard validExtensions.contains(obj.pathExtension.lowercased()),
                  !skipPaths.contains(obj.path) else { continue }
            out.append(DiscoveredFile(
                url: obj,
                creationDate: nil,
                fileSizeBytes: nil
            ))
        }
        return out
    }
}

// MARK: - Sendable Result

private struct FileResult: Sendable {
    let fileURL:        URL
    let tags:           [String]
    let pHashValue:     UInt64
    let cameraModel:    String?
    let locationString: String?
    let hasFaces:       Bool
    let facePrintsData: [Data]
    let aestheticScore: Double
    let clipEmbedding:  Data?
    let failed:         Bool
}

// MARK: - MediaProcessor

actor MediaProcessor {
    private let viewModel: AppViewModel
    private let store: FileIDDataStore

    private var workerCap: Int { Hardware.workerCap }

    // Accumulate prints across batches — fires the detached cluster Task only
    // when we've got enough work to amortize the actor hop and the O(N×K)
    // pass. Tail flush after the scan loop picks up any remainder, so no
    // prints are lost.
    fileprivate static let liveClusterThreshold = 2_000

    // Hard cap on the in-flight pendingFaces buffer. The "soft" threshold
    // above triggers a flush at batch-save boundaries; this hard cap forces
    // a flush mid-batch in case a face-rich photo run pushes the buffer
    // past comfort before the next save commit. At ~2 KB per print, 10 K
    // prints = ~20 MB before flushing — well under the OOM ceiling.
    fileprivate static let pendingFacesHardCap = 10_000

    // Vision/AVFoundation synchronous work runs on GCD (not the cooperative
    // pool) so a slow-I/O file can't park a cooperative thread and stall the
    // rest of the app. .userInitiated keeps it on P-cores.
    nonisolated static let visionQueue = DispatchQueue(
        label: "com.fileid.vision",
        qos: .userInitiated,
        attributes: .concurrent
    )

    init(viewModel: AppViewModel, store: FileIDDataStore) {
        self.viewModel = viewModel
        self.store     = store
    }

    // Per-file scan.log writes from 9 workers each opened a FileHandle,
    // seek-to-end, write, fsync, close — serialized at the VFS layer.
    // At ~14 files/s that's ~14 fsyncs/s under contention. Per-file lines
    // now buffer here and flush once per batch-save (inside commitBatchSave).
    // Phase-boundary / crash-forensic logs keep writing through
    // writeScanLogLine directly so they fsync immediately.
    private nonisolated static let perFileBufferLock = NSLock()
    private nonisolated(unsafe) static var perFileBuffer: [String] = []

    // Buffered; flushed on batch commit. Use for per-file ~14 Hz lines only.
    private nonisolated func appendScanLogPerFile(_ line: String) {
        Self.perFileBufferLock.lock()
        Self.perFileBuffer.append(line)
        Self.perFileBufferLock.unlock()
    }

    // Immediate write + fsync. Use for phase boundaries and anything that
    // needs to survive a crash within the next few hundred ms.
    private nonisolated func appendScanLog(_ line: String) {
        Self.writeScanLogLine(line)
    }

    // Flush the per-file buffer in one handle open+write+fsync. Called from
    // commitBatchSave (every ~400 files) and at scan end. Cheap when empty.
    nonisolated static func flushPerFileScanLog() {
        perFileBufferLock.lock()
        let pending = perFileBuffer
        perFileBuffer.removeAll(keepingCapacity: true)
        perFileBufferLock.unlock()
        guard !pending.isEmpty else { return }

        guard let dir = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)
            .first?.appendingPathComponent("Logs/FileID", isDirectory: true) else { return }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("scan.log")
        let formatter = ISO8601DateFormatter()
        let now = formatter.string(from: Date())
        var blob = Data()
        for line in pending {
            blob.append(Data("\(now) \(line)\n".utf8))
        }
        do {
            if let handle = try? FileHandle(forWritingTo: url) {
                defer { try? handle.close() }
                _ = try? handle.seekToEnd()
                try handle.write(contentsOf: blob)
                try handle.synchronize()
            } else {
                try blob.write(to: url, options: .atomic)
            }
        } catch {
            // Disk full / permission denied / volume gone — surface so the
            // user notices via Console.app instead of getting silently
            // missing scan.log lines that complicate crash forensics.
            NSLog("FileID scan.log write failed: %@", error.localizedDescription)
        }
    }

    // Nonisolated static bridge so non-MediaProcessor callers
    // (ClusterCircuitBreaker etc.) can append to scan.log without
    // needing an instance. Phase-boundary / crash-forensic lines — writes
    // direct with fsync so a crash mid-scan doesn't lose them.
    nonisolated static func appendScanLogExternal(_ line: String) {
        writeScanLogLine(line)
    }

    // MARK: - Per-batch profiler
    //
    // The per-file scan.log line covers Vision/EXIF/CLIP — the work running
    // INSIDE the worker pool. It does NOT cover the result-loop bottleneck
    // (`store.insertScanResult`, `viewModel.recordFileCompleted`, the for-await
    // dispatch overhead itself). On the 58K TrueNAS run we measured 14 workers
    // × 140ms theoretical = ~100 files/s but observed ~14 files/s. The 86%
    // missing time is somewhere outside the per-file Vision section. This
    // profiler captures the candidates so the next user run can pinpoint it.
    private nonisolated static let profilerLock = NSLock()
    private nonisolated(unsafe) static var profilerWorkerWith:     [Double] = []
    private nonisolated(unsafe) static var profilerStoreInsert:    [Double] = []
    private nonisolated(unsafe) static var profilerResultLoopIter: [Double] = []

    private nonisolated static func profilerRecordWorkerWith(_ ms: Double) {
        profilerLock.lock(); profilerWorkerWith.append(ms); profilerLock.unlock()
    }
    private nonisolated static func profilerRecordStoreInsert(_ ms: Double) {
        profilerLock.lock(); profilerStoreInsert.append(ms); profilerLock.unlock()
    }
    private nonisolated static func profilerRecordResultLoopIter(_ ms: Double) {
        profilerLock.lock(); profilerResultLoopIter.append(ms); profilerLock.unlock()
    }

    private struct ProfilerSnapshot {
        let workerWith: [Double]
        let storeInsert: [Double]
        let resultLoopIter: [Double]

        private static func percentile(_ arr: [Double], _ p: Double) -> Double {
            guard !arr.isEmpty else { return 0 }
            let sorted = arr.sorted()
            let idx = min(sorted.count - 1, Int(Double(sorted.count) * p))
            return sorted[idx]
        }
        private static func total(_ arr: [Double]) -> Double { arr.reduce(0, +) }

        // One scan.log row per stage. `total` is the SUM across the batch —
        // for `workerWith` this exceeds wall time when the pool is saturated
        // (good); for `storeInsert` and `resultLoopIter` the sum approximately
        // equals wall time because they run in the single result-loop task.
        func formatLines(workerCap: Int, batchWallSec: Double) -> [String] {
            let workerSum = Self.total(workerWith) / 1000.0
            let utilization = batchWallSec > 0
                ? min(1.0, workerSum / (batchWallSec * Double(workerCap)))
                : 0
            return [
                String(
                    format: "  workerWith     p50=%6.1fms p95=%7.1fms total=%6.2fs (n=%d)",
                    Self.percentile(workerWith, 0.50),
                    Self.percentile(workerWith, 0.95),
                    workerSum,
                    workerWith.count
                ),
                String(
                    format: "  storeInsert    p50=%6.1fms p95=%7.1fms total=%6.2fs (n=%d)",
                    Self.percentile(storeInsert, 0.50),
                    Self.percentile(storeInsert, 0.95),
                    Self.total(storeInsert) / 1000.0,
                    storeInsert.count
                ),
                String(
                    format: "  resultLoopIter p50=%6.1fms p95=%7.1fms total=%6.2fs (n=%d)",
                    Self.percentile(resultLoopIter, 0.50),
                    Self.percentile(resultLoopIter, 0.95),
                    Self.total(resultLoopIter) / 1000.0,
                    resultLoopIter.count
                ),
                String(
                    format: "  workerWall %d × %.2fs = %.2fs   utilization=%.0f%%",
                    workerCap, batchWallSec, batchWallSec * Double(workerCap), utilization * 100
                )
            ]
        }
    }

    private nonisolated static func profilerSnapshotAndReset() -> ProfilerSnapshot {
        profilerLock.lock()
        let snap = ProfilerSnapshot(
            workerWith:     profilerWorkerWith,
            storeInsert:    profilerStoreInsert,
            resultLoopIter: profilerResultLoopIter
        )
        profilerWorkerWith.removeAll(keepingCapacity: true)
        profilerStoreInsert.removeAll(keepingCapacity: true)
        profilerResultLoopIter.removeAll(keepingCapacity: true)
        profilerLock.unlock()
        return snap
    }

    // Direct write with fsync for phase-boundary and crash-forensic logs.
    private nonisolated static func writeScanLogLine(_ line: String) {
        guard let dir = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)
            .first?.appendingPathComponent("Logs/FileID", isDirectory: true) else { return }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("scan.log")
        let stamp = ISO8601DateFormatter().string(from: Date())
        let data = Data("\(stamp) \(line)\n".utf8)
        do {
            if let handle = try? FileHandle(forWritingTo: url) {
                defer { try? handle.close() }
                _ = try? handle.seekToEnd()
                try handle.write(contentsOf: data)
                try handle.synchronize()
            } else {
                try data.write(to: url, options: .atomic)
            }
        } catch {
            NSLog("FileID scan.log write failed: %@", error.localizedDescription)
        }
    }

    // MARK: - Main Entry Point

    func startDirectoryScan(url: URL) async {
        Hardware.installMemoryPressureMonitor()

        await viewModel.log("Scan started: \(url.lastPathComponent)")

        guard let discoveryStream = FileStream(url: url) else {
            await viewModel.log("Error: Cannot enumerate directory.")
            await MainActor.run { viewModel.isProcessing = false }
            return
        }

        await MainActor.run {
            viewModel.enterPhase(.discovering)
            viewModel.currentStatus = "Discovering files…"
        }
        let discoveryStartTime = Date()
        CrashSentinel.set(phase: "discovery", subject: url.lastPathComponent)
        appendScanLog("Discovery begin: root=\(url.lastPathComponent)")

        // Drain the enumerator fully before tagging begins so the Tagging
        // progress bar has a locked-in denominator. Batched + atomic-only
        // polling: per-file MainActor awaits used to dominate Discovery time
        // on big libraries (58K × ~5 ms per hop = ~5 minutes of pure scheduling
        // on top of the actual file enumeration). Now we hop to MainActor at
        // most once per 1 024-file batch.
        let vm = viewModel
        let allFiles: [DiscoveredFile] = await Task.detached(priority: .userInitiated) {
            var collected: [DiscoveredFile] = []
            collected.reserveCapacity(8_192)
            while true {
                if vm.isCancelledAtomic { break }
                while vm.isPausedAtomic {
                    try? await Task.sleep(nanoseconds: 200_000_000)
                    if vm.isCancelledAtomic { break }
                }
                let batch = discoveryStream.nextBatch(count: 1_024)
                if batch.isEmpty { break }
                vm.bumpDiscoveredAtomic(by: batch.count)
                collected.append(contentsOf: batch)
            }
            return collected
        }.value
        let discoveryDur = Date().timeIntervalSince(discoveryStartTime)
        let discoveryRate = discoveryDur > 0 ? Double(allFiles.count) / discoveryDur : 0
        appendScanLog(String(format: "Discovery end: %d files in %.1fs (%.1f/s) — resident=%dMB",
                             allFiles.count, discoveryDur, discoveryRate, Hardware.residentMB()))
        await viewModel.log("Discovery complete: \(allFiles.count) files found.")

        if await viewModel.isCancelled {
            await MainActor.run {
                viewModel.isProcessing  = false
                viewModel.isCancelled   = false
                viewModel.isPaused      = false
                viewModel.currentStatus = "Cancelled"
                viewModel.enterPhase(.idle)
            }
            return
        }
        if allFiles.isEmpty {
            await viewModel.log("No taggable files found.")
            await MainActor.run {
                viewModel.isProcessing  = false
                viewModel.currentStatus = "No files to tag"
                viewModel.enterPhase(.idle)
            }
            return
        }

        let cap  = workerCap
        let pool = VisionWorkerPool(count: cap)
        await viewModel.log("Vision workers: \(cap)")

        await MainActor.run {
            viewModel.totalCount    = allFiles.count
            viewModel.currentStatus = "Tagging files…"
            viewModel.enterPhase(.tagging)
        }

        var pendingMeta: [URL: (Date?, Int?)] = [:]
        let saveEvery = Hardware.saveEvery
        var processedTotal = 0
        var resultBatch    = 0
        let taggingStart   = Date()
        var batchStart     = Date()
        var pendingFaces: [(UUID, URL, Data)] = []
        CrashSentinel.set(phase: "vision", subject: url.lastPathComponent, batch: 0)
        appendScanLog("Tagging begin: saveEvery=\(saveEvery) total=\(allFiles.count)")

        // `vm` was already captured above for the Discovery loop — reuse it.
        // The result loop polls vm.isCancelledAtomic / vm.isPausedAtomic for
        // the same reason: per-file MainActor hops were stalling worker
        // dispatch and the Mac's CPU history showed P-cores at 30-50%
        // utilization despite 9 active workers.

        await withTaskGroup(of: FileResult.self) { group in
            var nextIndex = 0
            // cap*2 gives every worker a queued replacement while it runs —
            // enough cushion to absorb a result-loop stall without
            // over-seeding (cap*4 was tried in Batch 16 but the larger
            // queue depth amplified the CLIP-load startup cascade in
            // Batch 17 by piling 56 tasks all racing for ANE at once).
            let seedCap   = min(cap * 2, allFiles.count)

            while nextIndex < seedCap {
                if vm.isCancelledAtomic { break }
                let item = allFiles[nextIndex]
                pendingMeta[item.url] = (item.creationDate, item.fileSizeBytes)
                let u = item.url
                group.addTask {
                    let t0 = CFAbsoluteTimeGetCurrent()
                    let result = await pool.with { worker in
                        await self.processFile(fileURL: u, worker: worker)
                    }
                    Self.profilerRecordWorkerWith((CFAbsoluteTimeGetCurrent() - t0) * 1000)
                    return result
                }
                nextIndex += 1
            }

            // Pause-poll cadence: only check every 64 files instead of every
            // file, and only once the inner work is done. Saves ~58K MainActor
            // hops per scan.
            var sinceLastPauseCheck = 0

            for await result in group {
                let iterStart = CFAbsoluteTimeGetCurrent()
                if vm.isCancelledAtomic {
                    group.cancelAll()
                    appendScanLog(String(format: "Cancellation received: processedTotal=%d",
                                         processedTotal))
                    break
                }
                let meta = pendingMeta.removeValue(forKey: result.fileURL)
                let insertStart = CFAbsoluteTimeGetCurrent()
                let insertedID = await store.insertScanResult(
                    fileURL:        result.fileURL,
                    creationDate:   meta?.0,
                    fileSizeBytes:  meta?.1,
                    tags:           result.tags,
                    cameraModel:    result.cameraModel,
                    locationString: result.locationString,
                    hasFaces:       result.hasFaces,
                    pHashValue:     result.pHashValue,
                    aestheticScore: result.aestheticScore,
                    clipEmbedding:  result.clipEmbedding,
                    failed:         result.failed,
                    facePrintsData: result.facePrintsData
                )
                Self.profilerRecordStoreInsert((CFAbsoluteTimeGetCurrent() - insertStart) * 1000)

                viewModel.recordFileCompleted(fileURL: result.fileURL)

                for fp in result.facePrintsData {
                    pendingFaces.append((insertedID, result.fileURL, fp))
                }

                // Hard-cap guard: a face-dense photo run can push pendingFaces
                // past the live-cluster soft threshold before the next batch
                // save. Force a flush so the in-flight buffer stays bounded.
                if pendingFaces.count >= Self.pendingFacesHardCap {
                    flushFacesIfReady(&pendingFaces, force: true)
                }

                processedTotal += 1
                resultBatch    += 1

                if resultBatch >= saveEvery {
                    await commitBatchSave(
                        batchSize: resultBatch,
                        batchStart: batchStart,
                        processedTotal: processedTotal
                    )
                    resultBatch = 0
                    batchStart  = Date()
                    flushFacesIfReady(&pendingFaces)
                }

                if nextIndex < allFiles.count {
                    let item = allFiles[nextIndex]
                    pendingMeta[item.url] = (item.creationDate, item.fileSizeBytes)
                    let u = item.url
                    group.addTask {
                        let t0 = CFAbsoluteTimeGetCurrent()
                        let result = await pool.with { worker in
                            await self.processFile(fileURL: u, worker: worker)
                        }
                        Self.profilerRecordWorkerWith((CFAbsoluteTimeGetCurrent() - t0) * 1000)
                        return result
                    }
                    nextIndex += 1
                }

                sinceLastPauseCheck += 1
                if sinceLastPauseCheck >= 64 {
                    sinceLastPauseCheck = 0
                    while vm.isPausedAtomic {
                        try? await Task.sleep(nanoseconds: 200_000_000)
                        if vm.isCancelledAtomic { break }
                    }
                }
                Self.profilerRecordResultLoopIter((CFAbsoluteTimeGetCurrent() - iterStart) * 1000)
            }

            await store.save()
            // Final flush of the per-file scan.log buffer — catches any
            // tail lines that landed after the last batch-save commit.
            Self.flushPerFileScanLog()
            let finalProcessed = processedTotal
            await MainActor.run {
                viewModel.processedCount  = finalProcessed
                viewModel.scanBatchCount += 1
            }
            await flushPendingFaces(&pendingFaces)
        }

        let taggingDur = Date().timeIntervalSince(taggingStart)
        let taggingRate = taggingDur > 0 ? Double(processedTotal) / taggingDur : 0
        let wasCancelled = await viewModel.isCancelled
        let verb = wasCancelled ? "tagging cancelled" : "tagging total"
        NSLog("FileID %@: %d of %d files in %.1fs (%.1f/s avg)",
              verb, processedTotal, allFiles.count, taggingDur, taggingRate)
        appendScanLog(String(format: "%@: %d of %d files in %.1fs (%.1f/s avg) — resident=%dMB",
                             verb, processedTotal, allFiles.count, taggingDur, taggingRate,
                             Hardware.residentMB()))
        await viewModel.log("\(wasCancelled ? "Scan cancelled" : "Tagging complete"): \(processedTotal) files processed.")

        if wasCancelled {
            await MainActor.run {
                viewModel.isProcessing = false
                viewModel.isCancelled  = false
                viewModel.isPaused     = false
                viewModel.currentStatus = "Cancelled"
                viewModel.enterPhase(.idle)
            }
            return
        }

        // Each post-scan phase runs in its own crash boundary. If clustering
        // crashes (the 2026-04-24 OOM mode), the user still gets naming +
        // junk scoring. The runScan() caller continues to finishNamingPhase
        // regardless of which phase failed.
        await runFaceClusteringPassSafely()

        await MainActor.run {
            viewModel.clusteringCompletedAt = Date()
        }

        await runDeepAnalyzePassSafely()
    }

    private func runFaceClusteringPassSafely() async {
        // User escape hatch — if clustering ever misbehaves again, the user
        // can disable auto-clustering in Settings → Deep Analyze and trigger
        // it manually later. Defaults to ON so existing users see no change.
        let autoCluster = UserDefaults.standard.object(forKey: "autoClusterAfterScan") as? Bool ?? true
        guard autoCluster else {
            appendScanLog("Skipping face clustering: autoClusterAfterScan=false")
            await viewModel.log("Auto face-clustering is off. Re-enable in Settings → Deep Analyze to cluster on next scan.")
            return
        }
        // No try/catch around an async actor call can prevent SIGKILL — but
        // we CAN check memory pressure before entering and skip if the OS
        // is already telling us to back off. That alone prevents the most
        // common crash mode: a scan finishing with the OS already in
        // critical pressure, then clustering pushing past the cap.
        if Hardware.isUnderCriticalMemoryPressure {
            appendScanLog("Skipping face clustering: critical memory pressure detected")
            await viewModel.log("Skipping face clustering due to memory pressure. Re-run from Settings later.")
            return
        }
        await runFaceClusteringPass()
    }

    private func runDeepAnalyzePassSafely() async {
        if Hardware.isUnderCriticalMemoryPressure {
            appendScanLog("Skipping Deep Analyze: critical memory pressure detected")
            await viewModel.log("Skipping Deep Analyze due to memory pressure. Re-run from Settings later.")
            return
        }
        // Jetsam gate: the VLM load is a 2–3 GB MLX spike. On a 16 GB Mac
        // with a browser + IDE resident, firing this right after clustering
        // was killing the app silently (SIGKILL, no .ips dump).
        if !Hardware.canSafelyLoadLargeModel() {
            let free = Hardware.availableMemoryMB()
            appendScanLog("Skipping Deep Analyze: \(free) MB free (need ≥3000 on ≤16 GB, ≥2000 on 24 GB)")
            await viewModel.log("Skipping Deep Analyze: not enough free RAM right now. Re-run from Settings later.")
            return
        }
        await runDeepAnalyzePassIfEnabled()
    }

    // MARK: - Batch Commit

    // Saves the current batch, logs the throughput line, and re-arms the
    // CrashSentinel marker so a kill mid-next-batch points at the batch we
    // just finished rather than an older one.
    // Checkpoint cadence: every N batch saves we run PRAGMA wal_checkpoint
    // to truncate the SQLite WAL. 8 batches at saveEvery=400 = ~3 200 files.
    // At a typical 18 files/s that's roughly every 3 minutes — frequent
    // enough to keep the WAL small (< 50 MB) and infrequent enough that
    // the per-checkpoint cost (~50 ms on M1) doesn't dominate.
    private nonisolated(unsafe) static var batchesSinceCheckpoint = 0
    private static let checkpointEveryNBatches = 8

    private func commitBatchSave(
        batchSize: Int,
        batchStart: Date,
        processedTotal: Int
    ) async {
        // Flush the per-file scan.log buffer before the batch summary line
        // so the log stays chronologically consistent (per-file rows from
        // this batch appear before the "batch save done" summary).
        Self.flushPerFileScanLog()
        let saveBegin = Date()
        await store.save()
        await store.resetAfterSave()
        let saveDur    = Date().timeIntervalSince(saveBegin)
        let batchDur   = Date().timeIntervalSince(batchStart)
        let rate       = batchDur > 0 ? Double(batchSize) / batchDur : 0
        let residentMB = Hardware.residentMB()

        NSLog(
            "FileID batch: %d files in %.2fs (%.1f/s) — save took %.2fs — resident=%dMB",
            batchSize, batchDur, rate, saveDur, residentMB
        )
        appendScanLog(String(
            format: "batch: %d files in %.2fs (%.1f/s) — save took %.2fs — resident=%dMB processedTotal=%d",
            batchSize, batchDur, rate, saveDur, residentMB, processedTotal
        ))

        // PHASE-PROFILE: per-stage p50/p95/total + worker-pool utilization +
        // available memory. The per-file scan.log line measures Vision work;
        // this line measures the result-loop bottleneck (storeInsert,
        // resultLoopIter) and pool saturation (workerWith vs workerWall).
        let snap = Self.profilerSnapshotAndReset()
        let availMB = Hardware.availableMemoryMB()
        appendScanLog(String(
            format: "PHASE-PROFILE batch=%d processedTotal=%d availMB=%d residentMB=%d",
            batchSize, processedTotal, availMB, residentMB
        ))
        for line in snap.formatLines(workerCap: Hardware.workerCap, batchWallSec: batchDur) {
            appendScanLog(line)
        }

        // SQLite WAL grows with every save until something checkpoints it.
        // Without explicit checkpoints, the per-save fsync time grows with
        // total scan progress — the user-visible "incredibly long wait time
        // after running for a while." Checkpoint every N batches.
        Self.batchesSinceCheckpoint += 1
        if Self.batchesSinceCheckpoint >= Self.checkpointEveryNBatches {
            Self.batchesSinceCheckpoint = 0
            let walBefore = SQLiteCheckpoint.walSizeMB() ?? -1
            let cpBegin = Date()
            let result = SQLiteCheckpoint.truncateWAL()
            let cpDur = Date().timeIntervalSince(cpBegin)
            let walAfter = SQLiteCheckpoint.walSizeMB() ?? -1
            if let result {
                appendScanLog(String(
                    format: "WAL checkpoint: walMB %.1f→%.1f frames=%d checkpointed=%d busy=%d dur=%.2fs",
                    walBefore, walAfter, result.logFrames, result.checkpointed, result.busy, cpDur
                ))
            } else {
                appendScanLog(String(
                    format: "WAL checkpoint: skipped (busy or unavailable) walMB=%.1f dur=%.2fs",
                    walBefore, cpDur
                ))
            }
        }

        // Slow-save warning so a future user can see in scan.log if the
        // WAL is somehow still growing despite the checkpoint above.
        if saveDur > 1.5 {
            NSLog("FileID SLOW SAVE %.2fs at processedTotal=%d — investigate WAL/checkpoint",
                  saveDur, processedTotal)
        }

        CrashSentinel.set(
            phase: "vision",
            subject: "processedTotal=\(processedTotal)",
            batch: processedTotal
        )
        await MainActor.run { viewModel.scanBatchCount += 1 }
    }

    // MARK: - Face Clustering Handoff

    // Live clustering fires in-flight so PeopleView populates during the scan
    // rather than sitting empty for its full duration. Throttled across
    // batches — the O(prints × identities) pass only runs when we have
    // enough work to amortize the actor hop.
    // `force=true` bypasses the soft `liveClusterThreshold` — used by the
    // mid-batch hard-cap guard to flush a face-dense photo run before the
    // next batch-save tick.
    private func flushFacesIfReady(_ pending: inout [(UUID, URL, Data)], force: Bool = false) {
        guard force || pending.count >= Self.liveClusterThreshold else { return }
        guard !pending.isEmpty else { return }
        // Snapshot before the swap so the detached Task can't observe a
        // concurrent append. `pending.removeAll(keepingCapacity: true)`
        // empties the source array so subsequent processFile completions
        // start over — the snapshot in `handoff` is a value copy and is
        // safely owned by the Task closure.
        let handoff = pending
        pending.removeAll(keepingCapacity: true)
        Task.detached(priority: .utility) {
            let ok = await FaceClusteringService.shared.clusterBatch(prints: handoff)
            for id in ok { FacePrintCache.remove(id) }
        }
    }

    // Tail flush after the scan loop exits — catches any prints left under
    // the live-clustering threshold.
    private func flushPendingFaces(_ pending: inout [(UUID, URL, Data)]) async {
        guard !pending.isEmpty else { return }
        let tail = pending
        pending.removeAll(keepingCapacity: false)
        let ok = await FaceClusteringService.shared.clusterBatch(prints: tail)
        for id in ok { FacePrintCache.remove(id) }
    }

    // MARK: - Per-File Processor

    nonisolated private func processFile(
        fileURL: URL,
        worker: VisionWorker
    ) async -> FileResult {
        let ext      = fileURL.pathExtension.lowercased()
        let isVid    = FileTypes.videos.contains(ext)
        let isPDF    = FileTypes.pdfs.contains(ext)
        let isDoc    = FileTypes.documents.subtracting(FileTypes.pdfs).contains(ext)

        var tags:           [String] = []
        var pHashValue:     UInt64   = 0
        var cameraModel:    String?
        var locationString: String?
        var hasFaces        = false
        var facePrintsData: [Data]   = []
        var aestheticScore  = 0.5
        var clipEmbedding:  Data?

        let tFileStart = CFAbsoluteTimeGetCurrent()
        let rvSize = try? fileURL.resourceValues(forKeys: [.fileSizeKey])
        let sizeMB = Double(rvSize?.fileSize ?? 0) / 1_048_576
        // Skip enormous files — Discovery used to do this in its own loop;
        // moved here so Discovery doesn't have to stat() every URL up front
        // (huge slowdown on network volumes). 500 MB is conservative;
        // anything larger is almost always a video archive or disk image
        // where Vision tagging has nothing useful to say and the decode
        // can OOM on a 16 GB Mac.
        if sizeMB > 500 {
            appendScanLogPerFile(
                "skip large file size=\(String(format: "%.0fMB", sizeMB)) name=\(fileURL.lastPathComponent)"
            )
            return failed(fileURL)
        }
        var stageBreakdown = ""
        var kind = "image"
        var didFail = false

        if isVid {
            kind = "video"
            tags = (try? await processVideo(at: fileURL, worker: worker)) ?? ["Video"]
        } else if isPDF {
            kind = "pdf"
            tags = (try? await processPDF(at: fileURL, worker: worker)) ?? ["PDF"]
        } else if isDoc {
            kind = "doc"
            tags = processDocument(at: fileURL, ext: ext)
        } else {
            let outcome = await Self.runImagePipelineOnVisionQueue(
                fileURL: fileURL, worker: worker
            )
            stageBreakdown = outcome.timings.formatted
            if outcome.failed {
                didFail = true
            } else {
                tags             = outcome.tags
                pHashValue       = outcome.pHashValue
                cameraModel      = outcome.cameraModel
                locationString   = outcome.locationString
                hasFaces         = outcome.hasFaces
                facePrintsData   = outcome.facePrintsData
                aestheticScore   = outcome.aestheticScore
                clipEmbedding    = outcome.clipEmbedding
            }
        }

        let totalMs = (CFAbsoluteTimeGetCurrent() - tFileStart) * 1000
        let logLine: String = {
            var s = String(format: "file type=%@ ext=%@ size=%.2fMB total=%.0fms",
                           kind, ext, sizeMB, totalMs)
            if !stageBreakdown.isEmpty { s += " " + stageBreakdown }
            if didFail                 { s += " failed=1" }
            s += " name=" + fileURL.lastPathComponent
            return s
        }()
        appendScanLogPerFile(logLine)

        if didFail { return failed(fileURL) }

        if let rv   = try? fileURL.resourceValues(forKeys: [.creationDateKey]),
           let date = rv.creationDate {
            let f = DateFormatter(); f.dateFormat = "yyyy_MM"
            tags.append(f.string(from: date))
        }

        // Rewrite Vision taxonomy jargon ("Optical_Equipment" → "Glasses")
        // while leaving internal tag contracts (Tax_Document, Invoice, date
        // tags) untouched. Humanize also dedups, so the Set round-trip above
        // is redundant — kept out for clarity at the boundary.
        tags = TagTaxonomy.humanize(tags)
        return FileResult(
            fileURL: fileURL, tags: tags, pHashValue: pHashValue,
            cameraModel: cameraModel, locationString: locationString,
            hasFaces: hasFaces, facePrintsData: facePrintsData, aestheticScore: aestheticScore,
            clipEmbedding: clipEmbedding,
            failed: false
        )
    }

    nonisolated private func failed(_ fileURL: URL) -> FileResult {
        FileResult(fileURL: fileURL, tags: [], pHashValue: 0, cameraModel: nil,
                   locationString: nil, hasFaces: false, facePrintsData: [], aestheticScore: 0.0,
                   clipEmbedding: nil, failed: true)
    }

    // MARK: - Deep Analyze

    func runDeepAnalyzePassIfEnabled() async {
        let defaults = UserDefaults.standard
        let enabled = defaults.object(forKey: "deepAnalyzeEnabled") as? Bool
            ?? Hardware.deepAnalyzeAutoDefaultOn
        guard enabled else { return }
        guard AIModelKind.qwen2VL2B.descriptor.isInstalled else { return }

        let fullSweep = defaults.bool(forKey: "deepAnalyzeFullSweep")
        let total = await store.deepAnalyzeTargetCount(fullSweep: fullSweep)
        guard total > 0 else { return }

        Hardware.installMemoryPressureMonitor()

        await MainActor.run {
            viewModel.enterPhase(.scoring)
            viewModel.currentStatus = "Deep Analyze (\(total) files)…"
            viewModel.scoringDone   = 0
            viewModel.scoringTotal  = total
        }

        // Stream in chunks: fetch small batches with `deepAnalysis == nil`,
        // drain autorelease/CG/MLX scratch between files, let the actor pump.
        // The predicate shrinks as we go, so offset-0 each loop is correct and
        // the pass is trivially resumable after force-quit.
        //
        // Chunk size + inter-chunk sleep are user-tunable via the
        // "Deep Analyze intensity" setting. Gentle additionally waits for a
        // safe memory window before each chunk (and skips if MLX can't load).
        let throttle = defaults.string(forKey: "deepAnalyzeThrottle") ?? "balanced"
        let chunk: Int
        let interChunkSleepMs: Int
        switch throttle {
        case "performance": chunk = 64; interChunkSleepMs = 50
        case "gentle":      chunk = 16; interChunkSleepMs = 1000
        default:            chunk = 32; interChunkSleepMs = 250   // "balanced"
        }
        var done = 0
        while !Task.isCancelled {
            if throttle == "gentle" && !Hardware.canSafelyLoadLargeModel() {
                await viewModel.log("Deep Analyze (gentle): waiting for safe memory window…")
                try? await Task.sleep(for: .seconds(5))
                continue
            }

            let batch = await store.deepAnalyzeTargetIDs(fullSweep: fullSweep, limit: chunk)
            if batch.isEmpty { break }

            for target in batch {
                if Task.isCancelled { break }
                let caption = await DeepAnalyzeService.shared.analyze(imageURL: target.url)
                await store.setDeepAnalysis(recordID: target.id, text: caption)
                done += 1
                if done % 5 == 0 {
                    let d = done, t = total
                    await MainActor.run {
                        viewModel.currentStatus = "Deep Analyze (\(d) / \(t))…"
                        viewModel.scoringDone = d
                    }
                }
                await Task.yield()
            }

            await DeepAnalyzeService.shared.trimCaches()
            try? await Task.sleep(for: .milliseconds(interChunkSleepMs))
            if Hardware.isUnderMemoryPressure {
                await viewModel.log("Deep Analyze backing off: memory pressure")
                try? await Task.sleep(for: .milliseconds(500))
            }
        }

        let dFinal = done, tFinal = total
        await MainActor.run {
            viewModel.currentStatus = "Deep Analyze (\(dFinal) / \(tFinal))"
            viewModel.scoringDone = dFinal
        }

        await DeepAnalyzeService.shared.unload()
        await viewModel.log("Deep Analyze complete: \(done) files.")
    }

    // MARK: - Image pipeline

    private struct ImagePipelineOutcome: Sendable {
        var failed: Bool = false
        var tags: [String] = []
        var pHashValue: UInt64 = 0
        var cameraModel: String?
        var locationString: String?
        var hasFaces: Bool = false
        var facePrintsData: [Data] = []
        var aestheticScore: Double = 0.5
        var clipEmbedding: Data?
        var timings = PipelineTimings()
    }

    struct PipelineTimings: Sendable {
        var loadMs:      Double = 0
        var classifyMs:  Double = 0
        var ocrMs:       Double? = nil
        var hashMs:      Double = 0
        var aestheticMs: Double = 0
        var facesMs:     Double = 0
        var exifMs:      Double = 0
        var clipMs:      Double? = nil

        var formatted: String {
            var parts: [String] = [
                String(format: "load=%.0f", loadMs),
                String(format: "classify=%.0f", classifyMs),
            ]
            if let o = ocrMs  { parts.append(String(format: "ocr=%.0f", o)) }
            parts.append(String(format: "hash=%.0f",      hashMs))
            parts.append(String(format: "aesthetic=%.0f", aestheticMs))
            parts.append(String(format: "faces=%.0f",     facesMs))
            parts.append(String(format: "exif=%.0f",      exifMs))
            if let c = clipMs { parts.append(String(format: "clip=%.0f", c)) }
            return parts.joined(separator: " ")
        }
    }

    nonisolated private static func runImagePipelineOnVisionQueue(
        fileURL: URL,
        worker: VisionWorker
    ) async -> ImagePipelineOutcome {
        await withCheckedContinuation { (cont: CheckedContinuation<ImagePipelineOutcome, Never>) in
            visionQueue.async {
                var out = ImagePipelineOutcome()
                autoreleasepool {
                    let t0 = CFAbsoluteTimeGetCurrent()
                    guard let cgImage = VisionProcessor.shared.loadImage(from: fileURL) else {
                        out.failed = true; return
                    }
                    let t1 = CFAbsoluteTimeGetCurrent()
                    out.timings.loadMs = (t1 - t0) * 1000

                    // Bundled: classify + animals + face rects + per-face
                    // feature prints all share a single VNImageRequestHandler.
                    let pass = worker.runPrimaryPass(cgImage)
                    let t2 = CFAbsoluteTimeGetCurrent()
                    out.timings.classifyMs = (t2 - t1) * 1000

                    var tags = pass.legacyTagStrings()

                    if tags.contains(where: { ["Document","Screenshot","Receipt","Text","Presentation"].contains($0) }) {
                        let ocrStart = CFAbsoluteTimeGetCurrent()
                        let text = worker.ocrText(cgImage)
                        tags += TextTagger.tagsFromText(text)
                        out.timings.ocrMs = (CFAbsoluteTimeGetCurrent() - ocrStart) * 1000
                    }

                    let aStart = CFAbsoluteTimeGetCurrent()
                    let rv     = try? fileURL.resourceValues(forKeys: [.fileSizeKey])
                    let sizeMB = Double(rv?.fileSize ?? 0) / 1_048_576
                    out.aestheticScore = lightweightAestheticStatic(cgImage: cgImage, fileSizeMB: sizeMB)
                    let aEnd = CFAbsoluteTimeGetCurrent()
                    out.timings.aestheticMs = (aEnd - aStart) * 1000

                    out.pHashValue = computeDHashStatic(cgImage)
                    let hEnd = CFAbsoluteTimeGetCurrent()
                    out.timings.hashMs = (hEnd - aEnd) * 1000

                    if !pass.facePrints.isEmpty {
                        out.hasFaces = true
                        for (fp, _) in pass.facePrints {
                            if let d = try? NSKeyedArchiver.archivedData(withRootObject: fp, requiringSecureCoding: true) {
                                out.facePrintsData.append(d)
                            }
                        }
                    }
                    let fEnd = CFAbsoluteTimeGetCurrent()
                    // Face prints are bundled into the primary pass; this only
                    // measures NSKeyedArchiver serialization time now.
                    out.timings.facesMs = (fEnd - hEnd) * 1000

                    let exif = VisionProcessor.shared.readEXIF(from: fileURL)
                    out.cameraModel = exif.cameraModel
                    if let lat = exif.latitude, let lon = exif.longitude {
                        let finalLat = (exif.latRef ?? "N") == "S" ? -lat : lat
                        let finalLon = (exif.lonRef ?? "W") == "W" ? -lon : lon
                        out.locationString = String(format: "%.5f, %.5f", finalLat, finalLon)
                    }
                    let eEnd = CFAbsoluteTimeGetCurrent()
                    out.timings.exifMs = (eEnd - fEnd) * 1000

                    // CLIP image encoding runs ~100–200 ms per file on an
                    // already-decoded 256 px buffer. Only photos benefit from
                    // semantic embeddings — for PDFs, Office docs, etc. the
                    // file-extension + OCR text is the signal. Gating saves
                    // that overhead on every non-photo file in the scan.
                    let ext = fileURL.pathExtension.lowercased()
                    let isImage = FileTypes.images.contains(ext)
                    if isImage, let vec = MobileCLIPService.shared.embed(cgImage) {
                        out.clipEmbedding = vec.withUnsafeBufferPointer { Data(buffer: $0) }
                        // 0.28 (was 0.22) — below this the CLIP cosine match is
                        // noisy enough that Vision's label is almost always
                        // better; keeping only high-confidence CLIP tags.
                        // Reuse `vec` so the image encoder runs ONCE per file
                        // (was running twice — embed() above + a hidden second
                        // embed inside classify(cgImage:)).
                        let clipLabels = MobileCLIPService.shared.classify(usingEmbedding: vec, topK: 5)
                        for (label, score) in clipLabels where score > 0.28 {
                            let simplified = label
                                .replacingOccurrences(of: "a photo of ", with: "")
                                .replacingOccurrences(of: "a ", with: "")
                                .replacingOccurrences(of: "an ", with: "")
                                .capitalized
                            if simplified.count >= 3 { tags.append(simplified) }
                        }
                        out.timings.clipMs = (CFAbsoluteTimeGetCurrent() - eEnd) * 1000
                    }

                    // Drop generic/low-information tags; Vision occasionally
                    // slips them through above 0.50 even after the worker filter.
                    let generic: Set<String> = ["Outdoor","Indoor","Object","Item","Thing","Other","Background","Image","Photo"]
                    tags.removeAll { generic.contains($0) }
                    out.tags = Array(NSOrderedSet(array: tags)) as? [String] ?? tags
                }
                cont.resume(returning: out)
            }
        }
    }

    // MARK: - Static helpers

    nonisolated static func lightweightAestheticStatic(cgImage: CGImage, fileSizeMB: Double) -> Double {
        let mp        = Double(cgImage.width * cgImage.height) / 1_000_000
        let sizeScore = min(fileSizeMB / 5.0, 1.0)
        let resScore  = min(mp / 12.0,        1.0)
        return min(1.0, sizeScore * 0.5 + resScore * 0.5)
    }

    nonisolated static func computeDHashStatic(_ cgImage: CGImage) -> UInt64 {
        // Defensive: a 0-width/0-height CGImage from a corrupt source would
        // make `ctx.draw` a no-op (pixels stay zero, hash = 0 — correctly
        // treated as "no pHash" by the dedup index). We don't crash here, but
        // explicitly returning 0 keeps the contract clear.
        guard cgImage.width > 0, cgImage.height > 0 else { return 0 }
        let w = 9, h = 8
        var pixels = [UInt8](repeating: 0, count: w * h)
        let gray = CGColorSpaceCreateDeviceGray()
        guard let ctx = CGContext(data: &pixels, width: w, height: h, bitsPerComponent: 8,
                                  bytesPerRow: w, space: gray,
                                  bitmapInfo: CGImageAlphaInfo.none.rawValue) else { return 0 }
        ctx.draw(cgImage, in: CGRect(x: 0, y: 0, width: CGFloat(w), height: CGFloat(h)))
        var hash: UInt64 = 0
        for row in 0..<h {
            for col in 0..<(w - 1) {
                if pixels[row * w + col] > pixels[row * w + col + 1] {
                    hash |= (UInt64(1) << UInt64(row * 8 + col))
                }
            }
        }
        return hash
    }

    // Single mid-point frame extracted via the modern async API. Replaces
    // the deprecated `AVAsset(url:)` + `copyCGImage(at:actualTime:)` pair.
    nonisolated private func processVideo(at url: URL, worker: VisionWorker) async throws -> [String] {
        let asset = AVURLAsset(url: url)
        let secs: Double
        do {
            secs = try await asset.load(.duration).seconds
        } catch {
            return ["Video"]
        }

        let gen = AVAssetImageGenerator(asset: asset)
        gen.appliesPreferredTrackTransform = true
        gen.maximumSize = CGSize(width: 512, height: 512)
        let midT = CMTimeMakeWithSeconds(max(0.1, secs * 0.5), preferredTimescale: 600)

        let cgImage: CGImage?
        do {
            cgImage = try await gen.image(at: midT).image
        } catch {
            return ["Video"]
        }
        guard let cgImage else { return ["Video"] }

        var tags = worker.classify(cgImage)
        tags.append("Video")
        return tags
    }

    // MARK: - PDF

    nonisolated private func processPDF(at url: URL, worker: VisionWorker) async throws -> [String] {
        // Skip OCR on large PDFs — usually scanned manuals or archives where
        // 10 × accurate-OCR = 30+ s of Vision-worker time per file. Filename
        // + "Large_Document" tag is enough for cleanup / restructure to work.
        let fileSize = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        if fileSize > 20 * 1_048_576 {
            return ["PDF", "Large_Document"]
        }

        guard let doc = PDFDocument(url: url) else { return ["PDF"] }
        // First 3 pages carry the genre-defining vocabulary (title + intro).
        // 10 was the old default; combined with .accurate OCR it cost 28-38 s
        // per PDF in the 2026-04-24 TrueNAS run and stalled the whole pipeline.
        let pageCount = min(doc.pageCount, 3)
        var allText   = ""

        for pageIndex in 0..<pageCount {
            guard let page = doc.page(at: pageIndex) else { continue }
            let rect  = page.bounds(for: .mediaBox)
            guard rect.width > 0, rect.height > 0 else { continue }

            let scale = min(1.0, 1024.0 / max(rect.width, rect.height))
            let w = Int(rect.width * scale), h = Int(rect.height * scale)
            guard let ctx = CGContext(
                data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
            ) else { continue }
            ctx.scaleBy(x: scale, y: scale)
            ctx.setFillColor(CGColor(gray: 1, alpha: 1))
            ctx.fill(CGRect(origin: .zero, size: rect.size))
            page.draw(with: .mediaBox, to: ctx)
            guard let cg = ctx.makeImage() else { continue }

            let pageText = worker.ocrFast(cg)
            allText += pageText + " "
            if allText.count > 10_000 { break }
        }

        var tags = TextTagger.tagsFromText(allText)
        tags.append("PDF")
        return tags
    }

    // MARK: - Documents (Office / OpenDocument / iWork / RTF / plain text)

    nonisolated private func processDocument(at url: URL, ext: String) -> [String] {
        let text = OfficeDocReader.extractText(from: url)
        var tags = TextTagger.tagsFromText(text)
        if FileTypes.spreadsheet.contains(ext)      { tags.append("Spreadsheet") }
        else if FileTypes.presentation.contains(ext){ tags.append("Presentation") }
        else if FileTypes.word.contains(ext)        { tags.append("Document") }
        else if FileTypes.richText.contains(ext)    { tags.append("Document") }
        else if FileTypes.plainText.contains(ext)   { tags.append("Text") }
        return tags
    }

    // MARK: - Geocoding

    nonisolated private func reverseGeocode(
        lat: Double, lon: Double, latRef: String?, lonRef: String?
    ) async -> String? {
        let finalLat = (latRef ?? "N") == "S" ? -lat : lat
        let finalLon = (lonRef ?? "W") == "W" ? -lon : lon
        let location = CLLocation(latitude: finalLat, longitude: finalLon)
        guard let place = try? await CLGeocoder().reverseGeocodeLocation(location).first else { return nil }
        var parts: [String] = []
        if let city  = place.locality           { parts.append(city) }
        if let state = place.administrativeArea { parts.append(state) }
        return parts.isEmpty ? nil : parts.joined(separator: ", ")
    }

    // MARK: - Face clustering pass

    // Streams FileRecord IDs in chunks of 1 000 so we never hold all 58 K
    // records hot — the previous version fetched the entire table (~580 MB
    // resident at 58 K rows × ~10 KB/row) and built one giant allPrints
    // array (~200 MB at ~10 faces/file × 2 KB/print). Combined with the
    // thumbnail cache and Vision worker pool that pushed peak resident over
    // 2 GB on a 16 GB Mac, contributing to memory-pressure terminations.
    //
    // Also fixed: prints are now removed from FacePrintCache only AFTER
    // clusterBatch returns successfully. The previous code removed before
    // clustering, so a crash mid-pass permanently lost face prints.
    func runFaceClusteringPass() async {
        await MainActor.run {
            viewModel.enterPhase(.clustering)
            viewModel.currentStatus = "Clustering faces…"
        }

        struct Pending: Sendable { let fileURL: URL; let id: UUID }
        // 250 files × ~10 prints/file = ~2 500 prints per clusterBatch call.
        // At ~2 KB serialised + ~2 KB CFData + ~2 KB Float vector per print,
        // that's ~15 MB of transient allocations per chunk — well within
        // autoreleasepool drain capacity. Was 1 000 (~60 MB transient) which
        // could trip memory-pressure on libraries with many faces per file.
        let chunkSize = 250

        // Pre-filter on hasFaces — most files have no detected faces and don't
        // need a FacePrintCache.load call. On a 58 K-file library only ~30 %
        // typically have faces, so we save ~40 K disk pokes.
        let totalCount: Int = await store.perform { ctx in
            var d = FetchDescriptor<FileRecord>(predicate: #Predicate { $0.hasFaces == true })
            d.fetchLimit = nil
            return (try? ctx.fetchCount(d)) ?? 0
        }

        await MainActor.run {
            viewModel.clusteringFacesTotal = totalCount
            viewModel.clusteringFacesDone  = 0
        }

        // Pre-load the breaker's skip-list once per pass. Filter it against
        // each chunk so files that crashed 3+ times on prior runs never
        // reach clusterBatch again.
        let skipList = await ClusterCircuitBreaker.shared.skipList()
        if !skipList.isEmpty {
            appendScanLog("Face clustering: skipping \(skipList.count) known-bad face prints from prior crashes")
            await viewModel.log("Face clustering: skipping \(skipList.count) known-bad face prints from prior crashes.")
        }

        // Diagnostic: what did the live-during-scan pass already produce,
        // and what threshold is active? Logged to scan.log (durable) so the
        // evidence survives a crash. User reported "only 3 identities" on a
        // 58 K library — if that pattern recurs, this log line is the
        // signal that the threshold needs tuning OR clustering never ran.
        let startStats = await FaceClusteringService.shared.clusterStats()
        appendScanLog(String(format: "Face clustering begin: identities=%d threshold=%.2f hasFacesFiles=%d",
                             startStats.identityCount, startStats.distanceThreshold, totalCount))

        var offset = 0
        var processedFiles = 0
        var totalPrints    = 0
        while true {
            if Task.isCancelled { break }
            let currentOffset = offset
            let chunkRaw: [Pending] = await store.perform { ctx in
                var d = FetchDescriptor<FileRecord>(
                    predicate: #Predicate { $0.hasFaces == true },
                    sortBy: [SortDescriptor(\.id)]
                )
                d.fetchLimit  = chunkSize
                d.fetchOffset = currentOffset
                let files = (try? ctx.fetch(d)) ?? []
                return files.map { Pending(fileURL: $0.url, id: $0.id) }
            }
            if chunkRaw.isEmpty { break }

            // Drop blacklisted files BEFORE loading their prints. Their
            // FacePrintCache entries were already deleted by the breaker's
            // recoverFromCrash call at launch, so load() would return [].
            let chunk = chunkRaw.filter { !skipList.contains($0.id) }

            // Hard cap on prints-per-clusterBatch call: even if a single
            // chunk of 250 files contains group photos with 100+ faces each,
            // we never hand more than `maxPrintsPerBatch` to clusterBatch in
            // one go. Drains the pending buffer between sub-batches so
            // peak transient memory stays bounded.
            //
            // FacePrintCache.load returns synchronous Data — wrap each load
            // in autoreleasepool so the underlying NSData backing buffers
            // don't accumulate across the chunk.
            let maxPrintsPerBatch = 500
            var pendingPrints: [(UUID, URL, Data)] = []
            pendingPrints.reserveCapacity(maxPrintsPerBatch)
            // Track which files we handed to clusterBatch so we can remove
            // only the successfully-processed ones from FacePrintCache.
            // Breaks the stale-print feedback loop: if a mid-batch crash
            // kills the app, the files that DID cluster get their prints
            // cleaned up; the crasher's prints are handled by the breaker's
            // recoverFromCrash path on next launch.
            for p in chunk {
                let raw: [Data] = autoreleasepool { FacePrintCache.load(p.id) }
                for d in raw {
                    pendingPrints.append((p.id, p.fileURL, d))
                    if pendingPrints.count >= maxPrintsPerBatch {
                        let batch = pendingPrints
                        pendingPrints.removeAll(keepingCapacity: true)
                        let ok = await FaceClusteringService.shared.clusterBatch(prints: batch)
                        totalPrints += batch.count
                        for id in ok { FacePrintCache.remove(id) }
                        if Hardware.isUnderCriticalMemoryPressure {
                            appendScanLog("Face clustering: critical memory pressure, deferring remaining prints in this chunk")
                            // Give the OS a moment to recover.
                            try? await Task.sleep(for: .seconds(1))
                        }
                    }
                }
            }
            if !pendingPrints.isEmpty {
                let batch = pendingPrints
                let ok = await FaceClusteringService.shared.clusterBatch(prints: batch)
                totalPrints += batch.count
                for id in ok { FacePrintCache.remove(id) }
            }
            // Also clean up any chunk files that had zero prints to begin
            // with — they don't go through clusterBatch but their empty
            // cache files should still be garbage-collected.
            for p in chunk { FacePrintCache.remove(p.id) }

            processedFiles += chunk.count
            offset += chunkRaw.count  // advance by raw count so blacklisted files don't stall the offset
            let pf = processedFiles
            let pp = totalPrints
            let tc = totalCount
            await MainActor.run {
                viewModel.currentStatus       = "Clustering faces (\(pf) / \(tc) files · \(pp) prints)…"
                viewModel.clusteringFacesDone = pf
            }

            // Brief yield so the main actor isn't starved of UI updates and
            // SwiftData commit notifications get a chance to drain.
            await Task.yield()
            if Hardware.isUnderMemoryPressure {
                await viewModel.log("Face clustering: memory pressure detected, backing off")
                try? await Task.sleep(for: .milliseconds(500))
            }
        }

        await MainActor.run {
            viewModel.clusteringCompletedAt = Date()
            viewModel.clusteringFacesDone   = processedFiles
        }
        // Completion diagnostics — logged to scan.log AND viewModel so the
        // evidence survives even if the post-scan naming phase crashes.
        let endStats = await FaceClusteringService.shared.clusterStats()
        appendScanLog(String(format: "Face clustering complete: files=%d prints=%d identities=%d (delta=%d) — resident=%dMB",
                             processedFiles, totalPrints, endStats.identityCount,
                             endStats.identityCount - startStats.identityCount,
                             Hardware.residentMB()))
        await viewModel.log("Face clustering complete: \(processedFiles) files, \(totalPrints) prints → \(endStats.identityCount) identities.")
    }

    // MARK: - Post-processing

    func preparePreviewNames() async {
        let vm = viewModel
        await store.generateProposedNames(
            saveEvery: 500,
            tagger: { original, tags in
                MediaProcessor.generateFilenameStatic(original: original, tags: tags)
            },
            onProgress: { done, total in
                Task { @MainActor in
                    vm.namingDone  = done
                    vm.namingTotal = total
                }
            }
        )
        await runDuplicateDetection()
    }

    private func runDuplicateDetection() async {
        await store.runDuplicateDetection()
    }

    func applyIdentityNames(folderURL _: URL) async {
        let doRename = await viewModel.applyFilenameRename
        let doEXIF   = await viewModel.applyEXIFWrite
        let plans = await store.applyRenames(doRename: doRename, doEXIF: doEXIF)

        for plan in plans {
            let src = URL(fileURLWithPath: plan.srcPath)
            let dst = URL(fileURLWithPath: plan.dstPath)
            if plan.doEXIF {
                writeTagsToEXIF(src: src, dst: dst, tags: plan.tags)
            } else if src != dst {
                try? FileManager.default.moveItem(at: src, to: dst)
            }
        }
        await store.save()
    }

    // MARK: - Helpers

    nonisolated static func generateFilenameStatic(original: String, tags: [String]) -> String {
        let parts  = original.split(separator: ".")
        let base   = String(parts.first ?? "File")
        let ext    = String(parts.last  ?? "")
        let tagStr = tags.prefix(3).map { $0.replacingOccurrences(of: " ", with: "_") }.joined(separator: "_")
        let name   = tagStr.isEmpty ? base : "\(base)_\(tagStr)"
        return ext.isEmpty ? name : "\(name).\(ext)"
    }

    private func writeTagsToEXIF(src: URL, dst: URL, tags: [String]) {
        let tmp = src.deletingLastPathComponent()
            .appendingPathComponent(UUID().uuidString)
            .appendingPathExtension(src.pathExtension)
        guard let source = CGImageSourceCreateWithURL(src as CFURL, nil),
              let type   = CGImageSourceGetType(source),
              let dest   = CGImageDestinationCreateWithURL(tmp as CFURL, type, 1, nil) else { return }
        var props = (CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]) ?? [:]
        var iptc  = props[kCGImagePropertyIPTCDictionary] as? [CFString: Any] ?? [:]
        iptc[kCGImagePropertyIPTCKeywords] = tags
        props[kCGImagePropertyIPTCDictionary] = iptc
        CGImageDestinationAddImageFromSource(dest, source, 0, props as CFDictionary)
        CGImageDestinationFinalize(dest)
        try? FileManager.default.moveItem(at: tmp, to: dst)
        if src != dst { try? FileManager.default.removeItem(at: src) }
    }

    func processSingleNewFile(url: URL) async {
        let worker = VisionWorker()
        let result = await processFile(fileURL: url, worker: worker)
        let insertedID = await store.insertSingleNewResult(
            fileURL: url,
            tags: result.tags,
            hasFaces: result.hasFaces,
            aestheticScore: result.aestheticScore,
            facePrintsData: result.facePrintsData
        )
        guard let insertedID else { return }

        if !result.facePrintsData.isEmpty {
            let pairs: [(UUID, URL, Data)] = result.facePrintsData.map { (insertedID, url, $0) }
            let ok = await FaceClusteringService.shared.clusterBatch(prints: pairs)
            for id in ok { FacePrintCache.remove(id) }
        }
        await MainActor.run {
            viewModel.totalCount     += 1
            viewModel.processedCount += 1
            viewModel.log("New file detected & processed: \(url.lastPathComponent)")
        }
    }
}

// MARK: - Nonisolated category helper

nonisolated func fileIDCategory(for file: FileRecord) -> String {
    if let deep = file.deepAnalysis?.lowercased(), !deep.isEmpty {
        if deep.contains("invoice")                                     { return "Invoices" }
        if deep.contains("receipt")                                     { return "Receipts" }
        if deep.contains("tax") || deep.contains("w-2") || deep.contains("1099") { return "Taxes" }
        if deep.contains("contract") || deep.contains("agreement")      { return "Contracts" }
        if deep.contains("resume") || deep.contains("cv")               { return "Resumes" }
        if deep.contains("boarding pass") || deep.contains("itinerary") { return "Travel" }
        if deep.contains("presentation") || deep.contains("slides")     { return "Presentations" }
        if deep.contains("prescription") || deep.contains("medical")    { return "Medical" }
    }

    let ext = file.url.pathExtension.lowercased()
    if FileTypes.pdfs.contains(ext) {
        if file.aiTags.contains("Invoice")      { return "Invoices" }
        if file.aiTags.contains("Receipt")      { return "Receipts" }
        if file.aiTags.contains("Tax_Document") { return "Taxes" }
        return "Documents"
    }
    if FileTypes.documents.contains(ext)  { return "Documents" }
    if file.aiTags.contains("Screenshot") { return "Screenshots" }
    if FileTypes.videos.contains(ext)     { return "Videos" }
    if file.hasFaces                                        { return "People" }
    if file.aiTags.contains(where: { ["Landscape","Outdoor","Nature","Mountain","Beach","Sky"].contains($0) }) { return "Nature" }
    if file.aiTags.contains(where: { ["Food","Cooking"].contains($0) })            { return "Food" }
    if file.aiTags.contains(where: { ["Dog","Cat","Animal"].contains($0) })        { return "Animals" }
    return "Photos"
}
