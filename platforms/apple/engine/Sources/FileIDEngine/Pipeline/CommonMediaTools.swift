import AVFoundation
import AudioToolbox
import CoreMedia
import Foundation
import FileIDShared

enum CommonMediaTools {
    static let kinds: Set<String> = ["trim", "remux", "audio", "proxy", "rotate"]

    enum Message {
        static let videoOnlyFields = "Upscaling, framing and stabilization apply only to video export."
        static let trimOnlyFields = "Trim times and mode apply only to trim recipes."
        static let rotateOnlyFields = "Rotation applies only to rotate recipes."
        static let trimFormat = "Trim outputs must be MP4 or MOV."
        static let trimMode = "Choose keyframe or exact trimming."
        static let exactTrimFormat = "Exact trims re-encode to H.264/AAC MP4; choose MP4."
        static let trimTimes = "Choose a trim start and end time."
        static let trimNegative = "Trim times must be zero or later."
        static let trimRange = "The trim end must be at least 0.1 seconds after its start."
        static let remuxFormat = "Remux outputs must be MP4 or MOV."
        static let audioFormat = "Audio extraction writes AAC in an M4A file."
        static let proxyFormat = "Proxies are 640 or 960 pixel H.264 MP4 files."
        static let rotation = "Choose a 90, 180 or 270 degree clockwise rotation."
        static let rotateFormat = "Rotated outputs must be MP4 or MOV."
        static let input = "Media tools accept MP4, MOV or M4V inputs."
        static let unreadable = "The input is unreadable or protected."
        static let noVideo = "This file has no video track."
        static let noAudio = "This file has no audio track."
        static let tracks = "Media tools support one video track and at most one audio track; other tracks are refused rather than dropped."
        static let copyCodecs = "Lossless copy supports H.264 or HEVC video with AAC audio; these codecs would need re-encoding, which this recipe does not do."
        static let trimPastEnd = "The trim end is after the end of this video."
        static let hdr = "HDR sources need a color-managed adapter; this recipe would re-encode them to SDR."
        static let limits = "Unsupported media duration or dimensions."
        static func alreadyContainer(_ format: String) -> String { "This file is already \(format.uppercased()); remuxing would only copy it." }
    }

    struct Probe: Sendable, Equatable {
        var duration: Double
        var videoCount: Int
        var audioCount: Int
        var otherTrackCount: Int
        var width: Double
        var height: Double
        var rotation: Int
        var videoCodec: UInt32?
        var audioCodec: UInt32?
        var videoStart: Double?
        var audioStart: Double?
        var audioDuration: Double?
        var hdr: Bool
    }

    static func validate(_ recipe: ToolRecipe) throws {
        guard kinds.contains(recipe.kind) else { throw MediaTools.Failure(text: "Unsupported media recipe.") }
        guard recipe.allowUpscale == nil, recipe.videoAspectRatio == nil, recipe.videoFraming == nil, recipe.videoStabilization == nil else {
            throw MediaTools.Failure(text: Message.videoOnlyFields)
        }
        guard recipe.kind == "trim" || (recipe.trimStartSeconds == nil && recipe.trimEndSeconds == nil && recipe.trimMode == nil) else {
            throw MediaTools.Failure(text: Message.trimOnlyFields)
        }
        guard recipe.kind == "rotate" || recipe.rotationDegrees == nil else { throw MediaTools.Failure(text: Message.rotateOnlyFields) }
        guard (1...8192).contains(recipe.maxDimension) else { throw MediaTools.Failure(text: "Unsupported media recipe.") }
        switch recipe.kind {
        case "trim":
            guard ["mp4", "mov"].contains(recipe.format) else { throw MediaTools.Failure(text: Message.trimFormat) }
            guard let mode = recipe.trimMode, ["keyframe", "exact"].contains(mode) else { throw MediaTools.Failure(text: Message.trimMode) }
            guard mode == "keyframe" || recipe.format == "mp4" else { throw MediaTools.Failure(text: Message.exactTrimFormat) }
            guard let start = recipe.trimStartSeconds, let end = recipe.trimEndSeconds, start.isFinite, end.isFinite else { throw MediaTools.Failure(text: Message.trimTimes) }
            guard start >= 0 else { throw MediaTools.Failure(text: Message.trimNegative) }
            guard end - start >= 0.1 else { throw MediaTools.Failure(text: Message.trimRange) }
        case "remux":
            guard ["mp4", "mov"].contains(recipe.format) else { throw MediaTools.Failure(text: Message.remuxFormat) }
        case "audio":
            guard recipe.format == "m4a" else { throw MediaTools.Failure(text: Message.audioFormat) }
        case "proxy":
            guard recipe.format == "mp4", [640, 960].contains(recipe.maxDimension) else { throw MediaTools.Failure(text: Message.proxyFormat) }
        default:
            guard let degrees = recipe.rotationDegrees, [90, 180, 270].contains(degrees) else { throw MediaTools.Failure(text: Message.rotation) }
            guard ["mp4", "mov"].contains(recipe.format) else { throw MediaTools.Failure(text: Message.rotateFormat) }
        }
    }

