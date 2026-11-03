import Foundation

/// Model-free Best Takes scoring: subject sharpness, motion blur, exposure,
/// faces/eyes/framing and, for clips, steadiness and action. Pure functions on
/// 8-bit luma so the Rust engine (pipeline/take_scoring.rs) computes the same
/// signals, scores and reasons from the same pixels; change both together.
enum TakeScoring {
    static let version = "take-signals-v2"
    static let analysisLongSide = 1024
    static let videoLongSide = 384
    static let motionLongSide = 96
    static let motionSearch = 6

    struct Luma: Sendable, Equatable {
        let width: Int
        let height: Int
        let pixels: [UInt8]

        /// BT.601 integer luma, (77 R + 150 G + 29 B) >> 8, from packed RGB or RGBA.
        static func fromRGB(_ bytes: UnsafeBufferPointer<UInt8>, width: Int, height: Int, bytesPerPixel: Int, bytesPerRow: Int) -> Luma {
            var out = [UInt8](repeating: 0, count: width * height)
            for y in 0..<height {
                let row = y * bytesPerRow
                for x in 0..<width {
                    let p = row + x * bytesPerPixel
                    out[y * width + x] = UInt8((77 * Int(bytes[p]) + 150 * Int(bytes[p + 1]) + 29 * Int(bytes[p + 2])) >> 8)
                }
            }
            return Luma(width: width, height: height, pixels: out)
        }
    }

    struct Box: Codable, Sendable, Equatable {
        var x: Double
        var y: Double
        var width: Double
        var height: Double
    }

    /// One detected face, top-left origin, normalized to the frame. `quality`
    /// and `eyesOpen` are 0…1 when the detector can judge them.
    struct Face: Codable, Sendable, Equatable {
        var x: Double
        var y: Double
        var width: Double
        var height: Double
        var quality: Double?
        var eyesOpen: Double?
    }

    struct VideoSignals: Codable, Sendable, Equatable {
        var duration: Double
        /// Mean camera shift between paired frames, in pixels at `motionLongSide`.
        var shake: Double
        /// Mean subject change left after removing the camera shift, 0…1.
        var action: Double
        var peakSeconds: Double
    }

    /// Per-file measurements, cached in `take_quality.signals_json`.
    struct Signals: Codable, Sendable, Equatable {
        var width: Int
        var height: Int
        var sharpness: Double
        var anisotropy: Double
        var subject: Box?
        var meanLuma: Double
        var highlights: Double
        var shadows: Double
        var faces: [Face]
        var faceSharpness: Double?
        var video: VideoSignals?
        /// Whole-person boxes, when the platform has a person detector.
        var people: [Box]?
    }

    // MARK: - Per-image signals

    struct CellStats {
        var lap: Int64 = 0
        var dx: Int64 = 0
        var dy: Int64 = 0
        var d1: Int64 = 0
        var d2: Int64 = 0
        var luma: Int64 = 0
        var count: Int64 = 0
    }

    static func cellSize(width: Int, height: Int) -> Int {
        max(8, Int((Double(max(width, height)) / 32).rounded()))
    }

