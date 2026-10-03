import Foundation
import CryptoKit
import GRDB
import FileIDShared

actor CatalogVectorIndex {
    struct Match: Sendable, Equatable {
        let fileID: Int64
        let cosine: Float
        let fingerprint: String
    }

    struct Revision: Codable, Sendable, Equatable {
        let instance: String
        let generation: Int64
        let nonce: String
        var token: String { "\(instance):\(generation):\(nonce)" }
    }

    enum IndexError: Error { case capacityExceeded, corruptCache, invalidQuery }

    private struct Entry: Codable, Sendable {
        let fileID: Int64
        let fingerprint: String
    }

    private struct Manifest: Codable {
        let model: String
        let dimension: Int
        let revision: Revision
        let graphHash: String
        let entries: [Entry]
    }

    // Other awaiters can outlive a cache transfer to the next worker.
    private struct Prepared: Sendable {
        let cache: Cache
        let revision: Revision
    }

    // A cache is transferred to its owning actor after the worker finishes.
    private final class Cache: @unchecked Sendable {
        var graph: HNSWIndex
        var entries: [Entry]
        var nodesByFile: [Int64: Int32]
        var revision: Revision
        init(graph: HNSWIndex, entries: [Entry], revision: Revision) throws {
            self.graph = graph
            self.entries = entries
            self.revision = revision
            var lookup: [Int64: Int32] = [:]
            guard graph.dim == CLIPEmbeddingSpace.dimension, graph.rawCount == entries.count else { throw IndexError.corruptCache }
            for (id, entry) in entries.enumerated() {
                guard entry.fileID > 0, entry.fingerprint.utf8.count == 64,
                      entry.fingerprint.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else { throw IndexError.corruptCache }
                if graph.isActive(id: Int32(id)) {
                    guard lookup.updateValue(Int32(id), forKey: entry.fileID) == nil else { throw IndexError.corruptCache }
                }
            }
            self.nodesByFile = lookup
        }
    }

    private let pool: DatabasePool
    private let directory: URL
    private let maximumFiles: Int
    private let memoryInfo: @Sendable () -> (total: UInt64, available: UInt64)
    private var cache: Cache?
    private var worker: Task<Prepared, Error>?
    private var workerID: UUID?
    private(set) var failed = false

    init(pool: DatabasePool, directory: URL, maximumFiles: Int = 200_000,
         memoryInfo: @escaping @Sendable () -> (total: UInt64, available: UInt64) = {
             (ProcessInfo.processInfo.physicalMemory / 1_048_576, UInt64(max(0, Hardware.availableMemoryMB())))
         }) {
        self.pool = pool
        self.directory = directory
        self.maximumFiles = maximumFiles
        self.memoryInfo = memoryInfo
    }

    func synchronize() async throws -> Revision {
        if worker == nil {
            guard directory.isFileURL else { throw IndexError.corruptCache }
            try ReadOnlyLocations.requireWritable(directory)
            let previous = cache
            cache = nil
            let pool = self.pool
            let directory = self.directory
            let maximumFiles = self.maximumFiles
            let memoryInfo = self.memoryInfo
            let id = UUID()
            workerID = id
            worker = Task.detached(priority: .utility) { () throws -> Prepared in
                try Self.prepareWorker(previous, pool: pool, directory: directory, maximumFiles: maximumFiles, memoryInfo: memoryInfo)
            }
        }
        let id = workerID
        let active = worker!
        do {
            let result = try await active.value
            if id == workerID {
                cache = result.cache
                worker = nil
                workerID = nil
                failed = false
            }
            return result.revision
        } catch {
            if id == workerID { worker = nil; workerID = nil; failed = true }
            throw error
        }
    }

    func matches(_ query: [Float], limit: Int) throws -> [Match] {
        guard query.count == CLIPEmbeddingSpace.dimension, query.allSatisfy(\.isFinite), (1...1000).contains(limit) else { throw IndexError.invalidQuery }
        let norm = sqrt(query.reduce(0.0) { $0 + Double($1) * Double($1) })
        guard abs(norm * norm - 1) <= 0.02 else { throw IndexError.invalidQuery }
        guard let cache else { return [] }
        let normalized = query.map { $0 / Float(norm) }
        return cache.graph.search(normalized, k: limit, ef: max(256, limit * 2)).map { node, distance in
            let entry = cache.entries[Int(node)]
            return Match(fileID: entry.fileID, cosine: max(-1, min(1, 1 - distance * distance / 2)), fingerprint: entry.fingerprint)
        }
    }

    func currentRevision() -> Revision? { cache?.revision }

    func recover() async throws { try await CatalogIndexJob.recover(pool) }

    func control(_ request: CatalogRequest) async -> CatalogResponse {
        do {
            guard request.jobID == CatalogIndexJob.id, !request.requestID.isEmpty, request.requestID.count <= 200 else { throw CatalogStore.InvalidRequest() }
            guard ["pauseJob", "resumeJob", "cancelJob"].contains(request.action) else { throw CatalogStore.InvalidRequest() }
            if request.action == "resumeJob" {
                let state = try await pool.read { db in try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=? AND kind=?", arguments: [CatalogIndexJob.id,CatalogIndexJob.kind]) }
                guard let state, ["paused", "failed", "cancelled"].contains(state) else { throw CatalogStore.InvalidRequest() }
                if worker != nil { _ = try? await synchronize() }
            }
            try await pool.write { db in
                let state = try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=? AND kind=?", arguments: [CatalogIndexJob.id,CatalogIndexJob.kind])
                let allowed = request.action == "resumeJob" ? ["paused", "failed", "cancelled"] : request.action == "pauseJob" ? ["queued", "running"] : ["queued", "running", "paused"]
                guard let state, allowed.contains(state) else { throw CatalogStore.InvalidRequest() }
                let target = request.action == "resumeJob" ? "queued" : request.action == "pauseJob" ? "paused" : "cancelled"
                try db.execute(sql: "UPDATE catalog_jobs SET state=?,error=NULL,updated_at=? WHERE id=?", arguments: [target,Date().timeIntervalSince1970,CatalogIndexJob.id])
            }
            if request.action == "resumeJob" { failed = false; prepare() }
            else {
                failed = true
                worker?.cancel()
                if worker != nil { _ = try? await synchronize() }
                failed = true
            }
            let jobs = try await pool.read { db in try CatalogStore.jobs(db) }
            return CatalogResponse(requestID: request.requestID, status: "ok", jobs: jobs)
        } catch {
            return CatalogResponse(requestID: request.requestID, status: "error", message: error.localizedDescription)
        }
    }

    func refreshedMatches(_ query: [Float], limit: Int) async throws -> [Match] {
        repeat {
            try Task.checkCancellation()
            _ = try await synchronize()
        } while cache == nil
        try Task.checkCancellation()
        return try matches(query, limit: limit)
    }

    func prepare() {
        guard worker == nil, !failed else { return }
        Task { _ = try? await synchronize() }
    }

    private static func prepareWorker(_ previous: Cache?, pool: DatabasePool, directory: URL,
                                      maximumFiles: Int, memoryInfo: @Sendable () -> (total: UInt64, available: UInt64)) throws -> Prepared {
        try Task.checkCancellation()
        let latest = try pool.read { try Self.revision($0) }
        if let previous, previous.revision == latest {
            try CatalogIndexJob.finishCached(pool)
            return Prepared(cache: previous, revision: latest)
        }
        try CatalogIndexJob.begin(pool)
        do {
            let count = try pool.read { db in
                try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.model=? AND f.failed=0", arguments: [CLIPEmbeddingSpace.modelID]) ?? 0
            }
            let memory = memoryInfo()
            let (graphURL, manifestURL) = Self.paths(directory)
            let graphBytes = max(0, (try? graphURL.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
            let manifestBytes = max(0, (try? manifestURL.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
            guard graphBytes <= 512 * 1024 * 1024, manifestBytes <= 64 * 1024 * 1024 else { throw IndexError.capacityExceeded }
            let rawCount: Int = max(count, previous?.graph.rawCount ?? 0)
            let vectorMegabytes = min(250_000, rawCount) * 9 / 1024
            let diskMegabytes = (graphBytes * 3 + manifestBytes * 2) / 1_048_576
            let requested = UInt64(max(vectorMegabytes, diskMegabytes) + 64)
            if ModelMemoryAdmission.rejection(totalMB: memory.total, availableMB: memory.available, requestedMB: requested) != nil {
                throw CatalogIndexJob.AdmissionDeferred(message: "Search index preparation needs about \(requested) MB plus system headroom. Free memory and resume its job in Tools.")
            }
            let cache = try Self.synchronize(previous, pool: pool, directory: directory, maximumFiles: maximumFiles)
            try CatalogIndexJob.finish(pool)
            return Prepared(cache: cache, revision: cache.revision)
        } catch {
            try? CatalogIndexJob.finish(pool, error: error)
            throw error
        }
    }

    private static func synchronize(_ previous: Cache?, pool: DatabasePool, directory: URL, maximumFiles: Int) throws -> Cache {
        try Task.checkCancellation()
        return try pool.read { db in
            let latest = try revision(db)
            let base = previous ?? (try? load(db, directory: directory, latest: latest))
            try Task.checkCancellation()
            if let base, base.revision == latest { return base }
            let result: Cache
            if let base, try canAdvance(base.revision, to: latest, db: db) {
                result = base
                let rows = try Row.fetchAll(db, sql: "SELECT DISTINCT file_id FROM catalog_vector_changes WHERE namespace='clip' AND generation>? AND generation<=?", arguments: [base.revision.generation, latest.generation])
                try CatalogIndexJob.progress(pool, revision: latest, processed: 0, total: rows.count)
                for (processed, row) in rows.enumerated() {
                    try Task.checkCancellation()
                    if processed % 128 == 0 { try CatalogIndexJob.progress(pool, revision: latest, processed: processed, total: rows.count) }
                    guard let fileID: Int64 = row["file_id"], fileID > 0 else { throw IndexError.corruptCache }
                    if let old = result.nodesByFile.removeValue(forKey: fileID) { result.graph.remove(id: old) }
                    if let blob = try Data.fetchOne(db, sql: "SELECT e.embedding FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.file_id=? AND e.model=? AND f.failed=0", arguments: [fileID, CLIPEmbeddingSpace.modelID]), let vector = normalized(blob) {
                        try append(fileID: fileID, blob: blob, vector: vector, cache: result, maximumFiles: maximumFiles)
                    }
                }
                try compact(result)
                result.revision = latest
            } else {
                result = try rebuild(db, pool: pool, revision: latest, maximumFiles: maximumFiles)
            }
            try Task.checkCancellation()
            try save(result, directory: directory)
            return result
        }
    }

    private static func revision(_ db: GRDB.Database) throws -> Revision {
        guard let row = try Row.fetchOne(db, sql: "SELECT instance_id,generation,nonce FROM catalog_vector_state WHERE namespace='clip'") else { throw IndexError.corruptCache }
        return Revision(instance: row["instance_id"], generation: row["generation"], nonce: row["nonce"])
    }

    private static func canAdvance(_ old: Revision, to latest: Revision, db: GRDB.Database) throws -> Bool {
        guard old.instance == latest.instance, old.generation <= latest.generation else { return false }
        if old.generation == latest.generation { return old.nonce == latest.nonce }
        let checkpoint = old.generation == 0 ? old.instance : try String.fetchOne(db, sql: "SELECT nonce FROM catalog_vector_changes WHERE namespace='clip' AND generation=?", arguments: [old.generation])
        guard checkpoint == old.nonce else { return false }
        let changes = try Int64.fetchOne(db, sql: "SELECT COUNT(*) FROM catalog_vector_changes WHERE namespace='clip' AND generation>? AND generation<=?", arguments: [old.generation, latest.generation]) ?? 0
        return changes == latest.generation - old.generation
    }

    private static func rebuild(_ db: GRDB.Database, pool: DatabasePool, revision: Revision, maximumFiles: Int) throws -> Cache {
        let result = try Cache(graph: HNSWIndex(dim: CLIPEmbeddingSpace.dimension), entries: [], revision: revision)
        let total = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.model=? AND f.failed=0", arguments: [CLIPEmbeddingSpace.modelID]) ?? 0
        var processed = 0
        try CatalogIndexJob.progress(pool, revision: revision, processed: 0, total: total)
        let rows = try Row.fetchCursor(db, sql: "SELECT e.file_id,e.embedding FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.model=? AND f.failed=0 ORDER BY e.file_id", arguments: [CLIPEmbeddingSpace.modelID])
        while let row = try rows.next() {
            try Task.checkCancellation()
            processed += 1
            if processed % 128 == 0 { try CatalogIndexJob.progress(pool, revision: revision, processed: processed, total: total) }
            let blob: Data = row["embedding"]
            if let vector = normalized(blob) { try append(fileID: row["file_id"], blob: blob, vector: vector, cache: result, maximumFiles: maximumFiles) }
        }
        return result
    }

    private static func normalized(_ blob: Data) -> [Float]? {
        guard let vector = CLIPEmbeddingSpace.vector(from: blob) else { return nil }
        let norm = sqrt(vector.reduce(0.0) { $0 + Double($1) * Double($1) })
        return vector.map { $0 / Float(norm) }
    }

    private static func append(fileID: Int64, blob: Data, vector: [Float], cache: Cache, maximumFiles: Int) throws {
        if cache.graph.rawCount >= 250_000 { try compact(cache) }
        guard cache.graph.count < maximumFiles, cache.graph.rawCount < 250_000 else { throw IndexError.capacityExceeded }
        let node = cache.graph.insert(vector)
        guard node >= 0, Int(node) == cache.entries.count else { throw IndexError.corruptCache }
        cache.entries.append(Entry(fileID: fileID, fingerprint: digest(blob)))
        cache.nodesByFile[fileID] = node
    }

    private static func compact(_ cache: Cache) throws {
        guard cache.graph.deletedFraction > 0.2 || cache.graph.rawCount >= 250_000 else { return }
        let mapping = try cache.graph.compact(checkpoint: { try Task.checkCancellation() })
        cache.entries = cache.entries.enumerated().compactMap { old, entry in mapping[Int32(old)].map { (Int($0), entry) } }.sorted { $0.0 < $1.0 }.map(\.1)
        cache.nodesByFile = Dictionary(uniqueKeysWithValues: cache.entries.enumerated().map { ($0.element.fileID, Int32($0.offset)) })
    }

    private static func paths(_ directory: URL) -> (URL, URL) {
        let key = digest(Data((CLIPEmbeddingSpace.modelID + ":512:clip").utf8))
        return (directory.appendingPathComponent(key + ".graph"), directory.appendingPathComponent(key + ".json"))
    }

    private static func save(_ cache: Cache, directory: URL) throws {
        try ReadOnlyLocations.requireWritable(directory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let (graphURL, manifestURL) = paths(directory)
        try ReadOnlyLocations.requireWritable(graphURL)
        try ReadOnlyLocations.requireWritable(manifestURL)
        let graph = try cache.graph.snapshot(modelID: CLIPEmbeddingSpace.modelID, sourceRevision: cache.revision.token)
        let manifest = Manifest(model: CLIPEmbeddingSpace.modelID, dimension: 512, revision: cache.revision, graphHash: digest(graph), entries: cache.entries)
        let metadata = try JSONEncoder().encode(manifest)
        guard metadata.count <= 64 * 1024 * 1024 else { throw IndexError.capacityExceeded }
        try graph.write(to: graphURL, options: .atomic)
        try metadata.write(to: manifestURL, options: .atomic)
    }

    private static func load(_ db: GRDB.Database, directory: URL, latest: Revision) throws -> Cache {
        let (graphURL, manifestURL) = paths(directory)
        let manifest = try JSONDecoder().decode(Manifest.self, from: boundedRead(manifestURL, limit: 64 * 1024 * 1024))
        guard manifest.model == CLIPEmbeddingSpace.modelID, manifest.dimension == 512,
              try canAdvance(manifest.revision, to: latest, db: db) else { throw IndexError.corruptCache }
        let data = try boundedRead(graphURL, limit: 512 * 1024 * 1024)
        guard digest(data) == manifest.graphHash else { throw IndexError.corruptCache }
        let graph = try HNSWIndex.restoreSnapshot(data, modelID: manifest.model, sourceRevision: manifest.revision.token)
        return try Cache(graph: graph, entries: manifest.entries, revision: manifest.revision)
    }

    private static func boundedRead(_ url: URL, limit: Int) throws -> Data {
        guard url.isFileURL else { throw IndexError.corruptCache }
        let properties = try url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
        guard properties.isRegularFile == true, let size = properties.fileSize, size > 0, size <= limit else { throw IndexError.corruptCache }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var data = Data()
        data.reserveCapacity(size)
        while let chunk = try handle.read(upToCount: 1_048_576), !chunk.isEmpty {
            try Task.checkCancellation()
            guard chunk.count <= limit - data.count else { throw IndexError.corruptCache }
            data.append(chunk)
        }
        return data
    }

    static func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
}