    static func label(_ recipe: ToolRecipe) -> String {
        switch recipe.kind {
        case "trim": " - Trim"
        case "remux": " - Remux"
        case "audio": " - Audio"
        case "proxy": " - Proxy"
        default: " - Rotated \(recipe.rotationDegrees ?? 0)"
        }
    }

    static func outputKind(_ recipe: ToolRecipe) -> String { recipe.kind == "audio" ? "audio" : "video" }
    static func role(_ recipe: ToolRecipe) -> String { recipe.kind == "proxy" ? "proxy" : "conversion" }

    static func sourceContainer(_ url: URL) -> String? {
        switch url.pathExtension.lowercased() {
        case "mp4", "m4v": "mp4"
        case "mov": "mov"
        default: nil
        }
    }

    private static let copyVideoCodecs: Set<UInt32> = [kCMVideoCodecType_H264, kCMVideoCodecType_HEVC]

    static func checkSource(_ recipe: ToolRecipe, source: URL, probe: Probe) throws {
        guard let container = sourceContainer(source) else { throw MediaTools.Failure(text: Message.input) }
        guard probe.otherTrackCount == 0, probe.videoCount <= 1, probe.audioCount <= 1 else { throw MediaTools.Failure(text: Message.tracks) }
        if recipe.kind == "audio" {
            guard probe.audioCount == 1 else { throw MediaTools.Failure(text: Message.noAudio) }
            return
        }
        guard probe.videoCount == 1 else { throw MediaTools.Failure(text: Message.noVideo) }
        let copies = recipe.kind == "remux" || recipe.kind == "rotate" || (recipe.kind == "trim" && recipe.trimMode == "keyframe")
        if copies {
            guard let video = probe.videoCodec, copyVideoCodecs.contains(video),
                  probe.audioCount == 0 || probe.audioCodec == kAudioFormatMPEG4AAC else { throw MediaTools.Failure(text: Message.copyCodecs) }
        } else if probe.hdr {
            throw MediaTools.Failure(text: Message.hdr)
        }
        if recipe.kind == "remux", container == recipe.format { throw MediaTools.Failure(text: Message.alreadyContainer(recipe.format)) }
        if recipe.kind == "trim", let end = recipe.trimEndSeconds, end > probe.duration + 0.05 { throw MediaTools.Failure(text: Message.trimPastEnd) }
    }

    static func expectedDuration(_ recipe: ToolRecipe, source: Probe) -> Double {
        guard recipe.kind == "trim", let start = recipe.trimStartSeconds, let end = recipe.trimEndSeconds else {
            return recipe.kind == "audio" ? (expectedAudioDuration(recipe, source: source) ?? source.duration) : source.duration
        }
        return min(end, source.duration) - start
    }

    static func expectedSync(_ recipe: ToolRecipe, source: Probe) -> Double? {
        guard let video = source.videoStart, let audio = source.audioStart else { return nil }
        let start = recipe.kind == "trim" ? (recipe.trimStartSeconds ?? 0) : 0
        return max(0, audio - start) - max(0, video - start)
    }

    static func expectedAudioDuration(_ recipe: ToolRecipe, source: Probe) -> Double? {
        guard let audioStart = source.audioStart, let audioDuration = source.audioDuration else { return nil }
        let start = recipe.kind == "trim" ? (recipe.trimStartSeconds ?? 0) : 0
        let end = recipe.kind == "trim" ? min(recipe.trimEndSeconds ?? source.duration, source.duration) : .infinity
        return max(0, min(audioStart + audioDuration, end) - max(audioStart, start))
    }

