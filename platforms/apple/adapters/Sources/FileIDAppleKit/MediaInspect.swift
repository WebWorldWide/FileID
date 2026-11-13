import AVFoundation
import CoreMedia
import Foundation

/// AVFoundation facts about a media file, in the shape the Rust engine's
/// `media_recipes::MediaProbe` reads. Mirrors the Swift engine's
/// `CommonMediaTools.inspect` (strict) and the timeline's read-only probe.
struct MediaProbe: Sendable {
    var duration: Double
    var videoCount = 0
    var audioCount = 0
    var otherCount = 0
    var width = 0.0
    var height = 0.0
    var rotation = 0
    var videoCodec: String?
    var audioCodec: String?
    var videoStart: Double?
    var audioStart: Double?
    var audioDuration: Double?
    var hdr = false
    var alpha = false
    var frameRate: Double?

    var json: [String: Any] {
        var object: [String: Any] = [
            "duration": duration, "videoCount": videoCount, "audioCount": audioCount, "otherCount": otherCount,
            "width": width, "height": height, "rotation": rotation, "hdr": hdr, "alpha": alpha,
        ]
        object["videoCodec"] = videoCodec
        object["audioCodec"] = audioCodec
        object["videoStart"] = videoStart
        object["audioStart"] = audioStart
        object["audioDuration"] = audioDuration
        object["frameRate"] = frameRate
        return object
    }
}

enum MediaInspect {
    static let unreadable = "The input is unreadable or protected."
    static let limits = "Unsupported media duration or dimensions."

    static func asset(_ url: URL, mimeType: String? = nil) -> AVURLAsset {
        var options: [String: Any] = [AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue]
        if let mimeType { options[AVURLAssetOverrideMIMETypeKey] = mimeType }
        return AVURLAsset(url: url, options: options)
    }

    /// The ffprobe-style codec name the shared verification compares.
    static func codecName(_ subtype: FourCharCode) -> String {
        switch subtype {
        case kCMVideoCodecType_H264: return "h264"
        case kCMVideoCodecType_HEVC, kCMVideoCodecType_HEVCWithAlpha: return "hevc"
        case kAudioFormatMPEG4AAC: return "aac"
        default:
            let bytes = [24, 16, 8, 0].map { UInt8((subtype >> $0) & 0xFF) }
            let text = String(decoding: bytes, as: UTF8.self).trimmingCharacters(in: .whitespaces)
            return text.isEmpty ? "unknown" : text.lowercased()
        }
    }

    static func rotation(of transform: CGAffineTransform) -> Int {
        let degrees = Int((atan2(transform.b, transform.a) * 180 / .pi).rounded())
        return ((Int((Double(degrees) / 90).rounded()) * 90) % 360 + 360) % 360
    }

    /// `strict`: the media tools' refusals (unplayable, protected, limits). Otherwise
    /// only what timeline analysis needs; extra tracks and HDR are fine there.
    static func probe(_ url: URL, mimeType: String? = nil, strict: Bool) async throws -> MediaProbe {
        let asset = asset(url, mimeType: mimeType)
        if strict {
            guard try await asset.load(.isPlayable), !(try await asset.load(.hasProtectedContent)) else { throw MediaFailure(unreadable) }
        }
        let duration = try await asset.load(.duration).seconds
        guard duration.isFinite, duration > 0, duration <= 21_600 else { throw MediaFailure(limits) }
        var probe = MediaProbe(duration: duration)
        for track in try await asset.load(.tracks) {
            switch track.mediaType {
            case .video:
                probe.videoCount += 1
                guard probe.videoCount == 1 else { continue }
                let size = try await track.load(.naturalSize)
                let transform = try await track.load(.preferredTransform)
                let display = CGRect(origin: .zero, size: size).applying(transform)
                let maximum = strict ? 8192.0 : 16_384.0
                guard display.width.isFinite, display.height.isFinite, display.width > 0, display.height > 0,
                      display.width <= maximum, display.height <= maximum,
                      !strict || display.width * display.height <= 32_000_000 else { throw MediaFailure(limits) }
                probe.width = abs(display.width)
                probe.height = abs(display.height)
                probe.rotation = rotation(of: transform)
                probe.videoCodec = try await track.load(.formatDescriptions).first.map { codecName(CMFormatDescriptionGetMediaSubType($0)) }
                let characteristics = try await track.load(.mediaCharacteristics)
                probe.hdr = characteristics.contains(.containsHDRVideo)
                probe.alpha = characteristics.contains(.containsAlphaChannel)
                let rate = try await track.load(.nominalFrameRate)
                probe.frameRate = rate.isFinite && rate > 0 ? Double(rate) : nil
                let start = try await track.load(.timeRange).start.seconds
                guard start.isFinite else { throw MediaFailure("Invalid video timestamps.") }
                probe.videoStart = start
            case .audio:
                probe.audioCount += 1
                guard probe.audioCount == 1 else { continue }
                let range = try await track.load(.timeRange)
                guard range.start.seconds.isFinite, range.duration.seconds.isFinite, range.duration.seconds > 0 else {
                    throw MediaFailure("Invalid audio timestamps.")
                }
                probe.audioCodec = try await track.load(.formatDescriptions).first.map { codecName(CMFormatDescriptionGetMediaSubType($0)) }
                probe.audioStart = range.start.seconds
                probe.audioDuration = range.duration.seconds
            case .metadata, .timecode:
                continue
            default:
                probe.otherCount += 1
            }
        }
        return probe
    }

    struct Attributes: Equatable, Sendable {
        let size: Int64
        let modified: Double
    }

    static func attributes(_ url: URL) throws -> Attributes {
        let values = try url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey])
        guard let size = values.fileSize, let modified = values.contentModificationDate else {
            throw MediaFailure("The video worker could not read the source attributes.")
        }
        return Attributes(size: Int64(size), modified: modified.timeIntervalSince1970)
    }
}
