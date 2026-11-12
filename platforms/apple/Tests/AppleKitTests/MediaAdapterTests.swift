import AVFoundation
import Foundation
import Testing
@testable import FileIDAppleKit

@Suite("FileIDAppleKit media adapters (Stage 8)", .serialized)
struct MediaAdapterTests {
    final class Scratch {
        let url: URL
        init() throws {
            url = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDAppleKit-media-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        }
        deinit { try? FileManager.default.removeItem(at: url) }
        func path(_ name: String) -> URL { url.appendingPathComponent(name) }
    }

    private func object(_ reply: UnsafeMutablePointer<CChar>?) throws -> [String: Any] {
        let reply = try #require(reply)
        defer { fileid_apple_free(reply) }
        return try #require(try JSONSerialization.jsonObject(with: Data(String(cString: reply).utf8)) as? [String: Any])
    }

    private func probe(_ url: URL, mime: String? = nil, strict: Bool = true) throws -> [String: Any] {
        try object(url.path.withCString { path in
            if let mime { return mime.withCString { fileid_apple_media_probe(path, $0, strict) } }
            return fileid_apple_media_probe(path, nil, strict)
        })
    }

    private func export(_ recipe: [String: Any], source: URL, output: URL?) throws -> [String: Any] {
        var request: [String: Any] = ["recipe": recipe, "source": source.path]
        request["output"] = output?.path
        let text = String(decoding: try JSONSerialization.data(withJSONObject: request), as: UTF8.self)
        return try object(text.withCString { fileid_apple_media_export($0, nil) })
    }

    @Test func probeReadsCodecsTimingAndRotation() async throws {
        let scratch = try Scratch()
        let clip = scratch.path("Portrait.mov")
        try await MediaTestClip.write(to: clip, rotation: 90, fileType: .mov)
        let facts = try probe(clip)
        #expect(facts["error"] == nil)
        #expect(facts["videoCodec"] as? String == "h264")
        #expect(facts["audioCodec"] as? String == "aac")
        #expect(facts["rotation"] as? Int == 90)
        #expect((facts["width"] as? Double, facts["height"] as? Double) == (54, 96))
        #expect(abs((facts["duration"] as? Double ?? 0) - 2) < 0.1)
        #expect(facts["otherCount"] as? Int == 0)
        let text = scratch.path("Notes.txt")
        try Data("not media".utf8).write(to: text)
        #expect(try probe(text)["error"] as? String == "The input is unreadable or protected.")
    }

    @Test func commonRecipesWriteVerifiableOutputs() async throws {
        let scratch = try Scratch()
        let clip = scratch.path("Clip.mp4")
        try await MediaTestClip.write(to: clip)
        let original = try Data(contentsOf: clip)
        let cases: [([String: Any], String, String)] = [
            (["kind": "remux", "format": "mov", "maxDimension": 1920], "video/quicktime", "h264"),
            (["kind": "trim", "format": "mp4", "maxDimension": 1920, "trimStartSeconds": 0.5, "trimEndSeconds": 1.5, "trimMode": "keyframe"], "video/mp4", "h264"),
            (["kind": "trim", "format": "mp4", "maxDimension": 1920, "trimStartSeconds": 0.5, "trimEndSeconds": 1.5, "trimMode": "exact"], "video/mp4", "h264"),
            (["kind": "audio", "format": "m4a", "maxDimension": 1920], "audio/mp4", ""),
            (["kind": "proxy", "format": "mp4", "maxDimension": 640], "video/mp4", "h264"),
            (["kind": "rotate", "format": "mp4", "maxDimension": 1920, "rotationDegrees": 90], "video/mp4", "h264"),
        ]
        for (index, (recipe, mime, codec)) in cases.enumerated() {
            let stage = scratch.path(".FileIDExport-\(index).part")
            let reply = try export(recipe, source: clip, output: stage)
            #expect(reply["error"] == nil, "\(recipe): \(reply)")
            let after = try probe(stage, mime: mime)
            #expect(after["error"] == nil, "\(recipe): \(after)")
            if codec.isEmpty {
                #expect(after["videoCount"] as? Int == 0)
                #expect(after["audioCodec"] as? String == "aac")
            } else {
                #expect(after["videoCodec"] as? String == codec)
            }
            if recipe["kind"] as? String == "rotate" {
                #expect(after["rotation"] as? Int == 90)
            }
            if (recipe["kind"] as? String) == "trim" {
                #expect(abs((after["duration"] as? Double ?? 0) - 1) < 0.3, "\(recipe): \(after)")
            }
        }
        #expect(try Data(contentsOf: clip) == original)
        let occupied = try export(cases[0].0, source: clip, output: scratch.path(".FileIDExport-0.part"))
        #expect(occupied["error"] as? String == "The staged output is occupied.")
    }

    @Test func cancelledExportStops() async throws {
        let scratch = try Scratch()
        let clip = scratch.path("Clip.mp4")
        try await MediaTestClip.write(to: clip)
        let request: [String: Any] = ["recipe": ["kind": "proxy", "format": "mp4", "maxDimension": 640], "source": clip.path,
                                      "output": scratch.path(".FileIDExport.part").path]
        let text = String(decoding: try JSONSerialization.data(withJSONObject: request), as: UTF8.self)
        var cancelled = true
        let reply = try object(text.withCString { fileid_apple_media_export($0, &cancelled) })
        #expect(reply["error"] as? String == "Export cancelled before publication")
    }