    static func signals(_ image: Luma, faces: [Face], people: [Box]? = nil) -> Signals {
        let w = image.width, h = image.height
        let cell = cellSize(width: w, height: h)
        let columns = (w + cell - 1) / cell, rows = (h + cell - 1) / cell
        var cells = [CellStats](repeating: CellStats(), count: max(1, columns * rows))
        var lumaSum: Int64 = 0, bright: Int64 = 0, dark: Int64 = 0
        image.pixels.withUnsafeBufferPointer { p in
            for value in p {
                lumaSum += Int64(value)
                if value >= 250 { bright += 1 }
                if value <= 5 { dark += 1 }
            }
            guard w >= 3, h >= 3 else { return }
            for y in 1..<(h - 1) {
                let row = y * w
                let cellRow = (y / cell) * columns
                for x in 1..<(w - 1) {
                    let i = row + x
                    let c = Int(p[i])
                    let lap = 4 * c - Int(p[i - 1]) - Int(p[i + 1]) - Int(p[i - w]) - Int(p[i + w])
                    let dx = Int(p[i + 1]) - c, dy = Int(p[i + w]) - c
                    let d1 = Int(p[i + w + 1]) - c, d2 = Int(p[i - w + 1]) - c
                    let index = cellRow + x / cell
                    cells[index].lap += Int64(lap * lap)
                    cells[index].dx += Int64(dx * dx)
                    cells[index].dy += Int64(dy * dy)
                    cells[index].d1 += Int64(d1 * d1)
                    cells[index].d2 += Int64(d2 * d2)
                    cells[index].luma += Int64(c)
                    cells[index].count += 1
                }
            }
        }
        let minimum = Int64(max(1, cell * cell / 4))
        var ranked: [(index: Int, energy: Double)] = []
        for (index, stats) in cells.enumerated() where stats.count >= minimum {
            let mean = Double(stats.luma) / Double(stats.count)
            guard mean >= 8, mean <= 247 else { continue }
            ranked.append((index, Double(stats.lap) / Double(stats.count)))
        }
        ranked.sort { $0.energy == $1.energy ? $0.index < $1.index : $0.energy > $1.energy }
        let top = Array(ranked.prefix(max(1, Int((Double(ranked.count) * 0.1).rounded(.up)))))
        var sharpness = 0.0, anisotropy = 0.0
        var subject: Box?
        if !top.isEmpty {
            sharpness = (top.reduce(0) { $0 + $1.energy } / Double(top.count)).squareRoot()
            var sx: Int64 = 0, sy: Int64 = 0, s1: Int64 = 0, s2: Int64 = 0
            var weight = 0.0, cx = 0.0, cy = 0.0
            for entry in top {
                let stats = cells[entry.index]
                sx += stats.dx; sy += stats.dy; s1 += stats.d1; s2 += stats.d2
                let px = (Double(entry.index % columns) + 0.5) * Double(cell) / Double(w)
                let py = (Double(entry.index / columns) + 0.5) * Double(cell) / Double(h)
                weight += entry.energy; cx += entry.energy * px; cy += entry.energy * py
            }
            let directional = [Double(sx), Double(sy), Double(s1) / 2, Double(s2) / 2]
            if let high = directional.max(), high > 0 { anisotropy = 1 - directional.min()! / high }
            if weight > 0 {
                cx /= weight; cy /= weight
                var vx = 0.0, vy = 0.0
                for entry in top {
                    let px = (Double(entry.index % columns) + 0.5) * Double(cell) / Double(w)
                    let py = (Double(entry.index / columns) + 0.5) * Double(cell) / Double(h)
                    vx += entry.energy * (px - cx) * (px - cx)
                    vy += entry.energy * (py - cy) * (py - cy)
                }
                let half = (x: max(0.1, min(0.3, 1.5 * (vx / weight).squareRoot())), y: max(0.1, min(0.3, 1.5 * (vy / weight).squareRoot())))
                subject = clampBox(Box(x: cx - half.x, y: cy - half.y, width: 2 * half.x, height: 2 * half.y))
            }
        }
        let n = Double(max(1, image.pixels.count))
        let primary = primaryFaces(faces, width: w, height: h)
        let faceSharpness = primary.first.flatMap { regionSharpness(image, $0) }
        return Signals(width: w, height: h, sharpness: round4(sharpness), anisotropy: round4(anisotropy), subject: subject,
                       meanLuma: round4(Double(lumaSum) / n / 255), highlights: round4(Double(bright) / n),
                       shadows: round4(Double(dark) / n), faces: faces, faceSharpness: faceSharpness.map(round4), video: nil,
                       people: people)
    }

