import AVFoundation
import AudioToolbox
import CoreMedia
import Darwin
import Foundation
import FileIDShared

enum VideoConversionWorker {
    struct Probe: Codable, Sendable {
        var duration: Double
        var width: Double
        var height: Double
        var audioCount: Int
        var codec: UInt32
        var videoStart: Double
        var audioCodec: UInt32?
        var audioStart: Double?
        var audioDuration: Double?
        var framingSummary: String?
        var stabilizationSummary: String?
        var frameRate: Double?
        var toolSummary: String { [framingSummary, stabilizationSummary].compactMap { $0 }.joined(separator: " ") }
    }
    struct Report: Codable, Sendable {
        var probe: Probe?
        var message: String?
    }

    static func run(arguments: [String]) async -> Int32 {
        let parentPID = getppid()
        let monitor = Task.detached {
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(1)) } catch { return }
                if getppid() == 1 || getppid() != parentPID { kill(getpid(), SIGKILL); return }
            }
        }
        defer { monitor.cancel() }
        do {
            guard (4...8).contains(arguments.count), ["probe", "export"].contains(arguments[0]),
                  let dimension = Int(arguments[2]), [1280,1920,3840].contains(dimension) else {
                throw MediaTools.Failure(text: "Choose a 1280, 1920 or 3840 pixel video export.")
            }
            let aspectRatio = arguments.count >= 5 ? arguments[4] : "source"
            guard ["source", "9:16", "16:9", "1:1", "4:5"].contains(aspectRatio) else {
                throw MediaTools.Failure(text: "Choose a supported video frame.")
            }
            let framing = arguments.count >= 6 ? arguments[5] : "fit"
            guard ["fit", "auto"].contains(framing), framing != "auto" || aspectRatio != "source" else {
                throw MediaTools.Failure(text: "Choose a social frame before following people.")
            }
            let stabilizationFlag = arguments.count >= 7 ? arguments[6] : "0"
            guard ["0", "1"].contains(stabilizationFlag) else { throw MediaTools.Failure(text: "Invalid stabilization selection.") }
            let stabilize = stabilizationFlag == "1"
            let enlargementFlag = arguments.count == 8 ? arguments[7] : "0"
            guard ["0", "1"].contains(enlargementFlag) else { throw MediaTools.Failure(text: "Invalid enlargement selection.") }
            let enlarge = enlargementFlag == "1"
            let source = URL(fileURLWithPath: arguments[1])
            let output = URL(fileURLWithPath: arguments[3])
            if arguments[0] == "export" {
                try ReadOnlyLocations.requireSourceMutation(output)
                guard !FileManager.default.fileExists(atPath: output.path) else { throw MediaTools.Failure(text: "The staged output is occupied.") }
            }
            guard ["mp4","mov","m4v"].contains(source.pathExtension.lowercased()) else { throw MediaTools.Failure(text: "Video inputs must be MP4, MOV or M4V.") }
            let asset = AVURLAsset(url: source, options: [AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue])
            var before = try await inspect(asset)
            let preset = dimension == 1280 ? AVAssetExportPreset1280x720 : dimension == 1920 ? AVAssetExportPreset1920x1080 : AVAssetExportPreset3840x2160
            guard let exporter = AVAssetExportSession(asset: asset, presetName: preset), exporter.supportedFileTypes.contains(.mp4) else {
                throw MediaTools.Failure(text: "This video cannot use the selected native MP4 preset.")
            }
            if stabilize {
                guard before.duration <= 600, let rate = before.frameRate, rate > 0, rate <= 60 else {
                    throw MediaTools.Failure(text: "Stabilization currently supports clips up to ten minutes and 60 frames per second.")
                }
                before.stabilizationSummary = "Stabilization analyzes translation and rotation during export, with up to 12% edge-covering zoom. Unsafe or unavailable registrations remain uncorrected."
            }
            if arguments[0] == "export" || !stabilize {
                let stabilization = stabilize ? try VideoStabilization.analyze(asset: asset, duration: before.duration, sourceSize: CGSize(width: before.width, height: before.height), frameRate: before.frameRate ?? 0) : nil
                if let stabilization { before.stabilizationSummary = stabilization.summary }
                if let ratio = frameRatio(aspectRatio) ?? (stabilize || enlarge ? before.width / before.height : nil) {
                    let plan = framing == "auto" ? try VideoReframing.analyze(asset: asset, duration: before.duration, sourceSize: CGSize(width: before.width, height: before.height), ratio: ratio, stabilization: stabilization) : nil
                    before.framingSummary = plan?.summary
                    exporter.videoComposition = try await VideoReframing.composition(asset: asset, dimension: dimension, ratio: ratio, plan: plan, stabilization: stabilization)
                }
            } else if framing == "auto" {
                before.framingSummary = "People framing follows the stabilized canvas during export."
            }
            if arguments[0] == "export" {
                exporter.metadata = []
                exporter.shouldOptimizeForNetworkUse = true
                try await exporter.export(to: output, as: .mp4)
                var after = try await inspect(AVURLAsset(url: output, options: [AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue, AVURLAssetOverrideMIMETypeKey: "video/mp4"]))
                after.framingSummary = before.framingSummary
                after.stabilizationSummary = before.stabilizationSummary
                let expectedRatio = frameRatio(aspectRatio) ?? before.width / before.height
                guard abs(after.duration - before.duration) <= 0.25, after.audioCount == before.audioCount,
                      after.codec == kCMVideoCodecType_H264, max(after.width, after.height) <= Double(dimension) + 2,
                      abs(after.width / after.height - expectedRatio) <= 0.02 else {
                    throw MediaTools.Failure(text: "Export validation rejected changed timing, streams, codec or orientation.")
                }
                if before.audioCount == 1 {
                    guard after.audioCodec == kAudioFormatMPEG4AAC,
                          let beforeStart = before.audioStart, let afterStart = after.audioStart,
                          let beforeDuration = before.audioDuration, let afterDuration = after.audioDuration,
                          abs((afterStart - after.videoStart) - (beforeStart - before.videoStart)) <= 0.1,
                          abs(afterDuration - beforeDuration) <= 0.25 else {
                        throw MediaTools.Failure(text: "Export validation rejected changed audio timing or codec.")
                    }
                }
                try FileHandle.standardOutput.write(contentsOf: JSONEncoder().encode(Report(probe: after)))
            } else {
                try FileHandle.standardOutput.write(contentsOf: JSONEncoder().encode(Report(probe: before)))
            }
            return 0
        } catch {
            let report = Report(message: String(error.localizedDescription.prefix(800)))
            if let data = try? JSONEncoder().encode(report) { try? FileHandle.standardOutput.write(contentsOf: data) }
            return 3
        }
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

    private static func inspect(_ asset: AVURLAsset) async throws -> Probe {
        guard try await asset.load(.isPlayable), !(try await asset.load(.hasProtectedContent)) else {
            throw MediaTools.Failure(text: "The input is unreadable or protected.")
        }
        let tracks = try await asset.load(.tracks)
        var videos: [AVAssetTrack] = []
        var audioCount = 0
        var audio: AVAssetTrack?
        for track in tracks {
            switch track.mediaType {
            case .video: videos.append(track)
            case .audio: audioCount += 1; audio = track
            default: throw MediaTools.Failure(text: "Subtitle, metadata and auxiliary tracks require a future export adapter.")
            }
        }
        guard videos.count == 1, audioCount <= 1 else { throw MediaTools.Failure(text: "Export supports one video track and at most one audio track.") }
        let video = videos[0]
        let characteristics = try await video.load(.mediaCharacteristics)
        guard !characteristics.contains(.containsAlphaChannel) else { throw MediaTools.Failure(text: "Video alpha-channel export is not yet supported.") }
        guard !characteristics.contains(.containsHDRVideo) else { throw MediaTools.Failure(text: "HDR export is not yet supported; use an SDR source.") }
        let duration = try await asset.load(.duration).seconds
        let size = try await video.load(.naturalSize)
        let transform = try await video.load(.preferredTransform)
        let display = CGRect(origin: .zero, size: size).applying(transform)
        let descriptions = try await video.load(.formatDescriptions)
        guard let description = descriptions.first, duration.isFinite, duration > 0, duration <= 21_600,
              display.width.isFinite, display.height.isFinite, display.width > 0, display.height > 0,
              display.width <= 8192, display.height <= 8192, display.width * display.height <= 32_000_000 else {
            throw MediaTools.Failure(text: "Unsupported video duration or dimensions.")
        }
        let nominalRate = try await video.load(.nominalFrameRate)
        let videoRange = try await video.load(.timeRange)
        var probe = Probe(duration: duration, width: display.width, height: display.height, audioCount: audioCount, codec: CMFormatDescriptionGetMediaSubType(description), videoStart: videoRange.start.seconds)
        probe.frameRate = nominalRate.isFinite ? Double(nominalRate) : nil
        guard probe.videoStart.isFinite else { throw MediaTools.Failure(text: "Invalid video timestamps.") }
        if let audio {
            let range = try await audio.load(.timeRange)
            let formats = try await audio.load(.formatDescriptions)
            guard let format = formats.first, range.start.seconds.isFinite, range.duration.seconds.isFinite, range.duration.seconds > 0 else {
                throw MediaTools.Failure(text: "Invalid audio timestamps or format.")
            }
            probe.audioCodec = CMFormatDescriptionGetMediaSubType(format)
            probe.audioStart = range.start.seconds
            probe.audioDuration = range.duration.seconds
        }
        return probe
    }

    static func request(source: URL, output: URL? = nil, recipe: ToolRecipe) async throws -> Probe {
        guard recipe.kind == "video", recipe.format == "mp4", [1280,1920,3840].contains(recipe.maxDimension),
              ["source", "9:16", "16:9", "1:1", "4:5"].contains(recipe.videoAspectRatio ?? "source"),
              ["fit", "auto"].contains(recipe.videoFraming ?? "fit"),
              recipe.videoFraming != "auto" || (recipe.videoAspectRatio ?? "source") != "source" else {
            throw MediaTools.Failure(text: "Unsupported video recipe.")
        }
        if let output { try ReadOnlyLocations.requireWritable(output) }
        let process = Process()
        var executable = CommandLine.arguments[0]
        #if DEBUG
        executable = ProcessInfo.processInfo.environment["FILEID_TEST_ENGINE_PATH"] ?? executable
        #endif
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = ["--convert-video", output == nil ? "probe" : "export", source.path, String(recipe.maxDimension), output?.path ?? "-", recipe.videoAspectRatio ?? "source", recipe.videoFraming ?? "fit", recipe.videoStabilization == true ? "1" : "0", recipe.allowUpscale == true ? "1" : "0"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        let status = try await CancellableProcess.wait(process, timeoutSeconds: output == nil && recipe.videoFraming != "auto" ? 20 : 3600)
        guard let data = try pipe.fileHandleForReading.read(upToCount: 4096), let report = try? JSONDecoder().decode(Report.self, from: data) else {
            throw MediaTools.Failure(text: "The native video worker did not return a valid result.")
        }
        guard status == 0, let probe = report.probe else { throw MediaTools.Failure(text: report.message ?? "Native video export failed.") }
        return probe
    }
}
