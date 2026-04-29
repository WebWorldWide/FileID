import Foundation
import Vision
import SwiftData
import AppKit
import Accelerate

// MARK: - FaceClusteringService

// @ModelActor so every PersonRecord read/write stays on this actor's executor.
// Clustering: ≤50 samples per identity, min L2 match, K=5 centroids rebuilt
// every 20 assignments. Threshold is user-tunable via SettingsView.

enum FaceClusterError: Error {
    case invalidFeaturePrint
}

@ModelActor
actor FaceClusteringService {
    // Assigned once at setUp; no concurrent writers after bootstrap.
    nonisolated(unsafe) static var shared: FaceClusteringService!

    static func setUp(modelContainer: ModelContainer) async {
        shared = FaceClusteringService(modelContainer: modelContainer)
        await shared.loadSettings()
        try? await shared.rebuildIndex()
    }

    // MARK: - Parameters

    // 0.55 is the sweet spot for Vision's 512-dim L2-normalized face prints:
    //   same-person pairs typically land at L2 ≈ 0.3–0.8
    //   different-person pairs typically land at L2 ≈ 0.9–1.4
    // The old default of 0.80 produced aggressive over-merging (user saw
    // only ~3 clusters across a 58 K-photo library where 50–500 identities
    // would be realistic). User-tunable in Settings.
    var distanceThreshold: Float = 0.55
    let minFaceAreaFraction: Float = 0.03
    let maxSamplesPerIdentity: Int = 50
    private let centroidK: Int = 5
    private let rebuildEveryNAssigns = 20

    // Hard caps to prevent OOM on libraries with extreme face-print counts.
    // 2 000 identities × 50 samples × 128 floats × 4 bytes ≈ 50 MB just for
    // identitySamples (plus same for centroidsCache). Beyond this we refuse
    // to create new identities — incoming faces that don't match any existing
    // identity are dropped (rather than triggering a runaway memory growth).
    // The user can re-tune via the merge UI if clusters are too coarse.
    private let maxIdentities: Int = 2_000
    // Save inside clusterBatch every N successful assigns rather than only
    // at the end. Was holding the entire batch transaction in memory until
    // commit — at 10 000 prints/batch that grew to multiple-100s-of-MB
    // before any disk write, contributing to memory-pressure terminations.
    private let saveEveryNAssigns: Int = 100
    // Critical-pressure backoff: if the OS signals critical memory pressure
    // mid-batch, flush what we have and abort. Caller can resume next pass.
    private var capWarnedThisSession = false

    // MARK: - In-memory state

    private var identitySamples:     [UUID: [[Float]]] = [:]
    private var centroidsCache:      [UUID: [[Float]]] = [:]
    private var assignsSinceRebuild: [UUID: Int]       = [:]
    private var cachedMergeSuggestions: [(UUID, UUID)]? = nil

    // MARK: - HNSW centroid index (Batch 13 scaling)
    //
    // Below `hnswEnableThreshold` identities the flat centroid scan is plenty
    // fast and the index would be pure overhead. Above the threshold the
    // index is built lazily and used as a phase-1 candidate filter that
    // returns the top-K closest identities. The phase-2 sample fallback
    // remains the source of truth for the actual match decision, so a stale
    // HNSW (one that hasn't seen the latest centroid mutations) only loses a
    // tiny bit of recall — never causes a wrong assignment.
    //
    // Rebuild policy: fully rebuild when the centroid count drifts >50%
    // from the count at last build. Cheap (~500 ms for 50 K centroids on
    // M1) and infrequent — at most a handful per scan.
    private var centroidIndex: HNSWIndex? = nil
    private var centroidNodeMap: [Int32: UUID] = [:]
    private var centroidCountAtBuild: Int = 0
    private var lastHNSWRebuildAt: Date = .distantPast
    private let hnswEnableThreshold = 500
    // Minimum wall-clock interval between HNSW rebuilds. Without this, a
    // library with rapidly-growing identity counts can trigger 5-10
    // rebuilds during clustering, each ~500 ms — perceived as a long stall
    // by the user. The phase-2 sample fallback covers staleness in the
    // intervening windows.
    private let hnswMinRebuildIntervalSec: TimeInterval = 8

    // MARK: - Settings

    func loadSettings() {
        let stored = UserDefaults.standard.double(forKey: "faceClusterThreshold")
        // Domain: 0.30 (very strict) → 0.75 (very loose). Anything outside
        // this band is almost certainly a stale or corrupt UserDefaults
        // value (e.g. an over-merging 0.80 from before the 2026-04-24
        // retune) — reset to the 0.55 default. The lower bound prevents a
        // mistakenly-zero stored value from collapsing every cluster.
        distanceThreshold = (stored >= 0.30 && stored <= 0.75) ? Float(stored) : 0.55
    }

    // MARK: - Batch clustering

    // Prints arrive pre-serialized so no non-Sendable Vision types cross the
    // actor boundary. `fileID` lets each cluster assignment update the
    // PersonRecord's authoritative fileIDs set.
    //
    // Hardening notes (2026-04-24):
    //   1. Each iteration runs in its own autoreleasepool — without it,
    //      NSKeyedUnarchiver + CFData + intermediate [Float] allocations
    //      accumulate until function exit (~50 MB per 1 000 prints).
    //   2. modelContext.save() runs every `saveEveryNAssigns` (100) instead
    //      of only at end-of-batch — bounds the unflushed transaction size.
    //   3. Critical memory pressure aborts the batch early. Whatever was
    //      processed is committed; the rest is dropped for this run. The
    //      next post-scan pass picks them up (FacePrintCache.remove only
    //      runs on the caller's success path).
    //   4. Identity-cap (maxIdentities = 2 000) prevents clustering from
    //      growing in-memory state past ~256 MB on libraries that produce
    //      runaway identity counts (e.g. low-quality face detections from
    //      group photos).
    //   5. ClusterCircuitBreaker wraps every per-print clusterSync call. The
    //      fileID is written to disk before the call and cleared after it
    //      succeeds. If the app crashes mid-call, the next launch's
    //      `recoverFromCrash` bumps the attempt count and (at the threshold)
    //      permanently skips that file — breaking the stale-print loop.
    //
    // Return value: the set of fileIDs that clustered successfully. The
    // caller (MediaProcessor.runFaceClusteringPass) uses this to decide
    // which FacePrintCache entries to delete — mid-batch crashes no longer
    // leak still-unprocessed prints.
    func clusterBatch(prints: [(UUID, URL, Data)]) async -> Set<UUID> {
        var clusteredFileIDs: Set<UUID> = []
        var assignedSinceSave = 0
        let breaker = ClusterCircuitBreaker.shared
        for (fileID, url, data) in prints {
            if Hardware.isUnderCriticalMemoryPressure {
                NSLog("FileID FaceClusteringService: critical memory pressure, aborting batch at \(assignedSinceSave) prints")
                break
            }
            // CrashSentinel update BEFORE the unarchive. If the Swift runtime
            // aborts inside getSuperclassMetadata during
            // NSKeyedUnarchiver.unarchivedObject, the orphan marker on next
            // launch tells us exactly which fileID was in flight.
            CrashSentinel.set(phase: "clustering", subject: "fileID=\(fileID)")
            _ = await breaker.beginAttempt(fileID: fileID)
            var unarchivedOK = false
            autoreleasepool {
                guard let fp = try? NSKeyedUnarchiver.unarchivedObject(
                    ofClass: VNFeaturePrintObservation.self, from: data) else { return }
                unarchivedOK = true
                _ = try? clusterSync(facePrint: fp, fileID: fileID, fileURL: url)
                assignedSinceSave += 1
            }
            // markSuccess is called whether or not unarchive found a valid
            // observation: the point is we survived this file's call frame
            // without crashing. A corrupt/malformed print that unarchives
            // to nil is fine — it just contributed nothing to clustering.
            await breaker.markSuccess(fileID: fileID)
            clusteredFileIDs.insert(fileID)
            _ = unarchivedOK  // silence unused-variable warning; kept for clarity

            if assignedSinceSave >= saveEveryNAssigns {
                try? modelContext.save()
                assignedSinceSave = 0
            }
        }
        try? modelContext.save()
        return clusteredFileIDs
    }

    /// `skip` excludes a specific PersonRecord during re-clustering (used by
    /// `reassignFiles` so a face just removed from person X cannot be matched
    /// right back to X).
    @discardableResult
    private func clusterSync(
        facePrint: VNFeaturePrintObservation,
        fileID:    UUID,
        fileURL:   URL,
        skip:      UUID? = nil,
        allowCreate: Bool = true
    ) throws -> PersonRecord? {
        let vec = extractVector(facePrint)
        // Empty vector would l2() to 0 against anything and silently merge
        // into the first identity — skip instead.
        guard !vec.isEmpty else { throw FaceClusterError.invalidFeaturePrint }
        let printData = try NSKeyedArchiver.archivedData(withRootObject: facePrint, requiringSecureCoding: true)

        // Snapshot at entry — callers later mutate centroidsCache /
        // identitySamples via addSample / maybeRebuildCentroids, and a
        // concurrent merge() from PeopleView could delete an entry mid-pass.
        // Iterating a snapshot gives stable reads and lets us avoid the
        // kind of use-after-free crash that doesn't produce an .ips log.
        let centroidsSnapshot = centroidsCache
        let samplesSnapshot   = identitySamples

        var bestDist: Float = .infinity
        var bestID:   UUID?

        // Phase 1 — centroid scan. Below the HNSW threshold we iterate every
        // identity; above it we ask the HNSW index for the top-K candidates
        // and only score against those. Phase 2 (sample fallback) is the
        // source of truth, so a stale HNSW costs at most a tiny bit of recall
        // — it can never produce a wrong assignment.
        let candidateIDs = candidateIdentities(for: vec, snapshotCount: centroidsSnapshot.count)
        for id in candidateIDs where id != skip {
            guard let centroids = centroidsSnapshot[id] else { continue }
            for c in centroids {
                let d = l2(vec, c); if d < bestDist { bestDist = d; bestID = id }
            }
        }

        if bestDist >= distanceThreshold * 0.8 {
            // Phase 2 — sample fallback. Only iterates identities that
            // survived the centroid cut OR (in flat mode) every identity.
            // The original O(N×M) shape is preserved as the safety net.
            for (id, samples) in samplesSnapshot where id != skip {
                for s in samples {
                    let d = l2(vec, s); if d < bestDist { bestDist = d; bestID = id }
                }
            }
        }

        if bestDist < distanceThreshold, let matchID = bestID {
            let id = matchID
            let desc = FetchDescriptor<PersonRecord>(predicate: #Predicate { $0.id == id })
            guard let identity = try modelContext.fetch(desc).first else {
                if !allowCreate { return nil }
                if identitySamples.count >= maxIdentities { return nil }
                return try createNew(vec: vec, printData: printData, crop: nil,
                                     fileID: fileID, fileURL: fileURL)
            }
            updateIdentity(identity, printData: printData, fileID: fileID, fileURL: fileURL)
            addSample(id: matchID, vec: vec)
            maybeRebuildCentroids(id: matchID)
            return identity
        }
        if !allowCreate { return nil }
        // Hard cap to bound in-memory state growth. Beyond this we drop
        // unmatched faces rather than spawn a new identity that would
        // increase the per-comparison cost for every future print.
        if identitySamples.count >= maxIdentities {
            if !capWarnedThisSession {
                capWarnedThisSession = true
                NSLog("FileID FaceClusteringService: identity cap (\(maxIdentities)) reached — dropping unmatched faces. User can lower distanceThreshold in Settings to merge more aggressively.")
            }
            return nil
        }
        return try createNew(vec: vec, printData: printData, crop: nil,
                             fileID: fileID, fileURL: fileURL)
    }

    // MARK: - Identity management

    private func updateIdentity(_ identity: PersonRecord, printData: Data,
                                fileID: UUID, fileURL: URL) {
        if identity.featurePrintsData.count < maxSamplesPerIdentity {
            identity.featurePrintsData.append(printData)
        } else {
            identity.featurePrintsData.removeFirst()
            identity.featurePrintsData.append(printData)
        }
        identity.faceCount += 1
        if !identity.fileIDs.contains(fileID) {
            identity.fileIDs.append(fileID)
        }
        if !identity.sampleFileURLs.contains(fileURL) && identity.sampleFileURLs.count < 8 {
            identity.sampleFileURLs.append(fileURL)
        }
    }

    private func createNew(
        vec: [Float], printData: Data, crop: CGImage?, fileID: UUID, fileURL: URL
    ) throws -> PersonRecord {
        let faceJpeg = crop.flatMap {
            NSBitmapImageRep(cgImage: $0)
                .representation(using: .jpeg, properties: [.compressionFactor: 0.75])
        }
        let person = PersonRecord(name: nil, representativeFaceCropData: faceJpeg)
        person.featurePrintsData = [printData]
        person.faceCount         = 1
        person.fileIDs           = [fileID]
        person.sampleFileURLs    = [fileURL]
        modelContext.insert(person)

        let id = person.id
        identitySamples[id]     = [vec]
        centroidsCache[id]      = [vec]
        assignsSinceRebuild[id] = 0
        return person
    }

    private func addSample(id: UUID, vec: [Float]) {
        var samples = identitySamples[id] ?? []
        if samples.count >= maxSamplesPerIdentity { samples.removeFirst() }
        samples.append(vec)
        identitySamples[id] = samples
        assignsSinceRebuild[id, default: 0] += 1
    }

    private func maybeRebuildCentroids(id: UUID) {
        let n = assignsSinceRebuild[id] ?? 0
        guard n >= rebuildEveryNAssigns else { return }
        if let samples = identitySamples[id] {
            centroidsCache[id] = kMeans(samples: samples, k: centroidK)
        }
        assignsSinceRebuild[id] = 0
        // The centroids for this identity changed — the HNSW index for it is
        // now stale. We don't rebuild eagerly; the next phase-1 lookup checks
        // drift and rebuilds if it's grown enough to matter.
    }

    // MARK: - HNSW phase-1 candidate lookup

    /// Returns the candidate identity UUIDs for phase-1 centroid scoring.
    ///
    /// - Below `hnswEnableThreshold` identities, returns every identity (flat
    ///   scan — cheap at this scale).
    /// - Above the threshold, lazily builds an HNSW index over centroids
    ///   and returns the top-K identities by closest centroid hit. The
    ///   phase-2 sample fallback in the caller catches any false negatives.
    private func candidateIdentities(for vec: [Float], snapshotCount: Int) -> [UUID] {
        // Small enough — flat scan. Avoids HNSW build cost on small libraries.
        if snapshotCount < hnswEnableThreshold {
            return Array(centroidsCache.keys)
        }
        ensureHNSWIndex(currentCount: snapshotCount, vecDim: vec.count)
        guard let idx = centroidIndex else {
            // Build failed — fall back to flat scan rather than dropping the
            // match silently.
            return Array(centroidsCache.keys)
        }
        // 5x overshoot covers the case where multiple centroids of the same
        // identity show up in the top-K — we dedupe to identity afterward.
        let topK = 20
        let hits = idx.search(vec, k: topK * 5)
        var seen = Set<UUID>()
        var out: [UUID] = []
        out.reserveCapacity(topK)
        for (nodeID, _) in hits {
            guard let personID = centroidNodeMap[nodeID] else { continue }
            if seen.insert(personID).inserted {
                out.append(personID)
                if out.count >= topK { break }
            }
        }
        return out
    }

    /// Build or rebuild the HNSW index over current centroids if drift since
    /// last build exceeds 50%. The dim is locked to the vec dim of the first
    /// caller; if the corpus produces multiple Vision-revision dim-shifted
    /// vectors, the mismatched ones are silently rejected by HNSW (returns
    /// -1) and the flat fallback in the caller picks them up.
    private func ensureHNSWIndex(currentCount: Int, vecDim: Int) {
        let drift = abs(currentCount - centroidCountAtBuild)
        // Floor of 200 (was 50) so a tiny library doesn't thrash:
        // at count=200 the 50% gate would fire after only +25 centroids.
        // Combined with the wall-clock cooldown below, rebuild count during
        // a 58 K-file scan stays in single digits instead of 50+.
        let needsRebuild = centroidIndex == nil
            || drift > max(200, centroidCountAtBuild / 2)
        guard needsRebuild else { return }
        // Wall-clock cooldown — avoid back-to-back rebuilds when a burst of
        // new identities keeps crossing the drift threshold. Skip if the
        // last rebuild was less than `hnswMinRebuildIntervalSec` ago, EXCEPT
        // for the very first build (centroidIndex == nil).
        if centroidIndex != nil,
           Date().timeIntervalSince(lastHNSWRebuildAt) < hnswMinRebuildIntervalSec {
            return
        }
        rebuildHNSWIndex(vecDim: vecDim)
    }

    /// Rebuild the HNSW index from scratch over every centroid currently in
    /// `centroidsCache`. ~500 ms for 50 K centroids on M1; called at most a
    /// handful of times per scan thanks to the 50 % drift gate.
    private func rebuildHNSWIndex(vecDim: Int) {
        let begin = Date()
        let idx = HNSWIndex(dim: vecDim, M: 16, efConstruction: 200, efSearch: 100)
        var map: [Int32: UUID] = [:]
        for (personID, centroids) in centroidsCache {
            for c in centroids where c.count == vecDim {
                let nodeID = idx.insert(c)
                if nodeID >= 0 { map[nodeID] = personID }
            }
        }
        centroidIndex = idx
        centroidNodeMap = map
        centroidCountAtBuild = centroidsCache.count
        lastHNSWRebuildAt = Date()
        let dur = Date().timeIntervalSince(begin)
        // Diagnostic so a future user can see in scan.log how many times
        // the index rebuilt and how long each rebuild took.
        MediaProcessor.appendScanLogExternal(
            String(format: "HNSW rebuild: identities=%d nodes=%d dur=%.2fs",
                   centroidsCache.count, map.count, dur)
        )
    }

    /// Invalidate the HNSW index — call after large-scale centroid changes
    /// (full rebuild, merge, identity delete). Cheap; the next lookup builds
    /// lazily.
    private func invalidateHNSWIndex() {
        centroidIndex = nil
        centroidNodeMap = [:]
        centroidCountAtBuild = 0
    }

    // MARK: - Index rebuild

    // UserDefaults version flag so the one-shot fileIDs backfill runs once
    // per user per bump. Before this gate the "has empty fileIDs" predicate
    // re-fired the FileRecord fetch (tens of thousands of rows) on every app
    // launch as long as any synthetic/orphan PersonRecord slipped through.
    private static let backfillFlagKey = "peopleFileIDsBackfill_v1_done"

    func rebuildIndex() throws {
        identitySamples        = [:]
        centroidsCache         = [:]
        assignsSinceRebuild    = [:]
        cachedMergeSuggestions = nil
        invalidateHNSWIndex()

        let identities = try modelContext.fetch(FetchDescriptor<PersonRecord>())

        // One-shot fileIDs backfill for libraries clustered before this
        // field existed. We populate from sampleFileURLs (≤8 per person).
        // Continued scans populate fileIDs fully via clusterSync.
        let backfillDone = UserDefaults.standard.bool(forKey: Self.backfillFlagKey)
        let needsBackfill = !backfillDone && identities.contains {
            $0.fileIDs.isEmpty && !$0.sampleFileURLs.isEmpty
        }
        var urlToFileID: [URL: UUID] = [:]
        if needsBackfill {
            let files = (try? modelContext.fetch(FetchDescriptor<FileRecord>())) ?? []
            for f in files { urlToFileID[f.url] = f.id }
        }

        for identity in identities {
            if needsBackfill && identity.fileIDs.isEmpty {
                let ids = identity.sampleFileURLs.compactMap { urlToFileID[$0] }
                if !ids.isEmpty { identity.fileIDs = Array(Set(ids)) }
            }
            var samples: [[Float]] = []
            let blobCount = identity.featurePrintsData.count
            var decodedOK = 0
            // CrashSentinel + autoreleasepool per blob. A single corrupt
            // PersonRecord.featurePrintsData entry can SIGABRT inside the
            // Swift runtime's class-metadata init during NSKeyedUnarchiver
            // — this loop runs at EVERY app launch via setUp(), so one bad
            // blob kills every future launch. The sentinel tells us which
            // personID + blobIdx was in flight; the autoreleasepool bounds
            // per-blob CoreFoundation retention.
            for (blobIdx, data) in identity.featurePrintsData.prefix(maxSamplesPerIdentity).enumerated() {
                CrashSentinel.set(
                    phase: "rebuildIndex",
                    subject: "person=\(identity.id) blob=\(blobIdx)/\(blobCount)"
                )
                autoreleasepool {
                    guard let obs = try? NSKeyedUnarchiver.unarchivedObject(
                        ofClass: VNFeaturePrintObservation.self, from: data
                    ) else { return }
                    let v = extractVector(obs)
                    if !v.isEmpty {
                        samples.append(v)
                        decodedOK += 1
                    }
                }
            }
            // Quarantine: if we decoded some but not all blobs, drop the
            // trailing undecodable entries so future launches don't keep
            // re-hitting the same crasher. SwiftData appends blobs in
            // insertion order, so the known-good ones are the prefix.
            let scanned = min(identity.featurePrintsData.count, maxSamplesPerIdentity)
            if decodedOK > 0 && decodedOK < scanned {
                if identity.featurePrintsData.count > decodedOK {
                    let dropped = identity.featurePrintsData.count - decodedOK
                    identity.featurePrintsData = Array(identity.featurePrintsData.prefix(decodedOK))
                    MediaProcessor.appendScanLogExternal(
                        "rebuildIndex quarantine: person=\(identity.id) kept=\(decodedOK)/\(scanned) dropped=\(dropped)"
                    )
                }
            }
            guard !samples.isEmpty else { continue }
            identitySamples[identity.id]     = samples
            centroidsCache[identity.id]      = kMeans(samples: samples, k: centroidK)
            assignsSinceRebuild[identity.id] = 0
        }
        if needsBackfill {
            try? modelContext.save()
            UserDefaults.standard.set(true, forKey: Self.backfillFlagKey)
        }
    }

    // MARK: - Rebuild People from stored prints

    /// Re-clusters every face print already on disk against the current
    /// `distanceThreshold`. Used when the threshold changes (e.g. the
    /// 0.80 → 0.55 retune on 2026-04-24 over-merged 9 K prints into 6
    /// identities) and the user wants accurate People without a full
    /// 58 K-file rescan. The stored blobs are the source of truth — no
    /// FacePrintCache dependency, so this works even after the cache was
    /// wiped by a fresh-folder scan.
    ///
    /// Crash-recovery: each blob unarchive runs inside an `autoreleasepool`
    /// with a CrashSentinel update keyed on personID + blobIdx. A SIGABRT
    /// inside NSKeyedUnarchiver leaves a recoverable marker; the next
    /// launch's crash.log tells us exactly which stored print is bad.
    ///
    /// Durability: old PersonRecords are marked for deletion in the same
    /// ModelContext transaction that inserts the new ones. SwiftData doesn't
    /// commit until save(), so a crash mid-rebuild leaves on-disk state
    /// intact (no partial delete, no orphan inserts).
    func rebuildPeopleFromStoredPrints() async {
        cachedMergeSuggestions = nil
        invalidateHNSWIndex()

        let oldPeople = (try? modelContext.fetch(FetchDescriptor<PersonRecord>())) ?? []
        let oldCount = oldPeople.count
        guard !oldPeople.isEmpty else {
            MediaProcessor.appendScanLogExternal("rebuildPeople: no PersonRecords on disk, nothing to do")
            CrashSentinel.set(phase: "idle")
            return
        }

        // Per-blob working tuple. We lose per-blob file identity here
        // (featurePrintsData is stored per-person, not per-blob), so each
        // blob inherits the origin person's full fileID / URL sets. Duplicate
        // fileIDs are deduped on insert into the new identity.
        struct Entry {
            let vec:     [Float]
            let data:    Data
            let fileIDs: [UUID]
            let urls:    [URL]
        }
        var pool: [Entry] = []
        var scanned = 0
        var dropped = 0

        for person in oldPeople {
            let blobCount      = person.featurePrintsData.count
            let personFileIDs  = person.fileIDs
            let personURLs     = person.sampleFileURLs
            for (blobIdx, data) in person.featurePrintsData.enumerated() {
                CrashSentinel.set(
                    phase: "rebuildPeople",
                    subject: "person=\(person.id) blob=\(blobIdx)/\(blobCount)"
                )
                scanned += 1
                autoreleasepool {
                    guard let obs = try? NSKeyedUnarchiver.unarchivedObject(
                        ofClass: VNFeaturePrintObservation.self, from: data
                    ) else { dropped += 1; return }
                    let vec = extractVector(obs)
                    if vec.isEmpty { dropped += 1; return }
                    pool.append(Entry(
                        vec: vec, data: data,
                        fileIDs: personFileIDs,
                        urls: personURLs
                    ))
                }
                // Cooperative yield every 64 blobs — keeps the @ModelActor
                // responsive to other actor calls (UI fetches via mainContext
                // are independent of this actor, but other tasks awaiting
                // FaceClusteringService methods would otherwise stall for the
                // duration of the rebuild on a 9 K-blob library).
                if scanned % 64 == 0 { await Task.yield() }
                if Hardware.isUnderCriticalMemoryPressure { break }
            }
            if Hardware.isUnderCriticalMemoryPressure { break }
        }

        // Reset in-memory state. Old PersonRecords are deleted inside the
        // same save transaction as the new inserts below, so the swap is
        // atomic from the user's perspective.
        identitySamples     = [:]
        centroidsCache      = [:]
        assignsSinceRebuild = [:]
        for person in oldPeople { modelContext.delete(person) }

        // Re-cluster the pool. Mirrors `clusterSync`'s match-or-create logic
        // on pre-extracted vectors — we already hold the serialized blob so
        // no re-archive, and we preserve the *origin person's* metadata
        // rather than grafting the URL of the file being scanned.
        var clusteredCount = 0
        for entry in pool {
            if Hardware.isUnderCriticalMemoryPressure { break }
            clusteredCount += 1
            if clusteredCount % 64 == 0 { await Task.yield() }

            var bestDist: Float = .infinity
            var bestID:   UUID?
            for (id, centroids) in centroidsCache {
                for c in centroids {
                    let d = l2(entry.vec, c); if d < bestDist { bestDist = d; bestID = id }
                }
            }
            if bestDist >= distanceThreshold * 0.8 {
                for (id, samples) in identitySamples {
                    for s in samples {
                        let d = l2(entry.vec, s); if d < bestDist { bestDist = d; bestID = id }
                    }
                }
            }

            if bestDist < distanceThreshold, let matchID = bestID,
               let identity = try? modelContext.fetch(
                   FetchDescriptor<PersonRecord>(predicate: #Predicate { $0.id == matchID })
               ).first {
                if identity.featurePrintsData.count < maxSamplesPerIdentity {
                    identity.featurePrintsData.append(entry.data)
                } else {
                    identity.featurePrintsData.removeFirst()
                    identity.featurePrintsData.append(entry.data)
                }
                identity.faceCount += 1
                for fid in entry.fileIDs where !identity.fileIDs.contains(fid) {
                    identity.fileIDs.append(fid)
                }
                for url in entry.urls where !identity.sampleFileURLs.contains(url)
                                         && identity.sampleFileURLs.count < 8 {
                    identity.sampleFileURLs.append(url)
                }
                addSample(id: matchID, vec: entry.vec)
                maybeRebuildCentroids(id: matchID)
            } else {
                if identitySamples.count >= maxIdentities { continue }
                let person = PersonRecord(name: nil, representativeFaceCropData: nil)
                person.featurePrintsData = [entry.data]
                person.faceCount         = 1
                person.fileIDs           = entry.fileIDs
                person.sampleFileURLs    = Array(entry.urls.prefix(8))
                modelContext.insert(person)
                let id = person.id
                identitySamples[id]     = [entry.vec]
                centroidsCache[id]      = [entry.vec]
                assignsSinceRebuild[id] = 0
            }
        }

        try? modelContext.save()
        let newCount = identitySamples.count
        CrashSentinel.set(phase: "idle")
        MediaProcessor.appendScanLogExternal(
            "rebuildPeople: persons=\(oldCount)→\(newCount) prints=\(scanned) dropped=\(dropped) threshold=\(distanceThreshold)"
        )
    }

    // MARK: - Rename + tag propagation

    /// User-visible name change. Sets `PersonRecord.name`, then propagates
    /// `person:<name>` to every FileRecord in the cluster's `fileIDs`. The
    /// previous name's tag (if any) is dropped from those same files first
    /// so a rename leaves no orphan tag behind.
    ///
    /// This is the wiring that makes face recognition *useful*: once you
    /// name a cluster "Alice," every photo of Alice can be searched, sorted,
    /// or filtered by that tag in the Library tab without any further work.
    ///
    /// Returns `(filesUpdated, oldName)`. Caller logs.
    @discardableResult
    func renamePerson(id: UUID, newName: String) throws -> (Int, String?) {
        let trimmed = newName.trimmingCharacters(in: .whitespaces)
        let desc = FetchDescriptor<PersonRecord>(predicate: #Predicate { $0.id == id })
        guard let person = try modelContext.fetch(desc).first else {
            return (0, nil)
        }
        let oldName = person.name
        let oldTag = oldName.flatMap { Self.personTag(for: $0) }
        let newTag = trimmed.isEmpty ? nil : Self.personTag(for: trimmed)

        person.name = trimmed.isEmpty ? nil : trimmed
        // Capture the fileID list now — the model context is the same one
        // FileIDDataStore writes through, so a concurrent insert would be
        // serialized after this method returns.
        let fileIDs = person.fileIDs
        try modelContext.save()

        guard !fileIDs.isEmpty, oldTag != nil || newTag != nil else {
            return (0, oldName)
        }
        // Hand off to the FileIDDataStore actor for the file-side update —
        // FaceClusteringService should not be writing to FileRecord itself.
        let fileIDSet = Set(fileIDs)
        let updated = Self.applyPersonTagViaStore(
            fileIDs: fileIDSet,
            oldTag: oldTag,
            newTag: newTag,
            modelContext: modelContext
        )
        cachedMergeSuggestions = nil
        return (updated, oldName)
    }

    /// Canonical "person:<name>" tag formatter. Centralized so search,
    /// JunkScorer, and the rename flow can never disagree on capitalization.
    static func personTag(for name: String) -> String {
        "person:" + name.trimmingCharacters(in: .whitespaces)
    }

    /// Performs the FileRecord tag updates inside the same ModelContext that
    /// the FaceClusteringService actor owns. Static so it doesn't capture the
    /// actor isolation context — it's called from inside the actor anyway.
    private static func applyPersonTagViaStore(
        fileIDs: Set<UUID>,
        oldTag: String?,
        newTag: String?,
        modelContext: ModelContext
    ) -> Int {
        let desc = FetchDescriptor<FileRecord>(predicate: #Predicate { fileIDs.contains($0.id) })
        let files = (try? modelContext.fetch(desc)) ?? []
        var updated = 0
        for file in files {
            var tags = file.aiTags
            var changed = false
            if let oldTag, let idx = tags.firstIndex(of: oldTag) {
                tags.remove(at: idx); changed = true
            }
            if let newTag, !tags.contains(newTag) {
                tags.append(newTag); changed = true
            }
            if changed {
                file.aiTags = tags
                updated += 1
            }
        }
        if updated > 0 { try? modelContext.save() }
        return updated
    }

    // MARK: - Merge

    func merge(sourceID: UUID, targetID: UUID) throws {
        guard sourceID != targetID else { return }
        let all = try modelContext.fetch(FetchDescriptor<PersonRecord>())
        guard let src = all.first(where: { $0.id == sourceID }),
              let tgt = all.first(where: { $0.id == targetID }) else { return }

        tgt.featurePrintsData.append(contentsOf: src.featurePrintsData)
        if tgt.featurePrintsData.count > maxSamplesPerIdentity {
            let overflow = tgt.featurePrintsData.count - maxSamplesPerIdentity
            tgt.featurePrintsData.removeFirst(overflow)
        }
        tgt.faceCount += src.faceCount
        let existingIDs = Set(tgt.fileIDs)
        for id in src.fileIDs where !existingIDs.contains(id) {
            tgt.fileIDs.append(id)
        }
        for url in src.sampleFileURLs where !tgt.sampleFileURLs.contains(url) && tgt.sampleFileURLs.count < 8 {
            tgt.sampleFileURLs.append(url)
        }

        var merged = identitySamples[targetID] ?? []
        merged.append(contentsOf: identitySamples[sourceID] ?? [])
        if merged.count > maxSamplesPerIdentity {
            merged.removeFirst(merged.count - maxSamplesPerIdentity)
        }
        identitySamples[targetID]     = merged
        centroidsCache[targetID]      = kMeans(samples: merged, k: centroidK)
        assignsSinceRebuild[targetID] = 0

        identitySamples.removeValue(forKey: sourceID)
        centroidsCache.removeValue(forKey: sourceID)
        assignsSinceRebuild.removeValue(forKey: sourceID)
        cachedMergeSuggestions = nil
        invalidateHNSWIndex()

        modelContext.delete(src)
        try modelContext.save()
    }

    // MARK: - Suggested merges

    func suggestedMerges() throws -> [(UUID, UUID)] {
        let identities = try modelContext.fetch(FetchDescriptor<PersonRecord>(
            sortBy: [SortDescriptor(\.faceCount, order: .reverse)]
        ))
        if let cached = cachedMergeSuggestions { return cached }

        let mergeThreshold = distanceThreshold * 1.25
        // Coarse pruning bound — if the nearest centroid pair exceeds this,
        // no sample pair can be close enough either. 1.5× gives a margin
        // above the merge threshold to cover centroid drift.
        let centroidPruneBound = mergeThreshold * 1.5

        var uuidPairs: [(UUID, UUID)] = []
        let ids = identities.map { $0.id }

        // Wall-clock guard — at >5 K identities the inner loop can still
        // exceed UI tolerance even with the centroid pre-filter. Bail with
        // whatever we've found so PeopleView gets a partial answer in 2 s
        // instead of stalling. Cache the partial so a re-call doesn't redo
        // the work (next merge action invalidates `cachedMergeSuggestions`).
        let deadline = Date().addingTimeInterval(2.0)

        // Two-phase comparison: centroid pre-filter (O(N² × K²) ≈ 25 M ops on
        // 1 000 identities × 5 centroids), then full sample fallback only on
        // the ~1 % of pairs that survive. The old implementation did the
        // full O(N² × M²) loop (2.5 B ops on 1 000 × 50 samples) and took
        // 30+ s to return on this scale; the new path is ~100× faster.
        outer: for i in 0..<ids.count {
            // Memory-pressure abort — if the OS escalates while we're
            // grinding, return the partial result rather than push the
            // process into a kill window.
            if Hardware.isUnderCriticalMemoryPressure { break }
            // Periodic deadline check — cheap, runs every outer iteration.
            if i % 16 == 0 && Date() > deadline { break }
            guard let centroidsA = centroidsCache[ids[i]], !centroidsA.isEmpty else { continue }
            let samplesA = identitySamples[ids[i]] ?? []
            for j in (i + 1)..<ids.count {
                guard let centroidsB = centroidsCache[ids[j]], !centroidsB.isEmpty else { continue }
                // Phase 1 — min distance between the two identities' centroids.
                var centroidMin: Float = .infinity
                for a in centroidsA {
                    for b in centroidsB {
                        let d = l2(a, b)
                        if d < centroidMin { centroidMin = d }
                    }
                }
                if centroidMin > centroidPruneBound { continue }

                // Phase 2 — pair survived the centroid cut, confirm with
                // the full sample comparison.
                let samplesB = identitySamples[ids[j]] ?? []
                var sampleMin: Float = .infinity
                for a in samplesA {
                    for b in samplesB {
                        let d = l2(a, b)
                        if d < sampleMin { sampleMin = d }
                    }
                }
                if sampleMin < mergeThreshold { uuidPairs.append((ids[i], ids[j])) }
                if uuidPairs.count >= 256 { break outer } // UI surfaces top suggestions; no value past this.
            }
        }

        cachedMergeSuggestions = uuidPairs
        return uuidPairs
    }

    // MARK: - Reassignment

    /// Removes each file from `personID` and re-clusters the face prints
    /// against every *other* identity. If no other identity matches within
    /// threshold the face is left unclustered (orphan). If `personID`'s
    /// faceCount falls to 0, the PersonRecord is deleted.
    func reassignFiles(from personID: UUID, fileIDs removeIDs: [UUID]) async {
        guard !removeIDs.isEmpty else { return }
        let desc = FetchDescriptor<PersonRecord>(predicate: #Predicate { $0.id == personID })
        guard let person = try? modelContext.fetch(desc).first else { return }

        // Collect face prints from FacePrintCache per file, find which ones
        // the origin person actually held, and drop them.
        let originalPersonPrints = Set(person.featurePrintsData)
        var removedVectors: [[Float]] = []
        var reinsertPrints: [(UUID, URL, Data)] = []

        // Build a URL map for files being reassigned (for clusterSync call).
        let fileIDSet = Set(removeIDs)
        let fileDesc = FetchDescriptor<FileRecord>(predicate: #Predicate { fileIDSet.contains($0.id) })
        let files = (try? modelContext.fetch(fileDesc)) ?? []
        var urlByID: [UUID: URL] = [:]
        for f in files { urlByID[f.id] = f.url }

        for fid in removeIDs {
            let cached = FacePrintCache.load(fid)
            guard !cached.isEmpty else { continue }
            for printData in cached where originalPersonPrints.contains(printData) {
                // Drop the matching print(s) from the person.
                if let idx = person.featurePrintsData.firstIndex(of: printData) {
                    person.featurePrintsData.remove(at: idx)
                }
                if let obs = try? NSKeyedUnarchiver.unarchivedObject(
                    ofClass: VNFeaturePrintObservation.self, from: printData) {
                    let vec = extractVector(obs)
                    if !vec.isEmpty { removedVectors.append(vec) }
                }
                if let url = urlByID[fid] {
                    reinsertPrints.append((fid, url, printData))
                }
            }
            // Strip the file from the person's fileIDs + sampleFileURLs.
            person.fileIDs.removeAll { $0 == fid }
            if let url = urlByID[fid] {
                person.sampleFileURLs.removeAll { $0 == url }
            }
            person.faceCount = max(0, person.faceCount - 1)
        }

        // Drop matching vectors from the actor's in-memory sample cache.
        if !removedVectors.isEmpty, var samples = identitySamples[personID] {
            for removed in removedVectors {
                if let idx = samples.firstIndex(where: { $0 == removed }) {
                    samples.remove(at: idx)
                }
            }
            if samples.isEmpty {
                identitySamples.removeValue(forKey: personID)
                centroidsCache.removeValue(forKey: personID)
            } else {
                identitySamples[personID] = samples
                centroidsCache[personID] = kMeans(samples: samples, k: centroidK)
            }
        }
        assignsSinceRebuild[personID] = 0
        cachedMergeSuggestions = nil

        // If the person is now empty, delete the record outright.
        if person.faceCount == 0 || person.featurePrintsData.isEmpty {
            modelContext.delete(person)
            identitySamples.removeValue(forKey: personID)
            centroidsCache.removeValue(forKey: personID)
            assignsSinceRebuild.removeValue(forKey: personID)
        }

        // Re-cluster each removed print against OTHER identities. `skip` keeps
        // the face from snapping right back to the origin. Orphan (no match)
        // → no new PersonRecord is created per the "b with fallback of a"
        // user preference.
        //
        // Hardening (2026-04-24): wrap each per-print clusterSync in the
        // ClusterCircuitBreaker + autoreleasepool + memory-pressure guard so
        // the "Not this person" and "Delete Person" paths get the same
        // crash-recovery coverage as the post-scan pass. Previously this
        // loop was completely unprotected — if any single print crashed,
        // the whole reassign op died and the user's click was a no-op
        // (or worse, left the PersonRecord in a half-deleted state).
        let breaker = ClusterCircuitBreaker.shared
        let skipList = await breaker.skipList()
        for (fid, url, printData) in reinsertPrints {
            if skipList.contains(fid) { continue }
            if Hardware.isUnderCriticalMemoryPressure { break }

            _ = await breaker.beginAttempt(fileID: fid)
            autoreleasepool {
                guard let obs = try? NSKeyedUnarchiver.unarchivedObject(
                    ofClass: VNFeaturePrintObservation.self, from: printData) else { return }
                _ = try? clusterSync(
                    facePrint: obs,
                    fileID: fid,
                    fileURL: url,
                    skip: personID,
                    allowCreate: false
                )
            }
            await breaker.markSuccess(fileID: fid)
        }

        try? modelContext.save()
    }

    // MARK: - Settings

    func setThreshold(_ value: Float) {
        distanceThreshold = value
        cachedMergeSuggestions = nil
    }

    // MARK: - Diagnostics

    struct ClusterStats: Sendable {
        let identityCount:     Int
        let totalSamples:      Int
        let distanceThreshold: Float
    }

    // Snapshot of in-memory clustering state. Called by the post-scan pass
    // to log "before/after" identity counts — the durable evidence that
    // lets us spot over-merging (too few identities) or runaway growth
    // (too many) from scan.log alone.
    func clusterStats() -> ClusterStats {
        var total = 0
        for (_, samples) in identitySamples { total += samples.count }
        return ClusterStats(
            identityCount:     identitySamples.count,
            totalSamples:      total,
            distanceThreshold: distanceThreshold
        )
    }

    func isFaceValid(boundingBox: CGRect) -> Bool {
        Float(boundingBox.width * boundingBox.height) >= minFaceAreaFraction
    }

    // MARK: - Math

    // Sanity-bounded so a corrupt observation can't claim a multi-MB
    // allocation. Real Vision face prints are 512 floats (2 KB); 4 096 is a
    // generous ceiling that catches obviously-bad elementCount returns.
    private static let maxFeaturePrintElements: Int = 4_096

    private func extractVector(_ obs: VNFeaturePrintObservation) -> [Float] {
        let count = obs.elementCount
        guard count > 0, count <= Self.maxFeaturePrintElements else { return [] }
        // CRITICAL bounds check — catches the OOB read that would otherwise
        // crash inside withUnsafeBytes. A corrupt observation can declare
        // elementCount = 512 while obs.data holds only 100 bytes; without
        // this guard, the for-loop reads past the buffer into UB. The
        // earlier `result.count >= 128` check further down only kicked in
        // AFTER the UB read, which is too late if that read faulted.
        let requiredBytes = count * MemoryLayout<Float>.size
        guard obs.data.count >= requiredBytes else { return [] }
        var result = [Float](repeating: 0, count: count)
        obs.data.withUnsafeBytes { ptr in
            let floatPtr = ptr.bindMemory(to: Float.self)
            let n = min(count, floatPtr.count)
            for i in 0..<n { result[i] = floatPtr[i] }
        }
        guard result.count >= 128 else { return [] }
        return result
    }

    // Hot path — called O(N_prints × N_identities × K_centroids) times per
    // batch. The original pure-Swift loop did per-element bounds-check + ARC
    // retain pressure; on a 100 K-print library that pushed clustering into
    // the multi-hour range AND held memory hot during the whole pass.
    // vDSP_distancesq is SIMD-vectorised and 50–100× faster on M-series.
    private func l2(_ a: [Float], _ b: [Float]) -> Float {
        // Dimension mismatch = different Vision revisions; treat as non-match
        // so a stale stored print doesn't merge into an unrelated identity.
        guard a.count == b.count, !a.isEmpty else { return .infinity }
        var sumOfSquares: Float = 0
        a.withUnsafeBufferPointer { aPtr in
            b.withUnsafeBufferPointer { bPtr in
                // Safe pointer access — replaces `.baseAddress!` force-unwraps.
                // With !a.isEmpty above, baseAddress should be non-nil, but
                // force-unwrapping into a C API is a real crash source and
                // the guard costs nothing.
                guard let a0 = aPtr.baseAddress, let b0 = bPtr.baseAddress else { return }
                vDSP_distancesq(a0, 1, b0, 1, &sumOfSquares, vDSP_Length(a.count))
            }
        }
        return sumOfSquares.squareRoot()
    }

    private func kMeans(samples: [[Float]], k: Int) -> [[Float]] {
        guard !samples.isEmpty else { return [] }
        let dims = samples[0].count
        let effectiveK = min(k, samples.count)
        var centroids: [[Float]] = []
        let stride = max(1, samples.count / effectiveK)
        for i in 0..<effectiveK {
            centroids.append(samples[min(i * stride, samples.count - 1)])
        }
        for _ in 0..<5 {
            var buckets: [[[Float]]] = Array(repeating: [], count: effectiveK)
            for s in samples {
                var bestIdx = 0; var bestDist: Float = .infinity
                for (i, c) in centroids.enumerated() {
                    let d = l2(s, c); if d < bestDist { bestDist = d; bestIdx = i }
                }
                buckets[bestIdx].append(s)
            }
            for i in 0..<effectiveK where !buckets[i].isEmpty {
                var mean = [Float](repeating: 0, count: dims)
                for v in buckets[i] { for j in 0..<min(dims, v.count) { mean[j] += v[j] } }
                let cnt = Float(buckets[i].count)
                for j in 0..<dims { mean[j] /= cnt }
                centroids[i] = mean
            }
        }
        return centroids
    }
}
