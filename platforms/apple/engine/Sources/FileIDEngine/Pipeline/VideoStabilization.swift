import AVFoundation
import Foundation
import Vision

enum VideoStabilization {
    struct Observation: Sendable {
        var time: Double
        var alignment: CGAffineTransform?
    }

    struct Keyframe: Sendable {
        var time: Double
        var transform: CGAffineTransform
    }

    struct Plan: Sendable {
        var frames: [Keyframe]
        var correctedSamples: Int
        var uncorrectedSamples: Int
        var zoom: Double

        var summary: String {
            "Stabilization: \(correctedSamples) corrected samples, \(uncorrectedSamples) uncorrected samples; \(Int(((zoom - 1) * 100).rounded()))% edge-covering zoom. Review moving subjects and scene changes."
        }

        func transform(at time: Double) -> CGAffineTransform {
            guard let first = frames.first, let last = frames.last else { return .identity }
            if time <= first.time { return first.transform }
            if time >= last.time { return last.transform }
            var low = 0
            var high = frames.count - 1
            while high - low > 1 {
                let middle = (low + high) / 2
                if frames[middle].time <= time { low = middle } else { high = middle }
            }
            let left = frames[low]
            let right = frames[high]
            return interpolate(left.transform, right.transform, fraction: (time - left.time) / (right.time - left.time))
        }
    }

    private struct Pose {
        var time: Double
        var camera: CGAffineTransform
        var x: Double
        var y: Double
        var angle: Double
        var segment: Int
        var registered: Bool
    }

    static func interpolate(_ left: CGAffineTransform, _ right: CGAffineTransform, fraction: Double) -> CGAffineTransform {
        let fraction = min(max(fraction, 0), 1)
        return CGAffineTransform(a: left.a + (right.a - left.a) * fraction,
                                 b: left.b + (right.b - left.b) * fraction,
                                 c: left.c + (right.c - left.c) * fraction,
                                 d: left.d + (right.d - left.d) * fraction,
                                 tx: left.tx + (right.tx - left.tx) * fraction,
                                 ty: left.ty + (right.ty - left.ty) * fraction)
    }

    private static func rigid(angle: Double, x: Double, y: Double, center: CGPoint) -> CGAffineTransform {
        CGAffineTransform(translationX: -center.x, y: -center.y)
            .concatenating(CGAffineTransform(rotationAngle: angle))
            .concatenating(CGAffineTransform(translationX: center.x + x, y: center.y + y))
    }

    private static func valid(_ transform: CGAffineTransform) -> Bool {
        [transform.a, transform.b, transform.c, transform.d, transform.tx, transform.ty].allSatisfy(\.isFinite)
            && abs(transform.a * transform.d - transform.b * transform.c - 1) < 0.001
    }

    static func requiredZoom(_ correction: CGAffineTransform, size: CGSize) -> Double {
        let center = CGPoint(x: size.width / 2, y: size.height / 2)
        let inverse = correction.inverted()
        let shifted = center.applying(inverse)
        let roomX = size.width / 2 - abs(shifted.x - center.x)
        let roomY = size.height / 2 - abs(shifted.y - center.y)
        guard roomX > 0, roomY > 0 else { return .infinity }
        return max(1, (abs(inverse.a) * size.width + abs(inverse.c) * size.height) / (2 * roomX),
                   (abs(inverse.b) * size.width + abs(inverse.d) * size.height) / (2 * roomY))
    }

