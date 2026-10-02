import Foundation

enum FaceLandmarkMatch {
    static func index(stored: CGRect, candidates: [CGRect]) -> Int? {
        guard valid(stored) else { return nil }
        let scores = candidates.enumerated().compactMap { index, bounds -> (Int, CGFloat)? in
            guard valid(bounds) else { return nil }
            let intersection = stored.intersection(bounds)
            guard !intersection.isNull else { return nil }
            let area = intersection.width * intersection.height
            let union = stored.width * stored.height + bounds.width * bounds.height - area
            let overlap = area / union
            let dx = abs(stored.midX - bounds.midX) / min(stored.width, bounds.width)
            let dy = abs(stored.midY - bounds.midY) / min(stored.height, bounds.height)
            guard overlap >= 0.5, dx <= 0.35, dy <= 0.35 else { return nil }
            return (index, overlap)
        }.sorted { $0.1 > $1.1 }
        guard let best = scores.first else { return nil }
        if scores.count > 1, best.1 - scores[1].1 < 0.1 { return nil }
        return best.0
    }

    private static func valid(_ rect: CGRect) -> Bool {
        rect.origin.x.isFinite && rect.origin.y.isFinite && rect.width.isFinite && rect.height.isFinite
            && rect.width > 0 && rect.height > 0 && rect.minX >= 0 && rect.minY >= 0
            && rect.maxX <= 1 && rect.maxY <= 1
    }
}