    static func regionSharpness(_ image: Luma, _ face: Face) -> Double? {
        let w = image.width, h = image.height
        let x0 = max(1, Int((face.x * Double(w)).rounded(.down))), y0 = max(1, Int((face.y * Double(h)).rounded(.down)))
        let x1 = min(w - 1, Int(((face.x + face.width) * Double(w)).rounded(.up)))
        let y1 = min(h - 1, Int(((face.y + face.height) * Double(h)).rounded(.up)))
        guard x1 - x0 >= 4, y1 - y0 >= 4 else { return nil }
        var sum: Int64 = 0, count: Int64 = 0
        image.pixels.withUnsafeBufferPointer { p in
            for y in y0..<y1 {
                for x in x0..<x1 {
                    let i = y * w + x
                    let lap = 4 * Int(p[i]) - Int(p[i - 1]) - Int(p[i + 1]) - Int(p[i - w]) - Int(p[i + w])
                    sum += Int64(lap * lap)
                    count += 1
                }
            }
        }
        return (Double(sum) / Double(count)).squareRoot()
    }

    static let minFacePixels = 24.0
    static let minEyePixels = 40.0
    static let eyesClosedBelow = 0.35

    /// Faces big enough to judge, largest first: at least `minFacePixels` on the
    /// short side and a quarter of the largest face's area.
    static func primaryFaces(_ faces: [Face], width: Int, height: Int) -> [Face] {
        let sized = faces.filter { min($0.width * Double(width), $0.height * Double(height)) >= minFacePixels }
            .sorted { $0.width * $0.height == $1.width * $1.height ? $0.x < $1.x : $0.width * $0.height > $1.width * $1.height }
        guard let largest = sized.first else { return [] }
        return sized.filter { $0.width * $0.height >= 0.25 * largest.width * largest.height }
    }

    // MARK: - Video signals

    /// Box-averaged downsample to `longSide` on the long edge.
    static func downsample(_ image: Luma, longSide: Int) -> Luma {
        let scale = Double(max(image.width, image.height)) / Double(longSide)
        guard scale > 1 else { return image }
        let w = max(1, Int((Double(image.width) / scale).rounded())), h = max(1, Int((Double(image.height) / scale).rounded()))
        var out = [UInt8](repeating: 0, count: w * h)
        image.pixels.withUnsafeBufferPointer { p in
            for y in 0..<h {
                let sy0 = y * image.height / h, sy1 = max(sy0 + 1, (y + 1) * image.height / h)
                for x in 0..<w {
                    let sx0 = x * image.width / w, sx1 = max(sx0 + 1, (x + 1) * image.width / w)
                    var sum = 0
                    for sy in sy0..<sy1 { for sx in sx0..<sx1 { sum += Int(p[sy * image.width + sx]) } }
                    out[y * w + x] = UInt8(sum / ((sy1 - sy0) * (sx1 - sx0)))
                }
            }
        }
        return Luma(width: w, height: h, pixels: out)
    }

    /// Camera shift between two frames (integer search within ±`motionSearch`)
    /// and the mean absolute difference left after removing it, 0…1.
    static func shift(_ a: Luma, _ b: Luma) -> (dx: Int, dy: Int, residual: Double) {
        guard a.width == b.width, a.height == b.height, a.width > 2 * motionSearch, a.height > 2 * motionSearch else { return (0, 0, 0) }
        var best = (dx: 0, dy: 0, sad: Int64.max, count: Int64(1))
        let r = motionSearch
        a.pixels.withUnsafeBufferPointer { pa in
            b.pixels.withUnsafeBufferPointer { pb in
                for dy in -r...r {
                    for dx in -r...r {
                        var sad: Int64 = 0
                        for y in r..<(a.height - r) {
                            let ra = y * a.width, rb = (y + dy) * a.width + dx
                            for x in r..<(a.width - r) { sad += Int64(abs(Int(pa[ra + x]) - Int(pb[rb + x]))) }
                        }
                        let better = sad < best.sad || (sad == best.sad && abs(dx) + abs(dy) < abs(best.dx) + abs(best.dy))
                        if better { best = (dx, dy, sad, Int64((a.width - 2 * r) * (a.height - 2 * r))) }
                    }
                }
            }
        }
        return (best.dx, best.dy, Double(best.sad) / Double(best.count) / 255)
    }

