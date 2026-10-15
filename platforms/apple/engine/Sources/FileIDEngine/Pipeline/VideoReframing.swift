import AVFoundation
import Foundation
import Vision

enum VideoReframing {
    struct Observation: Sendable {
        var time: Double
        var people: [CGRect]
    }

    struct Keyframe: Sendable {
        var time: Double
        var crop: CGRect?
    }

    struct Plan: Sendable {
        var frames: [Keyframe]
        var followedSamples: Int
        var fittedSamples: Int

        var summary: String {
            "People framing: \(followedSamples) followed samples, \(fittedSamples) full-picture samples. Detection is sampled twice per second; review the exported framing."
        }
    }

    static func plan(observations: [Observation], sourceSize: CGSize, ratio: Double) throws -> Plan {
        guard sourceSize.width.isFinite, sourceSize.height.isFinite, sourceSize.width > 0, sourceSize.height > 0,
              ratio.isFinite, ratio > 0, !observations.isEmpty, observations.count <= 1_202 else {
            throw MediaTools.Failure(text: "Invalid people-framing observations or dimensions.")
        }
        let full = CGRect(origin: .zero, size: sourceSize)
        let cropSize = sourceSize.width / sourceSize.height > ratio
            ? CGSize(width: sourceSize.height * ratio, height: sourceSize.height)
            : CGSize(width: sourceSize.width, height: sourceSize.width / ratio)
        var frames: [Keyframe] = []
        var previous: CGRect?
        var previousTime: Double?
        for observation in observations {
            guard observation.time.isFinite, observation.time >= 0,
                  previousTime.map({ observation.time > $0 }) ?? true else {
                throw MediaTools.Failure(text: "People-framing timestamps must increase.")
            }
            let delta = previousTime.map { observation.time - $0 }
            let safe = safeBounds(observation.people, sourceSize: sourceSize)
            var crop: CGRect?
            if let safe, safe.width <= cropSize.width, safe.height <= cropSize.height,
               delta.map({ $0 <= 0.75 }) ?? true {
                let minimumX = max(0, safe.maxX - cropSize.width)
                let maximumX = min(full.maxX - cropSize.width, safe.minX)
                let minimumY = max(0, safe.maxY - cropSize.height)
                let maximumY = min(full.maxY - cropSize.height, safe.minY)
                let desiredX = min(max(safe.midX - cropSize.width / 2, minimumX), maximumX)
                let desiredY = min(max(safe.midY - cropSize.height / 2, minimumY), maximumY)
                var x = desiredX
                var y = desiredY
                if let priorCrop = previous, let delta {
                    let blend = 1 - exp(-delta / 0.4)
                    x = priorCrop.minX + (desiredX - priorCrop.minX) * blend
                    y = priorCrop.minY + (desiredY - priorCrop.minY) * blend
                    let maximumMove = max(sourceSize.width, sourceSize.height) * 0.18 * delta
                    x = min(max(x, priorCrop.minX - maximumMove), priorCrop.minX + maximumMove)
                    y = min(max(y, priorCrop.minY - maximumMove), priorCrop.minY + maximumMove)
                    if x < minimumX || x > maximumX || y < minimumY || y > maximumY {
                        frames.append(Keyframe(time: observation.time, crop: nil))
                        previous = nil
                        previousTime = observation.time
                        continue
                    }
                }
                crop = CGRect(x: x, y: y, width: cropSize.width, height: cropSize.height)
            }
            frames.append(Keyframe(time: observation.time, crop: crop))
            previous = crop
            previousTime = observation.time
        }
        let unsafe = frames.indices.filter { frames[$0].crop == nil }
        for index in unsafe {
            if index > 0 { frames[index - 1].crop = nil }
            if index + 1 < frames.count { frames[index + 1].crop = nil }
        }
        let followed = frames.filter { $0.crop != nil }.count
        return Plan(frames: frames, followedSamples: followed, fittedSamples: frames.count - followed)
    }

    private static func safeBounds(_ people: [CGRect], sourceSize: CGSize) -> CGRect? {
        guard !people.isEmpty, people.count <= 100 else { return nil }
        var bounds = CGRect.null
        for person in people {
            guard person.minX.isFinite, person.minY.isFinite, person.width.isFinite, person.height.isFinite,
                  person.width > 0, person.height > 0, person.minX >= 0, person.minY >= 0,
                  person.maxX <= 1, person.maxY <= 1 else { return nil }
            bounds = bounds.union(CGRect(x: person.minX * sourceSize.width, y: person.minY * sourceSize.height,
                                         width: person.width * sourceSize.width, height: person.height * sourceSize.height))
        }
        let paddingX = max(sourceSize.width * 0.025, bounds.width * 0.12)
        let paddingY = max(sourceSize.height * 0.025, bounds.height * 0.12)
        return bounds.insetBy(dx: -paddingX, dy: -paddingY)
            .intersection(CGRect(origin: .zero, size: sourceSize))
    }