    static func verify(_ recipe: ToolRecipe, source: Probe, output: Probe) throws {
        let duration = expectedDuration(recipe, source: source)
        let tolerance = ["remux", "rotate"].contains(recipe.kind) ? 0.1 : 0.25
        if recipe.kind == "audio" {
            guard output.videoCount == 0, output.audioCount == 1, output.otherTrackCount == 0, output.audioCodec == kAudioFormatMPEG4AAC else {
                throw MediaTools.Failure(text: "Output validation rejected the audio streams or codec.")
            }
            guard let length = output.audioDuration, abs(length - duration) <= tolerance else {
                throw MediaTools.Failure(text: "Output validation rejected changed audio duration.")
            }
            return
        }
        guard output.videoCount == 1, output.audioCount == source.audioCount, output.otherTrackCount == 0 else {
            throw MediaTools.Failure(text: "Output validation rejected changed stream counts.")
        }
        switch recipe.kind {
        case "remux", "rotate":
            guard output.videoCodec == source.videoCodec, output.audioCodec == source.audioCodec else { throw MediaTools.Failure(text: "Output validation rejected re-encoded streams.") }
        case "trim" where recipe.trimMode == "keyframe":
            guard output.videoCodec == source.videoCodec, output.audioCodec == source.audioCodec else { throw MediaTools.Failure(text: "Output validation rejected re-encoded streams.") }
        default:
            guard output.videoCodec == kCMVideoCodecType_H264, source.audioCount == 0 || output.audioCodec == kAudioFormatMPEG4AAC else {
                throw MediaTools.Failure(text: "Output validation rejected the codec.")
            }
        }
        guard abs(output.duration - duration) <= tolerance else { throw MediaTools.Failure(text: "Output validation rejected changed duration.") }
        if let expected = expectedSync(recipe, source: source) {
            guard let video = output.videoStart, let audio = output.audioStart, abs((audio - video) - expected) <= 0.1,
                  let expectedLength = expectedAudioDuration(recipe, source: source), let length = output.audioDuration,
                  abs(length - expectedLength) <= 0.25 else {
                throw MediaTools.Failure(text: "Output validation rejected audio/video sync.")
            }
        }
        let rotation = recipe.kind == "rotate" ? (source.rotation + (recipe.rotationDegrees ?? 0)) % 360 : source.rotation
        let quarterTurn = (rotation - source.rotation + 360) % 180 == 90
        let expectedRatio = quarterTurn ? source.height / source.width : source.width / source.height
        guard output.rotation == rotation, abs(output.width / output.height - expectedRatio) <= 0.02 * expectedRatio else {
            throw MediaTools.Failure(text: "Output validation rejected changed orientation or aspect.")
        }
        if recipe.kind == "proxy" {
            guard max(output.width, output.height) <= Double(recipe.maxDimension) + 1 else { throw MediaTools.Failure(text: "Output validation rejected the proxy dimensions.") }
        } else if ["remux", "rotate"].contains(recipe.kind) || recipe.trimMode == "keyframe" {
            guard abs(max(output.width, output.height) - max(source.width, source.height)) <= 1 else { throw MediaTools.Failure(text: "Output validation rejected changed dimensions.") }
        }
    }

    static func inspect(_ url: URL, mimeType: String? = nil) async throws -> Probe {
        var options: [String: Any] = [AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue]
        if let mimeType { options[AVURLAssetOverrideMIMETypeKey] = mimeType }
        let asset = AVURLAsset(url: url, options: options)
        guard try await asset.load(.isPlayable), !(try await asset.load(.hasProtectedContent)) else { throw MediaTools.Failure(text: Message.unreadable) }
        let duration = try await asset.load(.duration).seconds
        guard duration.isFinite, duration > 0, duration <= 21_600 else { throw MediaTools.Failure(text: Message.limits) }
        var probe = Probe(duration: duration, videoCount: 0, audioCount: 0, otherTrackCount: 0, width: 0, height: 0, rotation: 0, hdr: false)
        for track in try await asset.load(.tracks) {
            switch track.mediaType {
            case .video:
                probe.videoCount += 1
                guard probe.videoCount == 1 else { continue }
                let size = try await track.load(.naturalSize)
                let transform = try await track.load(.preferredTransform)
                let display = CGRect(origin: .zero, size: size).applying(transform)
                guard display.width.isFinite, display.height.isFinite, display.width > 0, display.height > 0,
                      display.width <= 8192, display.height <= 8192, display.width * display.height <= 32_000_000 else {
                    throw MediaTools.Failure(text: Message.limits)
                }
                probe.width = display.width
                probe.height = display.height
                probe.rotation = rotation(of: transform)
                probe.videoCodec = try await track.load(.formatDescriptions).first.map(CMFormatDescriptionGetMediaSubType)
                probe.hdr = try await track.load(.mediaCharacteristics).contains(.containsHDRVideo)
                let start = try await track.load(.timeRange).start.seconds
                guard start.isFinite else { throw MediaTools.Failure(text: "Invalid video timestamps.") }
                probe.videoStart = start
            case .audio:
                probe.audioCount += 1
                guard probe.audioCount == 1 else { continue }
                let range = try await track.load(.timeRange)
                guard range.start.seconds.isFinite, range.duration.seconds.isFinite, range.duration.seconds > 0 else {
                    throw MediaTools.Failure(text: "Invalid audio timestamps.")
                }
                probe.audioCodec = try await track.load(.formatDescriptions).first.map(CMFormatDescriptionGetMediaSubType)
                probe.audioStart = range.start.seconds
                probe.audioDuration = range.duration.seconds
            case .metadata, .timecode:
                continue
            default:
                probe.otherTrackCount += 1
            }
        }
        return probe
    }