    /// Clip signals from frame pairs taken a fraction of a second apart at
    /// evenly spaced anchors: steadiness from the camera shift inside each pair,
    /// action from what still changes, sharpness from the sharpest frame.
    static func videoSignals(pairs: [(seconds: Double, first: Luma, second: Luma)], duration: Double) -> Signals? {
        guard !pairs.isEmpty else { return nil }
        var best: Signals?
        var shakes: [Double] = [], actions: [Double] = []
        var peak = (seconds: pairs[0].seconds, action: -1.0)
        for pair in pairs {
            for frame in [pair.first, pair.second] {
                let s = signals(frame, faces: [])
                if best == nil || s.sharpness > best!.sharpness { best = s }
            }
            let motion = shift(downsample(pair.first, longSide: motionLongSide), downsample(pair.second, longSide: motionLongSide))
            shakes.append(Double(motion.dx * motion.dx + motion.dy * motion.dy).squareRoot())
            actions.append(motion.residual)
            if motion.residual > peak.action { peak = (pair.seconds, motion.residual) }
        }
        guard var result = best else { return nil }
        result.video = VideoSignals(duration: round4(duration), shake: round4(shakes.reduce(0, +) / Double(shakes.count)),
                                    action: round4(actions.reduce(0, +) / Double(actions.count)), peakSeconds: round4(peak.seconds))
        return result
    }

    static func anchorTimes(duration: Double, count: Int = 6, pairGap: Double = 0.2) -> [Double] {
        guard duration.isFinite, duration > 0 else { return [] }
        let usable = max(0, duration - pairGap)
        return (0..<count).map { round4(usable * (Double($0) + 0.5) / Double(count)) }
    }

    // MARK: - Group scoring

    struct Shot: Sendable {
        let primaryFileID: Int64
        let fileIDs: [Int64]
        let path: String
        let signals: Signals?
    }

    struct Quality: Sendable, Equatable {
        let primaryFileID: Int64
        let fileIDs: [Int64]
        let path: String
        let score: Double
        let rank: Int
        let recommended: Bool
        let reasons: [String]
        let sharpness: Double?
        let exposure: Double?
        let motionBlur: Bool
        let faceCount: Int
        let eyesClosed: Bool
        let faceCutOff: Bool
        let stability: Double?
        let action: Double?
        let subject: Box?
        let faces: [Face]
        let peakSeconds: Double?
        let highlights: Double?
        let shadows: Double?
        let framing: Double?
        let personCut: Bool
    }

    enum Reason {
        static let sharpestFace = "Sharpest face"
        static let sharpest = "Sharpest"
        static let eyesOpen = "Eyes open"
        static let allFaces = "Every face visible"
        static let bestExposure = "Best exposure"
        static let steadiest = "Steadiest"
        static let mostAction = "Most action"
        static let eyesClosed = "Eyes closed"
        static let motionBlur = "Motion blur"
        static let outOfFocus = "Out of focus"
        static let soft = "A little soft"
        static let highlights = "Bright spots blown out"
        static let tooDark = "Too dark"
        static let tooBright = "Too bright"
        static let faceCut = "Face cut off at the edge"
        static let faceHidden = "A face is hidden"
        static let shaky = "Shaky"
        static let littleAction = "Little action"
        static let unreadable = "Couldn't read this file"
        static let fillsFrame = "Subject fills the frame"
        static let everyone = "Everyone in the shot"
        static let smallSubject = "Subject small in the frame"
        static let personCut = "Someone is cut off at the edge"
    }

    static let motionBlurBelow = 0.75
    static let outOfFocusBelow = 0.6
    static let softBelow = 0.85
    static let motionAnisotropy = 0.45
    static let motionAnisotropyMargin = 0.12
    static let edgeMargin = 0.005
    static let shakyAbove = 2.0

