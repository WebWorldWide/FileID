import Accelerate
import Foundation
import GRDB

/// Stored BGE document vectors decoded once and kept in memory, keyed on the
/// catalog's `text` vector revision, so a Chat question scores a flat array
/// instead of reading and decoding every row. Scores match the row scan exactly
/// (both use `vDSP_dotpr`). Freed after five idle minutes; catalogs beyond
/// `maxRows` keep the scan.
final class BGEDocumentMatrix: @unchecked Sendable {
    static let shared = BGEDocumentMatrix()
    static let maxRows = 50_000
    static let idleSeconds: UInt64 = 300

    private struct Matrix {
        let key: String
        let dimension: Int
        let ids: [Int64]
        let values: [Float]
    }

    private let lock = NSLock()
    private var matrix: Matrix?
    private var lastUse = Date.distantPast
    private var sweeper: Task<Void, Never>?

    func reset() {
        lock.lock(); matrix = nil; sweeper?.cancel(); sweeper = nil; lock.unlock()
    }

    /// Scored (score, fileID) pairs at or above `floor`, best first, or nil when
    /// the catalog is too large to hold and the caller should scan.
    func scores(_ db: GRDB.Database, wanted: [Float], floor: Float, model: String, limit: Int) throws -> [(Float, Int64)]? {
        let revision = try Row.fetchOne(db, sql: "SELECT instance_id,generation,nonce FROM catalog_vector_state WHERE namespace='text'")
        guard let revision else { return nil }
        let key = "\(db.configuration.label ?? "")|\(revision["instance_id"] as String)|\(revision["generation"] as Int64)|\(revision["nonce"] as String)|\(model)|\(wanted.count)"
        lock.lock()
        let cached = matrix?.key == key ? matrix : nil
        lock.unlock()
        let current: Matrix
        if let cached {
            current = cached
        } else {
            let count = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM text_embeddings WHERE model=? AND length(embedding)=?", arguments: [model, wanted.count * 4]) ?? 0
            guard count <= Self.maxRows else { return nil }
            var ids: [Int64] = []; ids.reserveCapacity(count)
            var values: [Float] = []; values.reserveCapacity(count * wanted.count)
            let rows = try Row.fetchCursor(db, sql: "SELECT file_id,embedding FROM text_embeddings WHERE model=? AND length(embedding)=? ORDER BY file_id", arguments: [model, wanted.count * 4])
            while let row = try rows.next() {
                let blob: Data = row["embedding"]
                ids.append(row["file_id"])
                blob.withUnsafeBytes { raw in
                    for index in 0..<wanted.count {
                        values.append(Float(bitPattern: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: index * 4, as: UInt32.self))))
                    }
                }
            }
            current = Matrix(key: key, dimension: wanted.count, ids: ids, values: values)
        }
        lock.lock()
        matrix = current
        lastUse = Date()
        if sweeper == nil { sweeper = Task.detached { [weak self] in await self?.sweep() } }
        lock.unlock()
        var scores = [Float](repeating: 0, count: current.ids.count)
        current.values.withUnsafeBufferPointer { values in
            wanted.withUnsafeBufferPointer { query in
                for row in current.ids.indices {
                    vDSP_dotpr(values.baseAddress! + row * current.dimension, 1, query.baseAddress!, 1, &scores[row], vDSP_Length(current.dimension))
                }
            }
        }
        var scored: [(Float, Int64)] = []
        for (row, score) in scores.enumerated() where score.isFinite && score >= floor { scored.append((score, current.ids[row])) }
        return scored.sorted { $0.0 != $1.0 ? $0.0 > $1.0 : $0.1 < $1.1 }.prefix(limit).map { $0 }
    }

    /// One stored vector against the query, with the same arithmetic as the matrix.
    static func dot(_ blob: Data, _ wanted: [Float]) -> Float {
        var vector = [Float](repeating: 0, count: wanted.count)
        blob.withUnsafeBytes { raw in
            for index in 0..<wanted.count { vector[index] = Float(bitPattern: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: index * 4, as: UInt32.self))) }
        }
        var score: Float = 0
        vDSP_dotpr(vector, 1, wanted, 1, &score, vDSP_Length(wanted.count))
        return score
    }

    private func sweep() async {
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 30_000_000_000)
            if releaseIfIdle(now: Date()) { return }
        }
    }

    /// True once the matrix has been freed.
    func releaseIfIdle(now: Date) -> Bool {
        lock.withLock {
            guard now.timeIntervalSince(lastUse) >= Double(Self.idleSeconds) else { return false }
            matrix = nil
            sweeper = nil
            return true
        }
    }

    var isLoaded: Bool { lock.withLock { matrix != nil } }
}
