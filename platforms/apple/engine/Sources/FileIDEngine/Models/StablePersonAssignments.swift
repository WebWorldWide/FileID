import Foundation

enum StablePersonAssignments {
    struct Prior: Codable, Sendable {
        let id: Int64
        let faceIDs: [Int64]
    }

    struct Cluster: Codable, Sendable {
        let faceIDs: [Int64]
        let representative: Int64
    }

    enum Failure: Error { case invalidPartition }

    static func resolve(priors: [Prior], clusters: [Cluster], fixed: [Int64?], preserved: Set<Int64>) throws -> [Int64?] {
        guard fixed.count == clusters.count else { throw Failure.invalidPartition }
        var owner: [Int64: Int64] = [:]
        var sizes: [Int64: Int] = [:]
        for prior in priors {
            guard prior.id > 0, sizes[prior.id] == nil else { throw Failure.invalidPartition }
            sizes[prior.id] = prior.faceIDs.count
            for face in prior.faceIDs {
                guard face > 0, owner.updateValue(prior.id, forKey: face) == nil else { throw Failure.invalidPartition }
            }
        }
        var selected = fixed
        var claimed = Set<Int64>()
        var seen = Set<Int64>()
        struct Edge {
            let overlap: Int
            let ownsRepresentative: Bool
            let person: Int64
            let cluster: Int
        }
        var edges: [Edge] = []
        for (index, cluster) in clusters.enumerated() {
            guard !cluster.faceIDs.isEmpty, cluster.faceIDs.contains(cluster.representative) else { throw Failure.invalidPartition }
            var counts: [Int64: Int] = [:]
            for face in cluster.faceIDs {
                guard face > 0, seen.insert(face).inserted else { throw Failure.invalidPartition }
                if let person = owner[face] {
                    guard !preserved.contains(person) else { throw Failure.invalidPartition }
                    counts[person, default: 0] += 1
                }
            }
            if let person = fixed[index] {
                guard sizes[person] != nil, !preserved.contains(person), claimed.insert(person).inserted,
                      counts[person, default: 0] > 0 else { throw Failure.invalidPartition }
            } else {
                for (person, overlap) in counts where overlap >= max(1, (sizes[person, default: 0] + 1) / 2) {
                    edges.append(Edge(overlap: overlap, ownsRepresentative: owner[cluster.representative] == person, person: person, cluster: index))
                }
            }
        }
        edges.sort {
            if $0.overlap != $1.overlap { return $0.overlap > $1.overlap }
            if $0.ownsRepresentative != $1.ownsRepresentative { return $0.ownsRepresentative }
            if $0.person != $1.person { return $0.person < $1.person }
            return $0.cluster < $1.cluster
        }
        for edge in edges where selected[edge.cluster] == nil && !claimed.contains(edge.person) {
            selected[edge.cluster] = edge.person
            claimed.insert(edge.person)
        }
        return selected
    }
}
