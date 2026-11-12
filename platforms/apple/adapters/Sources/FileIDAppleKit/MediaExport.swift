import AVFoundation
import CoreMedia
import Foundation

/// The recipe fields the exports read (the IPC `ToolRecipe`). Validation is the
/// Rust engine's; the adapter re-checks only what would make AVFoundation unsafe.
struct MediaRecipe: Sendable {
    var kind: String
    var format: String
    var maxDimension: Int
    var allowUpscale = false
    var aspectRatio = "source"
    var framing = "fit"
    var stabilize = false
    var trimStart: Double?
    var trimEnd: Double?
    var trimMode: String?
    var rotation: Int?

    init(_ object: [String: Any]) throws {
        guard let kind = object["kind"] as? String, let format = object["format"] as? String,
              let dimension = (object["maxDimension"] as? NSNumber)?.intValue else {
            throw MediaFailure("Unsupported media recipe.")
        }
        self.kind = kind
        self.format = format
        maxDimension = dimension
        allowUpscale = (object["allowUpscale"] as? Bool) ?? false
        aspectRatio = (object["videoAspectRatio"] as? String) ?? "source"
        framing = (object["videoFraming"] as? String) ?? "fit"
        stabilize = (object["videoStabilization"] as? Bool) ?? false
        trimStart = (object["trimStartSeconds"] as? NSNumber)?.doubleValue
        trimEnd = (object["trimEndSeconds"] as? NSNumber)?.doubleValue
        trimMode = object["trimMode"] as? String
        rotation = (object["rotationDegrees"] as? NSNumber)?.intValue
    }
}

enum MediaExport {
    static let commonKinds: Set<String> = ["trim", "remux", "audio", "proxy", "rotate"]