    static func plan(observations: [Observation], sourceSize: CGSize) throws -> Plan {
        guard sourceSize.width.isFinite, sourceSize.height.isFinite, sourceSize.width > 0, sourceSize.height > 0,
              !observations.isEmpty, observations.count <= 36_002 else {
            throw MediaTools.Failure(text: "Invalid stabilization samples or dimensions.")
        }
        let center = CGPoint(x: sourceSize.width / 2, y: sourceSize.height / 2)
        var poses: [Pose] = []
        var camera = CGAffineTransform.identity
        var angle = 0.0
        var segment = 0
        for (index, observation) in observations.enumerated() {
            guard observation.time.isFinite, observation.time >= 0,
                  index == 0 || observation.time > observations[index - 1].time else {
                throw MediaTools.Failure(text: "Stabilization timestamps must increase.")
            }
            let registered = index > 0 && observation.alignment.map(valid) == true
                && observation.time - observations[index - 1].time <= 0.2
            if registered, let alignment = observation.alignment {
                camera = camera.concatenating(alignment.inverted())
                angle -= atan2(alignment.b, alignment.a)
            } else {
                camera = .identity
                angle = 0
                segment += 1
            }
            let point = center.applying(camera)
            poses.append(Pose(time: observation.time, camera: camera, x: point.x - center.x,
                              y: point.y - center.y, angle: angle, segment: segment, registered: registered))
        }
        var prefixX = [0.0]
        var prefixY = [0.0]
        var prefixAngle = [0.0]
        for pose in poses {
            prefixX.append(prefixX.last! + pose.x)
            prefixY.append(prefixY.last! + pose.y)
            prefixAngle.append(prefixAngle.last! + pose.angle)
        }
        var corrections: [CGAffineTransform] = []
        var zoom = 1.0
        var corrected = 0
        var uncorrected = 0
        var low = 0
        var high = 0
        for (index, pose) in poses.enumerated() {
            while low < index && (poses[low].segment != pose.segment || poses[low].time < pose.time - 0.25) { low += 1 }
            high = max(high, index)
            while high + 1 < poses.count && poses[high + 1].segment == pose.segment && poses[high + 1].time <= pose.time + 0.25 { high += 1 }
            let count = Double(high - low + 1)
            let desired = rigid(angle: (prefixAngle[high + 1] - prefixAngle[low]) / count,
                                x: (prefixX[high + 1] - prefixX[low]) / count,
                                y: (prefixY[high + 1] - prefixY[low]) / count, center: center)
            let correction = pose.camera.inverted().concatenating(desired)
            let required = requiredZoom(correction, size: sourceSize)
            if valid(correction), required.isFinite, required <= 1.118 {
                corrections.append(correction)
                zoom = max(zoom, required)
                if abs(correction.tx) + abs(correction.ty) > 0.002 || abs(correction.b) > 0.00005 { corrected += 1 }
                else if !pose.registered && index > 0 { uncorrected += 1 }
            } else {
                corrections.append(.identity)
                uncorrected += 1
            }
        }
        if zoom > 1 { zoom = min(1.12, zoom + 0.002) }
        let cover = CGAffineTransform(translationX: -center.x, y: -center.y)
            .concatenating(CGAffineTransform(scaleX: zoom, y: zoom))
            .concatenating(CGAffineTransform(translationX: center.x, y: center.y))
        let flip = CGAffineTransform(a: 1, b: 0, c: 0, d: -1, tx: 0, ty: sourceSize.height)
        let frames = zip(poses, corrections).map {
            Keyframe(time: $0.0.time, transform: flip.concatenating($0.1.concatenating(cover)).concatenating(flip))
        }
        return Plan(frames: frames, correctedSamples: corrected, uncorrectedSamples: uncorrected, zoom: zoom)
    }

