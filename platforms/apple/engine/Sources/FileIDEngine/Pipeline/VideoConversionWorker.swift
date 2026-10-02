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
            guard arguments.count == 4, ["probe", "export"].contains(arguments[0]),
                  let dimension = Int(arguments[2]), [1280,1920].contains(dimension) else {
                throw MediaTools.Failure(text: "Choose a 1280 or 1920 pixel video export.")
            }
            let source = URL(fileURLWithPath: arguments[1])
            let output = URL(fileURLWithPath: arguments[3])
            if arguments[0] == "export" {
                try ReadOnlyLocations.requireSourceMutation(output)
                guard !FileManager.default.fileExists(atPath: output.path) else { throw MediaTools.Failure(text: "The staged output is occupied.") }
            }
            guard ["mp4","mov","m4v"].contains(source.pathExtension.lowercased()) else { throw MediaTools.Failure(text: "Video inputs must be MP4, MOV or M4V.") }
            let asset = AVURLAsset(url: source, options: [AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue])
            let before = try await inspect(asset)
            let preset = dimension == 1280 ? AVAssetExportPreset1280x720 : AVAssetExportPreset1920x1080
            guard let exporter = AVAssetExportSession(asset: asset, presetName: preset), exporter.supportedFileTypes.contains(.mp4) else {
                throw MediaTools.Failure(text: "This video cannot use the selected native MP4 preset.")
            }
            if arguments[0] == "export" {
                exporter.metadata = []
                exporter.shouldOptimizeForNetworkUse = true
                try await exporter.export(to: output, as: .mp4)
                let after = try await inspect(AVURLAsset(url: output, options: [AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue, AVURLAssetOverrideMIMETypeKey: "video/mp4"]))
                guard abs(after.duration - before.duration) <= 0.25, after.audioCount == before.audioCount,
                      after.codec == kCMVideoCodecType_H264, max(after.width, after.height) <= Double(dimension) + 2,
                      abs(after.width / after.height - before.width / before.height) <= 0.02 else {
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
        let videoRange = try await video.load(.timeRange)
        var probe = Probe(duration: duration, width: display.width, height: display.height, audioCount: audioCount, codec: CMFormatDescriptionGetMediaSubType(description), videoStart: videoRange.start.seconds)
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
        guard recipe.kind == "video", recipe.format == "mp4", [1280,1920].contains(recipe.maxDimension) else {
            throw MediaTools.Failure(text: "Unsupported video recipe.")
        }
        if let output { try ReadOnlyLocations.requireWritable(output) }
        let process = Process()
        var executable = CommandLine.arguments[0]
        #if DEBUG
        executable = ProcessInfo.processInfo.environment["FILEID_TEST_ENGINE_PATH"] ?? executable
        #endif
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = ["--convert-video", output == nil ? "probe" : "export", source.path, String(recipe.maxDimension), output?.path ?? "-"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        let status = try await CancellableProcess.wait(process, timeoutSeconds: output == nil ? 20 : 3600)
        guard let data = try pipe.fileHandleForReading.read(upToCount: 4096), let report = try? JSONDecoder().decode(Report.self, from: data) else {
            throw MediaTools.Failure(text: "The native video worker did not return a valid result.")
        }
        guard status == 0, let probe = report.probe else { throw MediaTools.Failure(text: report.message ?? "Native video export failed.") }
        return probe
    }
}