    static func exposureScore(_ s: Signals) -> Double {
        var value = 1 - 2 * max(0, abs(s.meanLuma - 0.45) - 0.12)
        value -= 3 * max(0, s.highlights - 0.01)
        value -= 2 * max(0, s.shadows - 0.03)
        return min(1, max(0, value))
    }

    static func score(_ shots: [Shot]) -> [Quality] {
        let readable = shots.compactMap(\.signals)
        let isVideo = readable.contains { $0.video != nil }
        let maxSharp = readable.map(\.sharpness).max() ?? 0
        let everyFace = !readable.isEmpty && readable.allSatisfy { $0.faceSharpness != nil }
        let maxFaceSharp = everyFace ? readable.compactMap(\.faceSharpness).max() ?? 0 : 0
        let visible = { (s: Signals) in s.faces.filter { min($0.width * Double(s.width), $0.height * Double(s.height)) >= 16 }.count }
        let maxVisible = readable.map(visible).max() ?? 0
        let groupHasFaces = readable.contains { !primaryFaces($0.faces, width: $0.width, height: $0.height).isEmpty }
        let anisotropies = readable.map(\.anisotropy).sorted()
        let medianAnisotropy = anisotropies.isEmpty ? 0 : anisotropies[anisotropies.count / 2]
        let maxAction = readable.compactMap { $0.video?.action }.max() ?? 0
        let maxArea = readable.map(personArea).max() ?? 0
        let maxPeople = readable.map(personCount).max() ?? 0
        let groupHasPeople = !isVideo && readable.contains { ($0.people ?? []).contains { $0.height >= minPersonHeight } }

        var drafts: [(Shot, Double, Double, Quality)] = []
        for shot in shots {
            guard let s = shot.signals else {
                drafts.append((shot, -1, 0, Quality(primaryFileID: shot.primaryFileID, fileIDs: shot.fileIDs, path: shot.path, score: 0, rank: 0,
                                                    recommended: false, reasons: [Reason.unreadable], sharpness: nil, exposure: nil,
                                                    motionBlur: false, faceCount: 0, eyesClosed: false, faceCutOff: false, stability: nil,
                                                    action: nil, subject: nil, faces: [], peakSeconds: nil, highlights: nil, shadows: nil,
                                                    framing: nil, personCut: false)))
                continue
            }
            let peakRel = maxSharp > 0 ? s.sharpness / maxSharp : 1
            let faceRel = everyFace && maxFaceSharp > 0 ? (s.faceSharpness ?? 0) / maxFaceSharp : nil
            let sharpRel = round4(faceRel.map { 0.6 * $0 + 0.4 * peakRel } ?? peakRel)
            let exposure = round4(exposureScore(s))
            let primary = primaryFaces(s.faces, width: s.width, height: s.height)
            let judged = primary.filter { min($0.width * Double(s.width), $0.height * Double(s.height)) >= minEyePixels }
            let eyesClosed = judged.contains { ($0.eyesOpen ?? 1) < eyesClosedBelow }
            let faceCut = primary.contains { $0.x <= edgeMargin || $0.y <= edgeMargin || $0.x + $0.width >= 1 - edgeMargin || $0.y + $0.height >= 1 - edgeMargin }
            let motionBlur = !isVideo && sharpRel < motionBlurBelow && s.anisotropy >= max(motionAnisotropy, medianAnisotropy + motionAnisotropyMargin)
            let faceCount = visible(s)
            let largest = (s.people ?? []).max { $0.width * $0.height < $1.width * $1.height }
            let personCut = groupHasPeople && !faceCut && largest.map { $0.x <= edgeMargin || $0.x + $0.width >= 1 - edgeMargin } == true
            let framing: Double? = groupHasPeople
                ? round4(0.6 * (maxArea > 0 ? personArea(s) / maxArea : 0) + 0.4 * (maxPeople > 0 ? Double(personCount(s)) / Double(maxPeople) : 0))
                : nil
            var total: Double
            var stability: Double?, action: Double?
            if let video = s.video {
                let steady = round4(min(1, max(0, 1 - video.shake / 4)))
                let moving = round4(maxAction > 0 ? video.action / maxAction : 0)
                stability = steady; action = moving
                total = 0.35 * steady + 0.30 * moving + 0.25 * sharpRel + 0.10 * exposure
            } else if groupHasFaces {
                let qualities = primary.map { $0.quality ?? 0.5 }
                let avgQuality = qualities.isEmpty ? 0 : qualities.reduce(0, +) / Double(qualities.count)
                let faceScore = (maxVisible > 0 ? 0.5 * Double(faceCount) / Double(maxVisible) : 0) + 0.5 * avgQuality
                if let framing {
                    total = 0.35 * sharpRel + 0.15 * exposure + 0.25 * faceScore + 0.25 * framing
                } else {
                    total = 0.45 * sharpRel + 0.20 * exposure + 0.35 * faceScore
                }
                if personCut { total -= 0.05 }
                if eyesClosed { total -= 0.25 }
                if faceCut { total -= 0.10 }
                if motionBlur { total -= 0.10 }
            } else if let framing {
                total = 0.45 * sharpRel + 0.20 * exposure + 0.35 * framing
                if personCut { total -= 0.05 }
                if motionBlur { total -= 0.10 }
            } else {
                total = 0.65 * sharpRel + 0.35 * exposure
                if motionBlur { total -= 0.10 }
            }
            total = round3(min(1, max(0, total)))
            var negatives: [String] = []
            if eyesClosed { negatives.append(Reason.eyesClosed) }
            if motionBlur { negatives.append(Reason.motionBlur) }
            else if !isVideo && sharpRel < outOfFocusBelow { negatives.append(Reason.outOfFocus) }
            if faceCut { negatives.append(Reason.faceCut) }
            if personCut { negatives.append(Reason.personCut) }
            if groupHasFaces && faceCount < maxVisible { negatives.append(Reason.faceHidden) }
            if groupHasPeople, maxArea > 0, personArea(s) < 0.5 * maxArea { negatives.append(Reason.smallSubject) }
            if s.highlights > 0.05 { negatives.append(Reason.highlights) }
            if s.meanLuma < 0.20 || s.shadows > 0.25 { negatives.append(Reason.tooDark) }
            else if s.meanLuma > 0.78 { negatives.append(Reason.tooBright) }
            if let stability, stability < 1 - shakyAbove / 4 { negatives.append(Reason.shaky) }
            if let action, maxAction > 0, action < 0.4 { negatives.append(Reason.littleAction) }
            if negatives.isEmpty, !isVideo, sharpRel < softBelow { negatives.append(Reason.soft) }
            drafts.append((shot, total, sharpRel, Quality(
                primaryFileID: shot.primaryFileID, fileIDs: shot.fileIDs, path: shot.path, score: total, rank: 0, recommended: false,
                reasons: negatives, sharpness: sharpRel, exposure: exposure, motionBlur: motionBlur, faceCount: faceCount,
                eyesClosed: eyesClosed, faceCutOff: faceCut, stability: stability, action: action,
                subject: primary.first.map { Box(x: $0.x, y: $0.y, width: $0.width, height: $0.height) } ?? s.subject,
                faces: s.faces, peakSeconds: s.video?.peakSeconds, highlights: s.highlights, shadows: s.shadows,
                framing: framing, personCut: personCut)))
        }
        let order = drafts.indices.sorted { a, b in
            let l = drafts[a], r = drafts[b]
            if l.1 != r.1 { return l.1 > r.1 }
            if l.2 != r.2 { return l.2 > r.2 }
            return l.0.primaryFileID < r.0.primaryFileID
        }
        var rank = [Int](repeating: 0, count: drafts.count)
        for (position, index) in order.enumerated() { rank[index] = position + 1 }
        let winner = order.first.flatMap { drafts[$0].1 >= 0 ? $0 : nil }
        let readableDrafts = drafts.filter { $0.1 >= 0 }
        let shared = Set(readableDrafts.first?.3.reasons ?? []).filter { reason in
            readableDrafts.count > 1 && readableDrafts.allSatisfy { $0.3.reasons.contains(reason) }
        }
        let anyEyesClosed = drafts.contains { $0.3.eyesClosed }
        let maxExposure = drafts.compactMap { $0.3.exposure }.max() ?? 0
        let secondExposure = drafts.compactMap { $0.3.exposure }.sorted(by: >).dropFirst().first ?? maxExposure
        return drafts.indices.map { index in
            let q = drafts[index].3
            var reasons = q.reasons.filter { !shared.contains($0) }
            if index == winner, shots.count > 1 {
                var positives: [String] = []
                if isVideo {
                    if let s = q.stability, s >= (drafts.compactMap { $0.3.stability }.max() ?? 0) { positives.append(Reason.steadiest) }
                    if let a = q.action, a >= 1 { positives.append(Reason.mostAction) }
                    if q.sharpness == 1 && positives.isEmpty { positives.append(Reason.sharpest) }
                } else {
                    if q.sharpness == 1 { positives.append(everyFace ? Reason.sharpestFace : Reason.sharpest) }
                    if anyEyesClosed && !q.eyesClosed { positives.append(Reason.eyesOpen) }
                    if groupHasFaces && q.faceCount == maxVisible && drafts.contains(where: { $0.3.faceCount < maxVisible && $0.1 >= 0 }) {
                        positives.append(Reason.allFaces)
                    }
                    if groupHasPeople, let s = drafts[index].0.signals, maxArea > 0, personArea(s) >= maxArea,
                       readable.contains(where: { personArea($0) < 0.7 * maxArea }) { positives.append(Reason.fillsFrame) }
                    if groupHasPeople, maxPeople >= 2, let s = drafts[index].0.signals, personCount(s) == maxPeople,
                       readable.contains(where: { personCount($0) < maxPeople }) { positives.append(Reason.everyone) }
                    if let e = q.exposure, e == maxExposure, maxExposure - secondExposure >= 0.1 { positives.append(Reason.bestExposure) }
                }
                reasons = positives + reasons.filter { $0 != Reason.soft }
            }
            return Quality(primaryFileID: q.primaryFileID, fileIDs: q.fileIDs, path: q.path, score: q.score, rank: rank[index],
                           recommended: index == winner && shots.count > 1, reasons: Array(reasons.prefix(3)), sharpness: q.sharpness,
                           exposure: q.exposure, motionBlur: q.motionBlur, faceCount: q.faceCount, eyesClosed: q.eyesClosed,
                           faceCutOff: q.faceCutOff, stability: q.stability, action: q.action, subject: q.subject, faces: q.faces,
                           peakSeconds: q.peakSeconds, highlights: q.highlights, shadows: q.shadows,
                           framing: q.framing, personCut: q.personCut)
        }
    }

    static let minPersonHeight = 0.15

    static func personArea(_ s: Signals) -> Double {
        (s.people ?? []).filter { $0.height >= 0.1 }.map { $0.width * $0.height }.max() ?? 0
    }

    static func personCount(_ s: Signals) -> Int {
        (s.people ?? []).filter { $0.height >= 0.1 }.count
    }

    static func clampBox(_ box: Box) -> Box {
        let x = min(max(0, box.x), 1), y = min(max(0, box.y), 1)
        return Box(x: round4(x), y: round4(y), width: round4(min(box.width, 1 - x)), height: round4(min(box.height, 1 - y)))
    }

    static func round4(_ value: Double) -> Double { (value * 10_000).rounded() / 10_000 }
    static func round3(_ value: Double) -> Double { (value * 1_000).rounded() / 1_000 }
}