    static func rotation(of transform: CGAffineTransform) -> Int {
        let degrees = Int((atan2(transform.b, transform.a) * 180 / .pi).rounded())
        return ((Int((Double(degrees) / 90).rounded()) * 90) % 360 + 360) % 360
    }

    static func summary(_ recipe: ToolRecipe) -> String {
        switch recipe.kind {
        case "trim" where recipe.trimMode == "exact": "Re-encoded to H.264/AAC for a frame-accurate cut."
        case "trim": "Streams copied without re-encoding."
        case "remux": "Streams copied into a new container without re-encoding."
        case "audio": "Audio converted to AAC."
        case "proxy": "Small H.264 preview copy linked to its source."
        default: "Orientation changed; streams copied without re-encoding."
        }
    }

    static func preview(source: URL, recipe: ToolRecipe) async throws -> String {
        try validate(recipe)
        guard sourceContainer(source) != nil else { throw MediaTools.Failure(text: Message.input) }
        try checkSource(recipe, source: source, probe: try await inspect(source))
        return summary(recipe)
    }

    static func export(source: URL, output: URL, recipe: ToolRecipe) async throws -> String {
        try validate(recipe)
        try ReadOnlyLocations.requireWritable(output)
        guard !FileManager.default.fileExists(atPath: output.path) else { throw MediaTools.Failure(text: "The staged output is occupied.") }
        guard sourceContainer(source) != nil else { throw MediaTools.Failure(text: Message.input) }
        let asset = AVURLAsset(url: source, options: [AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue])
        let before = try await inspect(source)
        try checkSource(recipe, source: source, probe: before)
        let range: CMTimeRange
        if recipe.kind == "trim", let start = recipe.trimStartSeconds, let end = recipe.trimEndSeconds {
            range = CMTimeRange(start: CMTime(seconds: start, preferredTimescale: 600), end: CMTime(seconds: min(end, before.duration), preferredTimescale: 600))
        } else {
            range = CMTimeRange(start: .zero, duration: try await asset.load(.duration))
        }
        let composition = AVMutableComposition()
        if recipe.kind != "audio", let video = try await asset.loadTracks(withMediaType: .video).first {
            guard let track = composition.addMutableTrack(withMediaType: .video, preferredTrackID: kCMPersistentTrackID_Invalid) else { throw MediaTools.Failure(text: "The video track could not be prepared.") }
            try track.insertTimeRange(range, of: video, at: .zero)
            var transform = try await video.load(.preferredTransform)
            if recipe.kind == "rotate", let degrees = recipe.rotationDegrees {
                let size = try await video.load(.naturalSize)
                transform = transform.concatenating(CGAffineTransform(rotationAngle: CGFloat(degrees) * .pi / 180))
                let bounds = CGRect(origin: .zero, size: size).applying(transform)
                transform = transform.concatenating(CGAffineTransform(translationX: -bounds.minX, y: -bounds.minY))
            }
            track.preferredTransform = transform
        }
        if let audio = try await asset.loadTracks(withMediaType: .audio).first {
            guard let track = composition.addMutableTrack(withMediaType: .audio, preferredTrackID: kCMPersistentTrackID_Invalid) else { throw MediaTools.Failure(text: "The audio track could not be prepared.") }
            try track.insertTimeRange(range, of: audio, at: .zero)
        }
        let preset: String
        let fileType: AVFileType
        let mimeType: String
        switch recipe.kind {
        case "audio":
            (preset, fileType, mimeType) = (AVAssetExportPresetAppleM4A, .m4a, "audio/mp4")
        case "proxy":
            (preset, fileType, mimeType) = (recipe.maxDimension == 640 ? AVAssetExportPreset640x480 : AVAssetExportPreset960x540, .mp4, "video/mp4")
        case "trim" where recipe.trimMode == "exact":
            (preset, fileType, mimeType) = (AVAssetExportPresetHighestQuality, .mp4, "video/mp4")
        default:
            (preset, fileType, mimeType) = (AVAssetExportPresetPassthrough, recipe.format == "mov" ? .mov : .mp4, recipe.format == "mov" ? "video/quicktime" : "video/mp4")
        }
        guard let exporter = AVAssetExportSession(asset: composition, presetName: preset), exporter.supportedFileTypes.contains(fileType) else {
            throw MediaTools.Failure(text: preset == AVAssetExportPresetPassthrough ? Message.copyCodecs : "This file cannot use the native export preset for this recipe.")
        }
        exporter.metadata = []
        exporter.metadataItemFilter = .forSharing()
        exporter.shouldOptimizeForNetworkUse = fileType != .mov
        try await exporter.export(to: output, as: fileType)
        let after = try await inspect(output, mimeType: mimeType)
        try verify(recipe, source: before, output: after)
        return summary(recipe)
    }
}