    @Test func videoExportFitsPadsAndStabilizes() async throws {
        let scratch = try Scratch()
        let clip = scratch.path("Clip.mov")
        try await MediaTestClip.write(to: clip, width: 160, height: 90, fileType: .mov)
        let plan = try export(["kind": "video", "format": "mp4", "maxDimension": 1280, "videoAspectRatio": "9:16", "videoFraming": "auto"], source: clip, output: nil)
        #expect((plan["summary"] as? String)?.hasPrefix("People framing:") == true, "\(plan)")
        let fit = try export(["kind": "video", "format": "mp4", "maxDimension": 1280, "videoAspectRatio": "9:16"], source: clip, output: scratch.path(".fit.part"))
        let fitted = try #require(fit["probe"] as? [String: Any], "\(fit)")
        #expect(fitted["videoCodec"] as? String == "h264")
        #expect(abs((fitted["width"] as? Double ?? 0) / (fitted["height"] as? Double ?? 1) - 9.0 / 16.0) < 0.02)
        let steady = try export(["kind": "video", "format": "mp4", "maxDimension": 1280, "videoStabilization": true], source: clip, output: scratch.path(".steady.part"))
        #expect((steady["summary"] as? String)?.hasPrefix("Stabilization:") == true, "\(steady)")
        let unsupported = try export(["kind": "video", "format": "mp4", "maxDimension": 1000], source: clip, output: nil)
        #expect(unsupported["error"] as? String == "Choose a 1280, 1920 or 3840 pixel video export.")
    }

    @Test func framesAndPoolDecodeUprightPictures() async throws {
        let scratch = try Scratch()
        let clip = scratch.path("Clip.mp4")
        try await MediaTestClip.write(to: clip, width: 96, height: 54, seconds: 3)
        var seconds = 0.0
        let frame = clip.path.withCString { fileid_apple_video_frame($0, 1.0, 64, 36, &seconds) }
        let pixels = try #require(frame)
        defer { fileid_apple_free(pixels) }
        #expect(abs(seconds - 1.0) < 0.05)
        let top = Int(pixels[(2 * 64 + 2) * 3 + 1])
        let bottom = Int(pixels[(33 * 64 + 2) * 3 + 1])
        #expect(top > bottom + 40, "the green band must stay at the top: \(top) vs \(bottom)")
        #expect(clip.path.withCString { fileid_apple_video_frame($0, -1, 64, 36, &seconds) } == nil)

        let pool = scratch.path("pool")
        try FileManager.default.createDirectory(at: pool, withIntermediateDirectories: true)
        let status = clip.path.withCString { source in pool.path.withCString { fileid_apple_video_pool(source, 0.5, 448, $0, nil) } }
        #expect(status == 0)
        let manifest = try #require(try JSONSerialization.jsonObject(with: Data(contentsOf: pool.appendingPathComponent("manifest.json"))) as? [String: Any])
        let frames = try #require(manifest["frames"] as? [[String: Any]])
        #expect(frames.count == 6)
        #expect((frames.first?["signature"] as? [Int])?.count == 576)
        #expect(frames.compactMap { $0["seconds"] as? Double }.enumerated().allSatisfy { abs($0.element - Double($0.offset) * 0.5) < 0.05 })
        #expect(clip.path.withCString { source in pool.path.withCString { fileid_apple_video_pool(source, 0.01, 448, $0, nil) } } == 2)
    }

    @Test func speechReportsStatusWithoutAsking() throws {
        let status = fileid_apple_speech_status(false)
        #expect([-1, 0, 1].contains(status))
        let version = try #require(fileid_apple_speech_model_version())
        defer { fileid_apple_free(version) }
        #expect(String(cString: version).hasPrefix("apple-speech-ondevice-v1/"))
    }

    /// Transcription refuses bad intervals before touching the file and never
    /// asks for permission; unauthorised hosts (a bare test binary) get an
    /// error reply rather than a prompt or a crash.
    @Test func speechTranscribeRefusesWithoutAsking() throws {
        func reply(_ start: Double, _ end: Double) throws -> [String: Any] {
            let text = try #require("/nonexistent/clip.mov".withCString { fileid_apple_speech_transcribe($0, start, end, nil) })
            defer { fileid_apple_free(text) }
            return try #require(try JSONSerialization.jsonObject(with: Data(String(cString: text).utf8)) as? [String: Any])
        }
        for (start, end) in [(5.0, 5.0), (-1.0, 2.0), (0.0, 601.0), (0.0, Double.nan)] {
            #expect(try reply(start, end)["error"] as? String == "Invalid speech interval.")
        }
        #expect(try reply(0, 2)["error"] is String, "a missing file is refused whether or not speech is authorised")
        let invalid = try #require(fileid_apple_speech_transcribe(nil, 0, 2, nil))
        defer { fileid_apple_free(invalid) }
        #expect(String(cString: invalid).contains("Invalid media path"))
    }

    /// Writes the shared synthetic clip the Rust tests and conformance use
    /// (`shared/test-corpus/conformance-media/`): FILEID_WRITE_MEDIA_FIXTURE=<path.mov>.
    @Test func writesSharedFixtureOnRequest() async throws {
        guard let path = ProcessInfo.processInfo.environment["FILEID_WRITE_MEDIA_FIXTURE"] else { return }
        try? FileManager.default.removeItem(atPath: path)
        try await MediaTestClip.write(to: URL(fileURLWithPath: path), width: 96, height: 54, seconds: 3, fps: 12, rotation: 90, fileType: .mov)
    }
}
