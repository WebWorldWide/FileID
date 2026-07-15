import Foundation

struct ExactCosineEdge: Equatable, Sendable {
    let cosine: Float
    let first: Int32
    let second: Int32
}

enum ExactCosineJoinResult: Sendable {
    case success(edges: [ExactCosineEdge], distanceEvaluations: Int)
    case limitExceeded(reason: String, distanceEvaluations: Int)
}

enum ExactCosineJoin {
    struct Limits: Sendable {
        let distanceEvaluations: Int
        let edges: Int
        let directPairLimit: Int

        init(distanceEvaluations: Int, edges: Int, directPairLimit: Int = 250_000) {
            self.distanceEvaluations = distanceEvaluations
            self.edges = edges
            self.directPairLimit = directPairLimit
        }

        static let autoMerge = Limits(distanceEvaluations: 12_500_000, edges: 1_000_000)
    }

    private final class Node {
        let vantage: Int
        let threshold: Double
        let inner: Node?
        let outer: Node?

        init(vantage: Int, threshold: Double = 0, inner: Node? = nil, outer: Node? = nil) {
            self.vantage = vantage
            self.threshold = threshold
            self.inner = inner
            self.outer = outer
        }
    }

    static func edges(
        vectors: [[Float]],
        small: [Bool],
        tightThreshold: Float,
        smallThreshold: Float,
        limits: Limits = .autoMerge
    ) -> ExactCosineJoinResult {
        guard vectors.count == small.count, vectors.count >= 2,
              vectors.count <= Int(Int32.max), limits.distanceEvaluations > 0,
              limits.edges > 0 else {
            return .success(edges: [], distanceEvaluations: 0)
        }
        let dimension = vectors[0].count
        guard dimension > 0,
              vectors.allSatisfy({ $0.count == dimension && $0.allSatisfy(\.isFinite) }) else {
            return .success(edges: [], distanceEvaluations: 0)
        }

        let pairCount = vectors.count * (vectors.count - 1) / 2
        if pairCount <= limits.directPairLimit {
            var found: [ExactCosineEdge] = []
            found.reserveCapacity(min(pairCount, limits.edges))
            var evaluations = 0
            for i in 0..<vectors.count {
                for j in (i + 1)..<vectors.count {
                    evaluations += 1
                    if evaluations > limits.distanceEvaluations {
                        return .limitExceeded(reason: "distance_evaluations", distanceEvaluations: evaluations)
                    }
                    let cosine = dot(vectors[i], vectors[j])
                    if cosine >= tightThreshold ||
                        (cosine >= smallThreshold && (small[i] || small[j])) {
                        found.append(ExactCosineEdge(
                            cosine: cosine, first: Int32(i), second: Int32(j)))
                        if found.count > limits.edges {
                            return .limitExceeded(reason: "qualifying_edges", distanceEvaluations: evaluations)
                        }
                    }
                }
            }
            sort(&found)
            return .success(edges: found, distanceEvaluations: evaluations)
        }

        var evaluations = 0
        var exceeded = false
        func metric(_ a: Int, _ b: Int) -> Double {
            evaluations += 1
            if evaluations > limits.distanceEvaluations {
                exceeded = true
                return .infinity
            }
            var squared = 0.0
            for k in 0..<dimension {
                let delta = Double(vectors[a][k]) - Double(vectors[b][k])
                squared += delta * delta
            }
            return squared.squareRoot()
        }
        func build(_ indices: [Int]) -> Node? {
            guard !indices.isEmpty, !exceeded else { return nil }
            let vantage = indices[indices.count - 1]
            guard indices.count > 1 else { return Node(vantage: vantage) }
            var measured: [(distance: Double, index: Int)] = []
            measured.reserveCapacity(indices.count - 1)
            for index in indices.dropLast() {
                measured.append((metric(vantage, index), index))
                if exceeded { return nil }
            }
            measured.sort {
                $0.distance != $1.distance
                    ? $0.distance < $1.distance
                    : $0.index < $1.index
            }
            let split = measured.count / 2
            let threshold = measured[split].distance
            let inner = build(measured[..<split].map(\.index))
            let outer = build(measured[split...].map(\.index))
            return Node(vantage: vantage, threshold: threshold, inner: inner, outer: outer)
        }

        guard let root = build(Array(vectors.indices)), !exceeded else {
            return .limitExceeded(reason: "distance_evaluations", distanceEvaluations: evaluations)
        }

        var maxNormSquared = 0.0
        for vector in vectors {
            var squared = 0.0
            for value in vector { squared += Double(value) * Double(value) }
            maxNormSquared = max(maxNormSquared, squared)
        }
        let unitRoundoff = Double(Float.ulpOfOne) / 2
        let operations = Double(dimension * 2)
        let denominator = 1 - operations * unitRoundoff
        let dotRoundingMargin = denominator > 0
            ? operations * unitRoundoff / denominator * maxNormSquared
            : .infinity
        let loose = Double(min(tightThreshold, smallThreshold))
        let radius = max(0, 2 * maxNormSquared - 2 * (loose - dotRoundingMargin)).squareRoot()
        var found: [ExactCosineEdge] = []
        found.reserveCapacity(min(vectors.count * 8, limits.edges))

        func query(_ node: Node?, target: Int) {
            guard let node, !exceeded else { return }
            let distance = metric(target, node.vantage)
            guard !exceeded else { return }
            if node.vantage > target, distance <= radius {
                let cosine = dot(vectors[target], vectors[node.vantage])
                if cosine >= tightThreshold ||
                    (cosine >= smallThreshold && (small[target] || small[node.vantage])) {
                    found.append(ExactCosineEdge(
                        cosine: cosine, first: Int32(target), second: Int32(node.vantage)))
                    if found.count > limits.edges {
                        exceeded = true
                        return
                    }
                }
            }
            if node.inner != nil, distance - radius <= node.threshold {
                query(node.inner, target: target)
            }
            if node.outer != nil, distance + radius >= node.threshold {
                query(node.outer, target: target)
            }
        }

        for target in vectors.indices {
            query(root, target: target)
            if exceeded {
                let reason = found.count > limits.edges ? "qualifying_edges" : "distance_evaluations"
                return .limitExceeded(reason: reason, distanceEvaluations: evaluations)
            }
        }
        sort(&found)
        return .success(edges: found, distanceEvaluations: evaluations)
    }

    private static func sort(_ edges: inout [ExactCosineEdge]) {
        edges.sort {
            if $0.cosine != $1.cosine { return $0.cosine > $1.cosine }
            if $0.first != $1.first { return $0.first < $1.first }
            return $0.second < $1.second
        }
    }

    @inline(__always)
    private static func dot(_ a: [Float], _ b: [Float]) -> Float {
        var sum: Float = 0
        for index in a.indices { sum += a[index] * b[index] }
        return sum
    }
}