    static func alignment(reference: CGImage, current: CGImage, sourceSize: CGSize) throws -> CGAffineTransform? {
        guard reference.width == current.width, reference.height == current.height else { return nil }
        let translation = VNTranslationalImageRegistrationRequest(targetedCGImage: current)
        try VNImageRequestHandler(cgImage: reference, options: [:]).perform([translation])
        guard let seed = translation.results?.first?.alignmentTransform,
              seed.tx.isFinite, seed.ty.isFinite,
              hypot(seed.tx, seed.ty) <= Double(max(current.width,current.height)) * 0.1,
              let context = CGContext(data: nil, width: current.width, height: current.height,
                bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        context.concatenate(seed)
        context.draw(current, in: CGRect(x: 0, y: 0, width: current.width, height: current.height))
        guard let aligned = context.makeImage() else { return nil }
        let request = VNHomographicImageRegistrationRequest(targetedCGImage: aligned)
        try VNImageRequestHandler(cgImage: reference, options: [:]).perform([request])
        guard let matrix = request.results?.first?.warpTransform else { return nil }
        let x = Double(current.width) / 2
        let y = Double(current.height) / 2
        let a = Double(matrix.columns.0.x), b = Double(matrix.columns.0.y), p = Double(matrix.columns.0.z)
        let c = Double(matrix.columns.1.x), d = Double(matrix.columns.1.y), q = Double(matrix.columns.1.z)
        let tx = Double(matrix.columns.2.x), ty = Double(matrix.columns.2.y), r = Double(matrix.columns.2.z)
        let denominator = p * x + q * y + r
        guard [a,b,c,d,p,q,tx,ty,r].allSatisfy(\.isFinite), abs(denominator) > 0.1 else { return nil }
        let mappedX = (a * x + c * y + tx) / denominator
        let mappedY = (b * x + d * y + ty) / denominator
        let ja = (a - mappedX * p) / denominator, jb = (b - mappedY * p) / denominator
        let jc = (c - mappedX * q) / denominator, jd = (d - mappedY * q) / denominator
        let scaleX = hypot(ja, jb), scaleY = hypot(jc, jd)
        let rotation = atan2(jb - jc, ja + jd)
        let dx = (mappedX - x) * sourceSize.width / Double(current.width)
        let dy = (mappedY - y) * sourceSize.height / Double(current.height)
        guard (0.92...1.08).contains(scaleX), (0.92...1.08).contains(scaleY), ja * jd - jb * jc > 0,
              abs(ja * jc + jb * jd) <= 0.08, abs(rotation) <= 0.08,
              hypot(dx, dy) <= max(sourceSize.width, sourceSize.height) * 0.1 else { return nil }
        let translationScale = CGAffineTransform(translationX: seed.tx * sourceSize.width / Double(current.width),
                                                  y: seed.ty * sourceSize.height / Double(current.height))
        return translationScale.concatenating(rigid(angle: rotation, x: dx, y: dy,
            center: CGPoint(x: sourceSize.width / 2, y: sourceSize.height / 2)))
    }

    static func analyze(asset: AVURLAsset, duration: Double, sourceSize: CGSize, frameRate: Double) throws -> Plan {
        guard duration.isFinite, duration > 0, duration <= 600, frameRate.isFinite, frameRate > 0, frameRate <= 60 else {
            throw MediaTools.Failure(text: "Stabilization currently supports clips up to ten minutes and 60 frames per second.")
        }
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: 640, height: 640)
        generator.requestedTimeToleranceBefore = CMTime(seconds: 1 / frameRate / 3, preferredTimescale: 60_000)
        generator.requestedTimeToleranceAfter = generator.requestedTimeToleranceBefore
        var previous: CGImage?
        var observations: [Observation] = []
        let count = Int(ceil(duration * frameRate))
        for index in 0...count {
            try Task.checkCancellation()
            let time = min(Double(index) / frameRate, duration)
            let image = try autoreleasepool {
                try generator.copyCGImage(at: CMTime(seconds: min(time, max(0, duration - 1 / frameRate)), preferredTimescale: 60_000), actualTime: nil)
            }
            let alignment: CGAffineTransform? = autoreleasepool {
                guard let previous else { return nil }
                return try? self.alignment(reference: previous, current: image, sourceSize: sourceSize)
            }
            observations.append(Observation(time: time, alignment: alignment))
            previous = image
        }
        return try plan(observations: observations, sourceSize: sourceSize)
    }
}
