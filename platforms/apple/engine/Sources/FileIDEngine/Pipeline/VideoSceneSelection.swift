import Foundation

/// Splits a video's low-resolution frame pool into visual scenes (CLIP cosine) and
/// activity windows (luma change), so the VLM sees one representative per scene and
/// moment windows only where something moves. Mirrored in the Rust engine
/// (pipeline/video_scene_selection.rs) with the same thresholds; change both together.
enum VideoSceneSelection {
    struct Frame: Sendable, Equatable {
        let seconds: Double
        let embedding: [Float]
        /// Mean absolute luma change from the previous pool frame, 0...1.
        let change: Double
    }

    struct Scene: Sendable, Equatable {
        let start: Double
        let end: Double
        /// Index into the frame pool of the frame closest to the scene's mean embedding.
        let representative: Int
        let frames: Range<Int>
    }

    struct Window: Sendable, Equatable {
        let start: Double
        let end: Double
        let times: [Double]
    }

    static let poolInterval = 0.5
    static let cutCosine: Float = 0.86
    static let driftCosine: Float = 0.78
    static let minSceneSeconds = 1.0
    static let activityChange = 0.02
    static let activityPadSeconds = 0.5
    static let mergeGapSeconds = 1.0
    static let maxWindowSeconds = 4.0
    static let maxWindowFrames = 8

    static func poolTimes(duration: Double, interval: Double = poolInterval) -> [Double] {
        guard duration.isFinite, duration > 0, interval > 0 else { return [] }
        let count = min(4_096, Int((duration / interval).rounded(.up)))
        return (0..<count).map { min(duration - 0.001, Double($0) * interval) }.filter { $0 >= 0 }
    }

    static func scenes(_ frames: [Frame], duration: Double) -> [Scene] {
        guard !frames.isEmpty, duration.isFinite, duration > 0 else { return [] }
        var bounds: [Int] = [0]
        for i in frames.indices.dropFirst() {
            let start = bounds.last!
            let toPrevious = NearDuplicateGrouping.cosine(frames[i].embedding, frames[i - 1].embedding)
            let toStart = NearDuplicateGrouping.cosine(frames[i].embedding, frames[start].embedding)
            if toPrevious < cutCosine || toStart < driftCosine, frames[i].seconds - frames[start].seconds >= minSceneSeconds { bounds.append(i) }
        }
        bounds.append(frames.count)
        return zip(bounds, bounds.dropFirst()).map { lower, upper in
            let range = lower..<upper
            let dims = frames[lower].embedding.count
            var mean = [Float](repeating: 0, count: dims)
            for i in range where frames[i].embedding.count == dims {
                for d in 0..<dims { mean[d] += frames[i].embedding[d] }
            }
            let representative = range.max { a, b in
                NearDuplicateGrouping.cosine(frames[a].embedding, mean) < NearDuplicateGrouping.cosine(frames[b].embedding, mean)
            } ?? lower
            let end = upper < frames.count ? frames[upper].seconds : duration
            return Scene(start: lower == 0 ? 0 : frames[lower].seconds, end: end, representative: representative, frames: range)
        }
    }

    /// Spans whose frame-to-frame luma change reaches `activityChange`, padded, merged
    /// across short gaps, and split into windows of at most `maxWindowSeconds`. Each
    /// window samples pool times, at most `maxWindowFrames`, at least two.
    static func activityWindows(_ frames: [Frame], duration: Double) -> [Window] {
        guard frames.count >= 2, duration.isFinite, duration > 0 else { return [] }
        var spans: [(Double, Double)] = []
        for (i, frame) in frames.enumerated() where i > 0 && frame.change >= activityChange {
            let start = max(0, frames[i - 1].seconds - activityPadSeconds)
            let end = min(duration, frame.seconds + activityPadSeconds)
            if let last = spans.last, start - last.1 <= mergeGapSeconds {
                spans[spans.count - 1].1 = max(last.1, end)
            } else {
                spans.append((start, end))
            }
        }
        var windows: [Window] = []
        for (spanStart, spanEnd) in spans {
            let pieces = max(1, Int(((spanEnd - spanStart) / maxWindowSeconds).rounded(.up)))
            let length = (spanEnd - spanStart) / Double(pieces)
            for piece in 0..<pieces {
                let start = spanStart + Double(piece) * length
                let end = piece == pieces - 1 ? spanEnd : start + length
                var times = frames.map(\.seconds).filter { $0 >= start && $0 <= end }
                if times.count > maxWindowFrames {
                    let step = Double(times.count - 1) / Double(maxWindowFrames - 1)
                    times = (0..<maxWindowFrames).map { times[Int((Double($0) * step).rounded())] }
                }
                if times.count < 2 {
                    let nearest = frames.map(\.seconds).sorted { abs($0 - start) < abs($1 - start) }.prefix(2)
                    times = Array(Set(times + nearest)).sorted()
                }
                guard times.count >= 2 else { continue }
                windows.append(Window(start: min(start, times[0]), end: max(end, times.last!), times: times))
            }
        }
        return windows
    }

    static func lumaChange(_ lhs: [UInt8], _ rhs: [UInt8]) -> Double {
        guard lhs.count == rhs.count, !lhs.isEmpty else { return 0 }
        var total = 0
        for i in lhs.indices { total += abs(Int(lhs[i]) - Int(rhs[i])) }
        return Double(total) / Double(lhs.count * 255)
    }
}