    static func analyze(asset: AVURLAsset, duration: Double, sourceSize: CGSize, ratio: Double, stabilization: VideoStabilization.Plan? = nil) throws -> Plan {
        guard duration.isFinite, duration > 0, duration <= 600 else {
            throw MediaTools.Failure(text: "Follow-people framing currently supports videos up to ten minutes. Use full-picture fitting for longer videos.")
        }
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: 720, height: 720)
        generator.requestedTimeToleranceBefore = .zero
        generator.requestedTimeToleranceAfter = .zero
        var observations: [Observation] = []
        let count = Int(ceil(duration * 2))
        for index in 0...count {
            try Task.checkCancellation()
            let time = min(Double(index) / 2, duration)
            let sampleTime = max(0, min(time, duration - 0.02))
            let people: [CGRect] = try autoreleasepool {
                let image = try generator.copyCGImage(at: CMTime(seconds: sampleTime, preferredTimescale: 60_000), actualTime: nil)
                let request = VNDetectHumanRectanglesRequest()
                request.upperBodyOnly = false
                let handler = VNImageRequestHandler(cgImage: image, options: [:])
                try handler.perform([request])
                return (request.results ?? []).map {
                    let box = $0.boundingBox
                    let upright = CGRect(x: box.minX * sourceSize.width, y: (1 - box.maxY) * sourceSize.height, width: box.width * sourceSize.width, height: box.height * sourceSize.height)
                    let corrected = upright.applying(stabilization?.transform(at: time) ?? .identity)
                    return CGRect(x: corrected.minX / sourceSize.width, y: corrected.minY / sourceSize.height, width: corrected.width / sourceSize.width, height: corrected.height / sourceSize.height)
                }
            }
            observations.append(Observation(time: time, people: people))
        }
        return try plan(observations: observations, sourceSize: sourceSize, ratio: ratio)
    }

    static func composition(asset: AVURLAsset, dimension: Int, ratio: Double, plan: Plan? = nil, stabilization: VideoStabilization.Plan? = nil) async throws -> AVMutableVideoComposition {
        guard let video = try await asset.load(.tracks).first(where: { $0.mediaType == .video }) else {
            throw MediaTools.Failure(text: "The requested video frame is unavailable.")
        }
        let size = try await video.load(.naturalSize)
        let orientation = try await video.load(.preferredTransform)
        let bounds = CGRect(origin: .zero, size: size).applying(orientation)
        let longEdge = Double(dimension)
        let canvas = ratio < 1
            ? CGSize(width: (longEdge * ratio).rounded(), height: longEdge)
            : CGSize(width: longEdge, height: (longEdge / ratio).rounded())
        guard bounds.width > 0, bounds.height > 0, canvas.width > 0, canvas.height > 0 else {
            throw MediaTools.Failure(text: "The video has invalid display dimensions.")
        }
        func transform(crop: CGRect?) -> CGAffineTransform {
            let area = crop ?? CGRect(origin: .zero, size: bounds.size)
            let scale = crop == nil ? min(canvas.width / area.width, canvas.height / area.height) : max(canvas.width / area.width, canvas.height / area.height)
            let paddingX = (canvas.width - area.width * scale) / 2
            let paddingY = (canvas.height - area.height * scale) / 2
            return CGAffineTransform(translationX: -area.minX, y: -area.minY)
                .concatenating(CGAffineTransform(scaleX: scale, y: scale))
                .concatenating(CGAffineTransform(translationX: paddingX, y: paddingY))
        }
        let framingFrames = plan?.frames ?? [Keyframe(time: 0, crop: nil)]
        guard !framingFrames.isEmpty else { throw MediaTools.Failure(text: "The framing plan is empty.") }
        func framing(at time: Double) -> CGAffineTransform {
            guard let first = framingFrames.first, let last = framingFrames.last else { return transform(crop: nil) }
            if time <= first.time { return transform(crop: first.crop) }
            if time >= last.time { return transform(crop: last.crop) }
            var low = 0
            var high = framingFrames.count - 1
            while high - low > 1 {
                let middle = (low + high) / 2
                if framingFrames[middle].time <= time { low = middle } else { high = middle }
            }
            let left = framingFrames[low]
            let right = framingFrames[high]
            return VideoStabilization.interpolate(transform(crop: left.crop), transform(crop: right.crop), fraction: (time - left.time) / (right.time - left.time))
        }
        let upright = orientation.concatenating(CGAffineTransform(translationX: -bounds.minX, y: -bounds.minY))
        func composed(at time: Double) -> CGAffineTransform {
            upright.concatenating(stabilization?.transform(at: time) ?? .identity).concatenating(framing(at: time))
        }
        let times = Set(framingFrames.map(\.time) + (stabilization?.frames.map(\.time) ?? [])).sorted()
        let layer = AVMutableVideoCompositionLayerInstruction(assetTrack: video)
        layer.setTransform(composed(at: 0), at: .zero)
        for (start, end) in zip(times, times.dropFirst()) {
            let range = CMTimeRange(start: CMTime(seconds: start, preferredTimescale: 60_000),
                                    duration: CMTime(seconds: end - start, preferredTimescale: 60_000))
            layer.setTransformRamp(fromStart: composed(at: start), toEnd: composed(at: end), timeRange: range)
        }
        let instruction = AVMutableVideoCompositionInstruction()
        instruction.timeRange = CMTimeRange(start: .zero, duration: try await asset.load(.duration))
        instruction.layerInstructions = [layer]
        let composition = AVMutableVideoComposition()
        composition.renderSize = canvas
        let nominalRate = try await video.load(.nominalFrameRate)
        composition.frameDuration = CMTime(seconds: nominalRate.isFinite && nominalRate > 0 ? 1.0 / Double(nominalRate) : 1.0 / 30.0, preferredTimescale: 60_000)
        composition.instructions = [instruction]
        return composition
    }
}
