import Foundation
import SwiftData
import Vision
import ImageIO
import UniformTypeIdentifiers

// MARK: - FileIDDataStore

// Single-writer SwiftData actor. All writes and bulk fetches route through
// here; @MainActor views read from the container's mainContext directly.

struct DeepAnalyzeTarget: Sendable {
    let id: UUID
    let url: URL
}

@ModelActor
actor FileIDDataStore {

    // MARK: - Live-scan indexes

    // Incremental duplicate state, rebuilt per scan. Lets the Cleanup tab's
    // @Query surface duplicates mid-scan instead of after a post-scan sweep.
    private var pHashIndex: [UInt64: (groupID: UUID, firstRecordID: UUID, count: Int)] = [:]
    private var pHashIndexDirty: Bool = false
    private var recordByID: [UUID: FileRecord] = [:]

    // MARK: - Generic escape hatch

    func perform<T: Sendable>(_ block: @Sendable (ModelContext) throws -> T) rethrows -> T {
        try block(modelContext)
    }

    func save() {
        try? modelContext.save()
    }

    // SwiftData change-tracking metadata scales per-insert. Dropping tracked
    // objects after a batch save keeps the hot path O(1) instead of O(n).
    // Any pHash first/second-sighting pair split across a reset boundary is
    // rescued by runDuplicateDetection's post-scan consistency sweep.
    func resetAfterSave() {
        pHashIndexDirty = true
        recordByID.removeAll(keepingCapacity: true)
        modelContext.rollback()
    }

    // MARK: - Scan lifecycle

    func wipeForNewScan(folderPath: String) {
        // Surface schema-migration failures instead of swallowing them — stale
        // records leaking into the next scan was a silent data-corruption bug.
        do { try modelContext.delete(model: FileRecord.self) } catch {
            NSLog("FileIDDataStore.wipeForNewScan: delete FileRecord failed: \(error.localizedDescription)")
        }
        do { try modelContext.delete(model: PersonRecord.self) } catch {
            NSLog("FileIDDataStore.wipeForNewScan: delete PersonRecord failed: \(error.localizedDescription)")
        }
        do { try modelContext.delete(model: ScanSession.self) } catch {
            NSLog("FileIDDataStore.wipeForNewScan: delete ScanSession failed: \(error.localizedDescription)")
        }
        pHashIndex.removeAll(keepingCapacity: true)
        recordByID.removeAll(keepingCapacity: true)
        pHashIndexDirty = false
        let session = ScanSession(folderPath: folderPath)
        modelContext.insert(session)
        do { try modelContext.save() } catch {
            NSLog("FileIDDataStore.wipeForNewScan: save failed: \(error.localizedDescription)")
        }
    }

    // Strips trashed URLs from every PersonRecord so PeopleView thumbnails
    // don't point at dead files.
    func reconcilePersonSamples(removed: [URL]) {
        guard !removed.isEmpty else { return }
        let removedSet = Set(removed)
        let desc = FetchDescriptor<PersonRecord>()
        guard let identities = try? modelContext.fetch(desc) else { return }
        var changed = false
        for person in identities {
            let filtered = person.sampleFileURLs.filter { !removedSet.contains($0) }
            if filtered.count != person.sampleFileURLs.count {
                person.sampleFileURLs = filtered
                changed = true
            }
        }
        if changed {
            do { try modelContext.save() } catch {
                NSLog("FileIDDataStore.reconcilePersonSamples: save failed: \(error.localizedDescription)")
            }
        }
    }

    func markScanSessionComplete() {
        let desc = FetchDescriptor<ScanSession>(predicate: #Predicate { $0.completedAt == nil })
        if let s = try? modelContext.fetch(desc).first {
            s.completedAt = Date()
            try? modelContext.save()
        }
        recordByID.removeAll()
    }

    // MARK: - Per-result insert (scan hot path)

    @discardableResult
    func insertScanResult(
        fileURL: URL,
        creationDate: Date?,
        fileSizeBytes: Int?,
        tags: [String],
        cameraModel: String?,
        locationString: String?,
        hasFaces: Bool,
        pHashValue: UInt64,
        aestheticScore: Double,
        clipEmbedding: Data?,
        failed: Bool,
        facePrintsData: [Data]
    ) -> UUID {
        let r = FileRecord(
            url: fileURL,
            status: failed ? .failed : .namingRequired,
            creationDate: creationDate,
            fileSizeBytes: fileSizeBytes
        )
        r.aiTags         = tags
        r.cameraModel    = cameraModel
        r.locationString = locationString
        r.hasFaces       = hasFaces
        r.pHashValue     = pHashValue
        r.aestheticScore = aestheticScore
        r.clipEmbedding  = clipEmbedding

        // Inline score so Cleanup tab updates mid-scan; `scoreJunkAll` runs
        // post-scan as a consistency pass after any field changes.
        let (junk, reasons) = JunkScorer.score(r)
        r.junkScore   = junk
        r.junkReasons = reasons

        // pHash == 0 means no hash (video, non-image, failed read). First
        // sighting stages a group UUID without writing; second sighting
        // backfills both records.
        if pHashValue != 0 {
            if var bucket = pHashIndex[pHashValue] {
                bucket.count += 1
                pHashIndex[pHashValue] = bucket
                r.duplicateGroupUUID = bucket.groupID
                if bucket.count == 2 {
                    recordByID[bucket.firstRecordID]?.duplicateGroupUUID = bucket.groupID
                }
            } else {
                pHashIndex[pHashValue] = (groupID: UUID(), firstRecordID: r.id, count: 1)
            }
        }

        modelContext.insert(r)
        recordByID[r.id] = r

        // Face prints live outside SwiftData to avoid loading large blobs on
        // every record fetch. Keyed by the new record's id.
        if !facePrintsData.isEmpty {
            FacePrintCache.store(r.id, prints: facePrintsData)
        }
        return r.id
    }

    /// Called by folder-watcher single-file path. Returns the new record's id
    /// if inserted, `nil` if the URL was already present.
    func insertSingleNewResult(
        fileURL: URL,
        tags: [String],
        hasFaces: Bool,
        aestheticScore: Double,
        facePrintsData: [Data]
    ) -> UUID? {
        let path = fileURL.path
        let desc = FetchDescriptor<FileRecord>(predicate: #Predicate { $0.url.path == path })
        if let existing = try? modelContext.fetch(desc), !existing.isEmpty { return nil }

        let r = FileRecord(url: fileURL, status: .namingRequired)
        r.aiTags         = tags
        r.hasFaces       = hasFaces
        r.aestheticScore = aestheticScore
        modelContext.insert(r)
        if !facePrintsData.isEmpty {
            FacePrintCache.store(r.id, prints: facePrintsData)
        }
        try? modelContext.save()
        return r.id
    }

    // MARK: - Naming / review / apply

    func generateProposedNames(
        saveEvery: Int = 500,
        tagger: @Sendable (_ original: String, _ tags: [String]) -> String,
        onProgress: (@Sendable (Int, Int) -> Void)? = nil
    ) {
        let descriptor = FetchDescriptor<FileRecord>(
            predicate: #Predicate { $0.statusValue == "namingRequired" },
            sortBy: [SortDescriptor(\.creationDate)]
        )
        guard let files = try? modelContext.fetch(descriptor), !files.isEmpty else {
            onProgress?(0, 0)
            return
        }
        let total = files.count
        onProgress?(0, total)
        for (i, file) in files.enumerated() {
            file.proposedFilename = tagger(file.filename, file.aiTags)
            file.status = .reviewRequired
            if (i + 1) % saveEvery == 0 {
                try? modelContext.save()
                onProgress?(i + 1, total)
            }
        }
        try? modelContext.save()
        onProgress?(total, total)
    }

    struct RenamePlan: Sendable {
        let srcPath: String
        let dstPath: String
        let tags: [String]
        let doRename: Bool
        let doEXIF: Bool
    }

    func applyRenames(doRename: Bool, doEXIF: Bool) -> [RenamePlan] {
        let descriptor = FetchDescriptor<FileRecord>(
            predicate: #Predicate { $0.statusValue == "reviewRequired" }
        )
        guard let files = try? modelContext.fetch(descriptor) else { return [] }
        var plans: [RenamePlan] = []
        for file in files {
            guard file.isSelectedForRename else { file.status = .completed; continue }
            let src = file.url
            let dst = doRename
                ? src.deletingLastPathComponent()
                     .appendingPathComponent(file.proposedFilename ?? file.filename)
                : src
            plans.append(.init(
                srcPath: src.path, dstPath: dst.path,
                tags: file.aiTags, doRename: doRename, doEXIF: doEXIF
            ))
            file.url = dst
            file.filename = dst.lastPathComponent
            file.status = .completed
        }
        try? modelContext.save()
        return plans
    }

    // MARK: - Paginated iteration helpers

    // Stream all FileRecord rows (respecting an optional predicate) in
    // chunks. Replaces `fetch(FetchDescriptor<FileRecord>())` which loads
    // the whole table — at 58 K rows × ~10 KB/row that's ~580 MB resident
    // and was a major memory-pressure trigger.
    private func forEachFileRecord(
        chunkSize: Int = 1_000,
        predicate: Predicate<FileRecord>? = nil,
        _ body: ([FileRecord]) -> Void
    ) {
        var offset = 0
        while true {
            var d = FetchDescriptor<FileRecord>(predicate: predicate)
            d.fetchLimit  = chunkSize
            d.fetchOffset = offset
            d.sortBy = [SortDescriptor(\.id)]
            guard let chunk = try? modelContext.fetch(d), !chunk.isEmpty else { return }
            body(chunk)
            if chunk.count < chunkSize { return }
            offset += chunk.count
        }
    }

    private func forEachPersonRecord(
        chunkSize: Int = 500,
        _ body: ([PersonRecord]) -> Void
    ) {
        var offset = 0
        while true {
            var d = FetchDescriptor<PersonRecord>()
            d.fetchLimit  = chunkSize
            d.fetchOffset = offset
            d.sortBy = [SortDescriptor(\.id)]
            guard let chunk = try? modelContext.fetch(d), !chunk.isEmpty else { return }
            body(chunk)
            if chunk.count < chunkSize { return }
            offset += chunk.count
        }
    }

    // MARK: - Duplicate detection

    // Consistency sweep only — groups are already authoritative from the
    // incremental `pHashIndex` path unless something dirtied it.
    func runDuplicateDetection() {
        guard pHashIndexDirty else { return }
        // Build pHash buckets by streaming in chunks — previously fetched
        // the entire FileRecord table into one array (~580 MB at 58 K rows).
        var buckets: [UInt64: [FileRecord]] = [:]
        forEachFileRecord { chunk in
            for file in chunk where file.pHashValue != 0 {
                buckets[file.pHashValue, default: []].append(file)
            }
        }
        var dirty = false
        for (_, bucket) in buckets where bucket.count > 1 {
            let gid = bucket.compactMap { $0.duplicateGroupUUID }.first ?? UUID()
            for file in bucket where file.duplicateGroupUUID != gid {
                file.duplicateGroupUUID = gid
                dirty = true
            }
        }
        if dirty { try? modelContext.save() }
        pHashIndexDirty = false
    }

    // MARK: - Junk scoring

    func scoreJunkAll(
        pageSize: Int = 500,
        scorer: @Sendable (FileRecord) -> (Double, [String]),
        onProgress: (@Sendable (Int, Int) -> Void)? = nil
    ) {
        let total = (try? modelContext.fetchCount(FetchDescriptor<FileRecord>())) ?? 0
        var offset = 0
        onProgress?(0, total)
        while true {
            var descriptor = FetchDescriptor<FileRecord>()
            descriptor.fetchLimit = pageSize
            descriptor.fetchOffset = offset
            guard let files = try? modelContext.fetch(descriptor), !files.isEmpty else { break }
            for file in files {
                let (s, r) = scorer(file)
                file.junkScore = s
                file.junkReasons = r
            }
            try? modelContext.save()
            offset += files.count
            onProgress?(offset, total)
            if files.count < pageSize { break }
        }
    }

    // MARK: - Report export

    struct ReportSnapshot: Sendable {
        let fileCount: Int
        let peopleCount: Int
        let duplicateGroupCount: Int
        let trashedCount: Int
        let totalMB: Double
        let reclaimMB: Double
        let categories: [(String, Int)]
    }

    func reportSnapshot(categoryFor: @Sendable (FileRecord) -> String) -> ReportSnapshot {
        // Stream both tables in chunks to avoid loading them entirely into
        // RAM on large libraries.
        var fileCount    = 0
        var trashedCount = 0
        var totalMB      = 0.0
        var reclaimMB    = 0.0
        var dupeGroups   = Set<UUID>()
        var cats: [String: Int] = [:]
        forEachFileRecord { chunk in
            for f in chunk {
                fileCount += 1
                totalMB   += f.fileSizeMB
                if f.isTrashed {
                    trashedCount += 1
                    reclaimMB    += f.fileSizeMB
                }
                if let g = f.duplicateGroupUUID { dupeGroups.insert(g) }
                cats[categoryFor(f), default: 0] += 1
            }
        }
        var peopleCount = 0
        forEachPersonRecord { chunk in peopleCount += chunk.count }

        let sortedCats = cats.sorted { $0.value > $1.value }.map { ($0.key, $0.value) }

        return ReportSnapshot(
            fileCount: fileCount,
            peopleCount: peopleCount,
            duplicateGroupCount: dupeGroups.count,
            trashedCount: trashedCount,
            totalMB: totalMB,
            reclaimMB: reclaimMB,
            categories: sortedCats
        )
    }

    // MARK: - Deep Analyze

    // `fullSweep = false` limits to documents + screenshot-ish images;
    // `true` returns every un-analyzed non-trashed record.
    //
    // Paginated across `forEachFileRecord` so we don't load the entire
    // FileRecord table into RAM just to filter it — the old implementation
    // was the last remaining unbounded fetch that could OOM a 58 K library.
    func deepAnalyzeTargets(fullSweep: Bool) -> [DeepAnalyzeTarget] {
        let docExts: Set<String>  = FileTypes.documents
        let docTags: Set<String>  = ["Document","Screenshot","Receipt","Text","Presentation","Invoice","Taxes"]

        var out: [DeepAnalyzeTarget] = []
        let predicate = #Predicate<FileRecord> {
            $0.isTrashed == false && $0.deepAnalysis == nil
        }
        forEachFileRecord(predicate: predicate) { chunk in
            for f in chunk {
                guard f.status != .failed, f.status != .pending else { continue }
                if fullSweep {
                    out.append(DeepAnalyzeTarget(id: f.id, url: f.url))
                    continue
                }
                let ext = f.url.pathExtension.lowercased()
                if docExts.contains(ext) || !Set(f.aiTags).isDisjoint(with: docTags) {
                    out.append(DeepAnalyzeTarget(id: f.id, url: f.url))
                }
            }
        }
        return out
    }

    // Paginated variant: `deepAnalysis == nil` shrinks as rows are marked, so
    // a fresh offset-0 fetch each call yields a natural streaming cursor.
    // Do not hold FileRecord objects across chunks.
    func deepAnalyzeTargetIDs(fullSweep: Bool, limit: Int) -> [DeepAnalyzeTarget] {
        var desc = FetchDescriptor<FileRecord>(
            predicate: #Predicate<FileRecord> {
                $0.isTrashed == false && $0.deepAnalysis == nil
            },
            sortBy: [SortDescriptor(\.creationDate)]
        )
        desc.fetchLimit = fullSweep ? limit : limit * 4
        guard let files = try? modelContext.fetch(desc) else { return [] }

        let docExts: Set<String>  = FileTypes.documents
        let docTags: Set<String>  = ["Document","Screenshot","Receipt","Text","Presentation","Invoice","Taxes"]

        var out: [DeepAnalyzeTarget] = []
        out.reserveCapacity(limit)
        for f in files {
            if out.count >= limit { break }
            guard f.status != .failed, f.status != .pending else { continue }
            if fullSweep {
                out.append(DeepAnalyzeTarget(id: f.id, url: f.url))
                continue
            }
            let ext = f.url.pathExtension.lowercased()
            if docExts.contains(ext) || !Set(f.aiTags).isDisjoint(with: docTags) {
                out.append(DeepAnalyzeTarget(id: f.id, url: f.url))
            }
        }
        return out
    }

    // Best-effort count for progress reporting. Avoid calling in a hot loop.
    func deepAnalyzeTargetCount(fullSweep: Bool) -> Int {
        if fullSweep {
            let desc = FetchDescriptor<FileRecord>(
                predicate: #Predicate<FileRecord> {
                    $0.isTrashed == false && $0.deepAnalysis == nil
                }
            )
            return (try? modelContext.fetchCount(desc)) ?? 0
        }
        return deepAnalyzeTargets(fullSweep: false).count
    }

    func setDeepAnalysis(recordID: UUID, text: String) {
        let desc = FetchDescriptor<FileRecord>(
            predicate: #Predicate<FileRecord> { $0.id == recordID }
        )
        guard let record = try? modelContext.fetch(desc).first else { return }
        record.deepAnalysis = text
        try? modelContext.save()
    }

    // MARK: - Pagination (Library grid)

    struct PageFetch: Sendable {
        let ids: [UUID]
        let hasMore: Bool
    }

    func fetchPage(
        offset: Int,
        pageSize: Int,
        sortByAesthetic: Bool,
        mediaTab: Bool,
        query: String
    ) -> PageFetch {
        // `\.id` tiebreak keeps pagination stable when the primary sort key ties.
        var descriptor = FetchDescriptor<FileRecord>(
            sortBy: sortByAesthetic
                ? [SortDescriptor(\.aestheticScore, order: .reverse), SortDescriptor(\.id)]
                : [SortDescriptor(\.creationDate, order: .reverse), SortDescriptor(\.id)]
        )
        descriptor.fetchLimit  = pageSize
        descriptor.fetchOffset = offset
        guard let fetched = try? modelContext.fetch(descriptor) else {
            return PageFetch(ids: [], hasMore: false)
        }
        let mediaExts = FileTypes.images.union(FileTypes.videos)
        let docExts   = FileTypes.documents
        let q = query.lowercased()
        let filtered = fetched.filter { file in
            let ext = file.url.pathExtension.lowercased()
            let matchesTab = mediaTab ? mediaExts.contains(ext) : docExts.contains(ext)
            guard matchesTab else { return false }
            guard !q.isEmpty else { return true }
            return file.filename.lowercased().contains(q)
                || file.aiTags.contains(where: { $0.lowercased().contains(q) })
                || (file.cameraModel?.lowercased().contains(q) ?? false)
                || (file.locationString?.lowercased().contains(q) ?? false)
        }
        return PageFetch(ids: filtered.map { $0.id }, hasMore: !fetched.isEmpty)
    }
}
