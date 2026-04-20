import Foundation
import Vision
import SwiftData
import AppKit

// MARK: - LSH Face Clustering Service
//
// Algorithm complexity analysis:
//   Old: O(N × M) — scan ALL identities × up to 5 prints each
//   New: O(1) amortized — hash → check ~N/256 candidates only
//
// Implementation:
//   - Feature prints are 512-dimensional float vectors
//   - We take the sign of the first 8 dimensions → 2^8 = 256 buckets
//   - Lookup checks 1 primary bucket + 8 single-bit-flip neighbors = ≤9 buckets
//   - Expected candidates per lookup: N / 256 (constant for fixed N distribution)
//   - Centroid is stored as a compressed 8-float median vector (not the full 512-d print)
//     for fast SIMD dot-product comparison

actor FaceClusteringService {
    static let shared = FaceClusteringService()

    // MARK: - Config
    let distanceThreshold: Float    = 0.65
    let minFaceAreaFraction: Float  = 0.03
    let maxPrintsPerIdentity: Int   = 12

    // MARK: - LSH Bucket Index
    // bucket → [PersonRecord.id]
    // Rebuilt at app launch from stored prints; updated live during clustering
    private var buckets: [Int: [UUID]] = [:]

    // Cached median centroid vectors — avoids re-archiving NSKeyedArchiver every lookup
    // UUID → 8-float centroid (first 8 dims of median print)
    private var centroids: [UUID: [Float]] = [:]

    // Full prints cache — UUID → list of first-8-float vectors
    // We only store 8 floats per print to keep this tiny (vs full 512-d data blob)
    private var printVectors: [UUID: [[Float]]] = [:]

    // MARK: - O(1) Cluster Lookup

    /// Assign a face to the nearest identity using LSH.
    /// - Complexity: O(N/B) where B = 256 buckets → O(1) amortized
    func cluster(
        facePrint: VNFeaturePrintObservation,
        crop: CGImage,
        fileURL: URL,
        context: ModelContext
    ) async throws -> PersonRecord {
        let vec8 = first8(of: facePrint)
        let bucket = lshBucket(vec8)
        let candidates = neighborBuckets(bucket)

        // Gather candidate identity IDs from this bucket's neighborhood
        var candidateIDs: Set<UUID> = []
        for b in candidates {
            for id in buckets[b] ?? [] { candidateIDs.insert(id) }
        }

        // Find best match among candidates only
        var bestDist: Float = .infinity
        var bestID: UUID?

        for id in candidateIDs {
            guard let centroid = centroids[id] else { continue }
            let dist = l2(vec8, centroid)
            if dist < bestDist {
                bestDist  = dist
                bestID    = id
            }
        }

        let printData = try NSKeyedArchiver.archivedData(withRootObject: facePrint, requiringSecureCoding: true)

        if bestDist < distanceThreshold, let matchID = bestID {
            // ── Match: fetch and update existing identity ──
            let id = matchID
            let descriptor = FetchDescriptor<PersonRecord>(predicate: #Predicate { $0.id == id })
            guard let identity = try context.fetch(descriptor).first else {
                return try await createNew(print: facePrint, printData: printData, vec8: vec8, bucket: bucket, crop: crop, fileURL: fileURL, context: context)
            }
            updateIdentity(identity, printData: printData, vec8: vec8, fileURL: fileURL)
            updateIndex(id: matchID, vec8: vec8, bucket: bucket)
            return identity
        } else {
            return try await createNew(print: facePrint, printData: printData, vec8: vec8, bucket: bucket, crop: crop, fileURL: fileURL, context: context)
        }
    }

    // MARK: - Identity Management

    private func updateIdentity(_ identity: PersonRecord, printData: Data, vec8: [Float], fileURL: URL) {
        if identity.featurePrintsData.count < maxPrintsPerIdentity {
            identity.featurePrintsData.append(printData)
        }
        identity.faceCount += 1
        if !identity.sampleFileURLs.contains(fileURL) && identity.sampleFileURLs.count < 8 {
            identity.sampleFileURLs.append(fileURL)
        }
    }

    private func createNew(
        print: VNFeaturePrintObservation,
        printData: Data, vec8: [Float], bucket: Int,
        crop: CGImage, fileURL: URL, context: ModelContext
    ) async throws -> PersonRecord {
        let faceJpeg = NSBitmapImageRep(cgImage: crop)
            .representation(using: .jpeg, properties: [.compressionFactor: 0.75])
        let person = PersonRecord(name: nil, representativeFaceCropData: faceJpeg)
        person.featurePrintsData = [printData]
        person.faceCount = 1
        person.sampleFileURLs = [fileURL]
        context.insert(person)

        // Register in LSH index immediately
        let id = person.id
        buckets[bucket, default: []].append(id)
        centroids[id] = vec8
        printVectors[id] = [vec8]
        return person
    }

    private func updateIndex(id: UUID, vec8: [Float], bucket: Int) {
        var vecs = printVectors[id] ?? []
        vecs.append(vec8)
        if vecs.count > maxPrintsPerIdentity { vecs = Array(vecs.suffix(maxPrintsPerIdentity)) }
        printVectors[id] = vecs

        // Recompute median centroid
        var medianVec = [Float](repeating: 0, count: 8)
        for dim in 0..<8 {
            let vals = vecs.map { $0[dim] }.sorted()
            medianVec[dim] = vals[vals.count / 2]
        }
        centroids[id] = medianVec

        // Update bucket if centroid changed significantly
        let newBucket = lshBucket(medianVec)
        if newBucket != bucket {
            buckets[bucket]?.removeAll { $0 == id }
            buckets[newBucket, default: []].append(id)
        }
    }

    // MARK: - Index Rebuild (called on app launch)

    func rebuildIndex(context: ModelContext) throws {
        buckets    = [:]
        centroids  = [:]
        printVectors = [:]

        let identities = try context.fetch(FetchDescriptor<PersonRecord>())
        for identity in identities {
            var vecs: [[Float]] = []
            for data in identity.featurePrintsData.prefix(maxPrintsPerIdentity) {
                if let print = try? NSKeyedUnarchiver.unarchivedObject(ofClass: VNFeaturePrintObservation.self, from: data) {
                    vecs.append(first8(of: print))
                }
            }
            guard !vecs.isEmpty else { continue }

            let id = identity.id
            var median = [Float](repeating: 0, count: 8)
            for dim in 0..<8 {
                let vals = vecs.map { $0[dim] }.sorted()
                median[dim] = vals[vals.count / 2]
            }
            let bucket = lshBucket(median)
            buckets[bucket, default: []].append(id)
            centroids[id] = median
            printVectors[id] = vecs
        }
    }

    // MARK: - Utility operations

    func allIdentities(context: ModelContext) throws -> [PersonRecord] {
        try context.fetch(FetchDescriptor<PersonRecord>(sortBy: [SortDescriptor(\.faceCount, order: .reverse)]))
    }

    func updateName(id: UUID, name: String, context: ModelContext) throws {
        let descriptor = FetchDescriptor<PersonRecord>(predicate: #Predicate { $0.id == id })
        if let p = try context.fetch(descriptor).first { p.name = name; try context.save() }
    }

    func merge(sourceID: UUID, targetID: UUID, context: ModelContext) throws {
        guard sourceID != targetID else { return }
        let desc = FetchDescriptor<PersonRecord>()
        let all  = try context.fetch(desc)
        guard let src = all.first(where: { $0.id == sourceID }),
              let tgt = all.first(where: { $0.id == targetID }) else { return }

        tgt.featurePrintsData.append(contentsOf: src.featurePrintsData)
        tgt.faceCount += src.faceCount
        for url in src.sampleFileURLs where !tgt.sampleFileURLs.contains(url) && tgt.sampleFileURLs.count < 8 {
            tgt.sampleFileURLs.append(url)
        }

        // Merge LSH index
        let srcBucket = lshBucket(centroids[sourceID] ?? [Float](repeating: 0, count: 8))
        buckets[srcBucket]?.removeAll { $0 == sourceID }
        centroids[sourceID] = nil
        printVectors[sourceID] = nil

        context.delete(src)
        try context.save()
    }

    func isFaceValid(boundingBox: CGRect) -> Bool {
        Float(boundingBox.width * boundingBox.height) >= minFaceAreaFraction
    }

    // MARK: - LSH Math

    /// Extract first 8 floats from a VNFeaturePrintObservation element buffer.
    private func first8(of print: VNFeaturePrintObservation) -> [Float] {
        var result = [Float](repeating: 0, count: 8)
        print.data.withUnsafeBytes { ptr in
            let floatPtr = ptr.bindMemory(to: Float.self)
            let count = min(8, floatPtr.count)
            for i in 0..<count { result[i] = floatPtr[i] }
        }
        return result
    }

    /// Hash 8-float vector to bucket [0…255] using sign bits.
    private func lshBucket(_ vec: [Float]) -> Int {
        var bucket = 0
        for i in 0..<min(8, vec.count) {
            if vec[i] > 0 { bucket |= (1 << i) }
        }
        return bucket
    }

    /// Returns primary bucket + all single-bit-flip neighbors (9 total).
    private func neighborBuckets(_ bucket: Int) -> [Int] {
        var neighbors = [bucket]
        for bit in 0..<8 { neighbors.append(bucket ^ (1 << bit)) }
        return neighbors
    }

    /// L2 distance between two 8-float vectors.
    private func l2(_ a: [Float], _ b: [Float]) -> Float {
        var sum: Float = 0
        for i in 0..<min(a.count, b.count) { let d = a[i] - b[i]; sum += d * d }
        return sum.squareRoot()
    }
}