    /// Polls the caller's flag while AVAssetExportSession runs; cancelling the
    /// export task stops the session.
    private static func run(_ exporter: AVAssetExportSession, to output: URL, as type: AVFileType, cancel: CancelFlag) async throws {
        let cancelled = MediaFailure("Export cancelled before publication")
        if cancel.isSet { throw cancelled }
        let session = SendableExporter(exporter)
        let export = Task { try await session.value.export(to: output, as: type) }
        let watcher = Task.detached {
            while !Task.isCancelled {
                if cancel.isSet { export.cancel(); return }
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
        defer { watcher.cancel() }
        do {
            try await export.value
        } catch {
            if cancel.isSet { throw cancelled }
            throw error
        }
        if cancel.isSet { throw cancelled }
    }

    private struct SendableExporter: @unchecked Sendable {
        let value: AVAssetExportSession
        init(_ value: AVAssetExportSession) { self.value = value }
    }

    static func requireFreshOutput(_ output: URL) throws {
        guard !FileManager.default.fileExists(atPath: output.path) else { throw MediaFailure("The staged output is occupied.") }
    }

    /// Trim, remux, audio, proxy and rotate; the Swift engine's `CommonMediaTools.export`.
    static func common(source: URL, output: URL, recipe: MediaRecipe, cancel: CancelFlag) async throws -> MediaProbe {
        guard commonKinds.contains(recipe.kind) else { throw MediaFailure("Unsupported media recipe.") }
        try requireFreshOutput(output)
        let asset = MediaInspect.asset(source)
        let before = try await MediaInspect.probe(source, strict: true)
        let range: CMTimeRange
        if recipe.kind == "trim", let start = recipe.trimStart, let end = recipe.trimEnd {
            range = CMTimeRange(start: CMTime(seconds: start, preferredTimescale: 600), end: CMTime(seconds: min(end, before.duration), preferredTimescale: 600))
        } else {
            range = CMTimeRange(start: .zero, duration: try await asset.load(.duration))
        }
        let composition = AVMutableComposition()
        if recipe.kind != "audio", let video = try await asset.loadTracks(withMediaType: .video).first {
            guard let track = composition.addMutableTrack(withMediaType: .video, preferredTrackID: kCMPersistentTrackID_Invalid) else {
                throw MediaFailure("The video track could not be prepared.")
            }
            try track.insertTimeRange(range, of: video, at: .zero)
            var transform = try await video.load(.preferredTransform)
            if recipe.kind == "rotate", let degrees = recipe.rotation {
                let size = try await video.load(.naturalSize)
                transform = transform.concatenating(CGAffineTransform(rotationAngle: CGFloat(degrees) * .pi / 180))
                let bounds = CGRect(origin: .zero, size: size).applying(transform)
                transform = transform.concatenating(CGAffineTransform(translationX: -bounds.minX, y: -bounds.minY))
            }
            track.preferredTransform = transform
        }
        if let audio = try await asset.loadTracks(withMediaType: .audio).first {
            guard let track = composition.addMutableTrack(withMediaType: .audio, preferredTrackID: kCMPersistentTrackID_Invalid) else {
                throw MediaFailure("The audio track could not be prepared.")
            }
            try track.insertTimeRange(range, of: audio, at: .zero)
        }
        let preset: String
        let fileType: AVFileType
        switch recipe.kind {
        case "audio": (preset, fileType) = (AVAssetExportPresetAppleM4A, .m4a)
        case "proxy": (preset, fileType) = (recipe.maxDimension == 640 ? AVAssetExportPreset640x480 : AVAssetExportPreset960x540, .mp4)
        case "trim" where recipe.trimMode == "exact": (preset, fileType) = (AVAssetExportPresetHighestQuality, .mp4)
        default: (preset, fileType) = (AVAssetExportPresetPassthrough, recipe.format == "mov" ? .mov : .mp4)
        }
        guard let exporter = AVAssetExportSession(asset: composition, presetName: preset), exporter.supportedFileTypes.contains(fileType) else {
            throw MediaFailure(preset == AVAssetExportPresetPassthrough
                ? "Lossless copy supports H.264 or HEVC video with AAC audio; these codecs would need re-encoding, which this recipe does not do."
                : "This file cannot use the native export preset for this recipe.")
        }
        exporter.metadata = []
        exporter.metadataItemFilter = .forSharing()
        exporter.shouldOptimizeForNetworkUse = fileType != .mov
        try await run(exporter, to: output, as: fileType, cancel: cancel)
        return before
    }

    private static func frameRatio(_ aspectRatio: String) -> Double? {
        switch aspectRatio {
        case "9:16": 9.0 / 16.0
        case "16:9": 16.0 / 9.0
        case "1:1": 1.0
        case "4:5": 4.0 / 5.0
        default: nil
        }
    }

    struct Converted: Sendable {
        var probe: MediaProbe
        var framingSummary: String?
        var stabilizationSummary: String?
        var summary: String { [framingSummary, stabilizationSummary].compactMap { $0 }.joined(separator: " ") }
    }

    /// The video export (fit or pad to a frame, optional stabilization and people
    /// framing); the Swift engine's `VideoConversionWorker`. `output == nil` probes.
    static func video(source: URL, output: URL?, recipe: MediaRecipe, cancel: CancelFlag) async throws -> Converted {
        let dimension = recipe.maxDimension
        guard recipe.kind == "video", recipe.format == "mp4", [1280, 1920, 3840].contains(dimension) else {
            throw MediaFailure("Choose a 1280, 1920 or 3840 pixel video export.")
        }
        guard ["source", "9:16", "16:9", "1:1", "4:5"].contains(recipe.aspectRatio) else { throw MediaFailure("Choose a supported video frame.") }
        guard ["fit", "auto"].contains(recipe.framing), recipe.framing != "auto" || recipe.aspectRatio != "source" else {
            throw MediaFailure("Choose a social frame before following people.")
        }
        if let output { try requireFreshOutput(output) }
        guard ["mp4", "mov", "m4v"].contains(source.pathExtension.lowercased()) else { throw MediaFailure("Video inputs must be MP4, MOV or M4V.") }
        let asset = MediaInspect.asset(source)
        let before = try await inspectVideo(source)
        var converted = Converted(probe: before)
        let preset = dimension == 1280 ? AVAssetExportPreset1280x720 : dimension == 1920 ? AVAssetExportPreset1920x1080 : AVAssetExportPreset3840x2160
        guard let exporter = AVAssetExportSession(asset: asset, presetName: preset), exporter.supportedFileTypes.contains(.mp4) else {
            throw MediaFailure("This video cannot use the selected native MP4 preset.")
        }
        let size = CGSize(width: before.width, height: before.height)
        if recipe.stabilize {
            guard before.duration <= 600, let rate = before.frameRate, rate > 0, rate <= 60 else {
                throw MediaFailure("Stabilization currently supports clips up to ten minutes and 60 frames per second.")
            }
            converted.stabilizationSummary = "Stabilization analyzes translation and rotation during export, with up to 12% edge-covering zoom. Unsafe or unavailable registrations remain uncorrected."
        }
        if output != nil || !recipe.stabilize {
            let stabilization = recipe.stabilize
                ? try MediaStabilization.analyze(asset: asset, duration: before.duration, sourceSize: size, frameRate: before.frameRate ?? 0, cancel: cancel)
                : nil
            if let stabilization { converted.stabilizationSummary = stabilization.summary }
            if let ratio = frameRatio(recipe.aspectRatio) ?? (recipe.stabilize || recipe.allowUpscale ? before.width / before.height : nil) {
                let plan = recipe.framing == "auto"
                    ? try MediaReframing.analyze(asset: asset, duration: before.duration, sourceSize: size, ratio: ratio, stabilization: stabilization, cancel: cancel)
                    : nil
                converted.framingSummary = plan?.summary
                exporter.videoComposition = try await MediaReframing.composition(asset: asset, dimension: dimension, ratio: ratio, plan: plan, stabilization: stabilization)
            }
        } else if recipe.framing == "auto" {
            converted.framingSummary = "People framing follows the stabilized canvas during export."
        }
        guard let output else { return converted }
        exporter.metadata = []
        exporter.metadataItemFilter = .forSharing()
        exporter.shouldOptimizeForNetworkUse = true
        try await run(exporter, to: output, as: .mp4, cancel: cancel)
        let after = try await inspectVideo(output, mimeType: "video/mp4")
        let expectedRatio = frameRatio(recipe.aspectRatio) ?? before.width / before.height
        guard abs(after.duration - before.duration) <= 0.25, after.audioCount == before.audioCount,
              after.videoCodec == "h264", max(after.width, after.height) <= Double(dimension) + 2,
              abs(after.width / after.height - expectedRatio) <= 0.02 else {
            throw MediaFailure("Export validation rejected changed timing, streams, codec or orientation.")
        }
        if before.audioCount == 1 {
            guard after.audioCodec == "aac",
                  let beforeStart = before.audioStart, let afterStart = after.audioStart,
                  let beforeVideo = before.videoStart, let afterVideo = after.videoStart,
                  let beforeDuration = before.audioDuration, let afterDuration = after.audioDuration,
                  abs((afterStart - afterVideo) - (beforeStart - beforeVideo)) <= 0.1,
                  abs(afterDuration - beforeDuration) <= 0.25 else {
                throw MediaFailure("Export validation rejected changed audio timing or codec.")
            }
        }
        converted.probe = after
        return converted
    }

    /// The video export's input rules (one video, at most one audio, no subtitles,
    /// alpha or HDR); the Swift engine's `VideoConversionWorker.inspect`.
    static func inspectVideo(_ url: URL, mimeType: String? = nil) async throws -> MediaProbe {
        let asset = MediaInspect.asset(url, mimeType: mimeType)
        guard try await asset.load(.isPlayable), !(try await asset.load(.hasProtectedContent)) else { throw MediaFailure(MediaInspect.unreadable) }
        let probe: MediaProbe
        do { probe = try await MediaInspect.probe(url, mimeType: mimeType, strict: true) } catch let error as MediaFailure where error.text == MediaInspect.limits {
            throw MediaFailure("Unsupported video duration or dimensions.")
        }
        guard probe.otherCount == 0 else { throw MediaFailure("Subtitle, metadata and auxiliary tracks require a future export adapter.") }
        guard probe.videoCount == 1, probe.audioCount <= 1 else { throw MediaFailure("Export supports one video track and at most one audio track.") }
        guard !probe.alpha else { throw MediaFailure("Video alpha-channel export is not yet supported.") }
        guard !probe.hdr else { throw MediaFailure("HDR export is not yet supported; use an SDR source.") }
        return probe
    }
}
