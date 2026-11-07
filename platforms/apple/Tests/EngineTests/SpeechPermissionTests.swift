import Foundation
import Testing
@testable import FileIDEngine

@Suite("Speech permission")
struct SpeechPermissionTests {
    @Test("a process without a speech usage description never asks, so macOS can't kill it")
    func bareProcessDoesNotAsk() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDSpeech-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        for (name, plist) in [("Bare.bundle", [String: Any]()), ("App.bundle", ["NSSpeechRecognitionUsageDescription": "Transcribes on-device."])] {
            let contents = root.appendingPathComponent(name).appendingPathComponent("Contents")
            try FileManager.default.createDirectory(at: contents, withIntermediateDirectories: true)
            var info = plist
            info["CFBundleIdentifier"] = "test.\(name)"
            try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0).write(to: contents.appendingPathComponent("Info.plist"))
        }
        #expect(!SpeechPermission.canAsk(bundle: try #require(Bundle(url: root.appendingPathComponent("Bare.bundle")))))
        #expect(SpeechPermission.canAsk(bundle: try #require(Bundle(url: root.appendingPathComponent("App.bundle")))))
    }
}

@Suite("Video frame pool timing")
struct VideoFramePoolTimingTests {
    @Test("a frame just before a step counts as that step, so the next one is not a near-duplicate")
    func nextStepAfterNearBoundaryFrame() {
        let frame = 959.0 / (60_000.0 / 1001.0)
        #expect(abs(frame - 15.9993) < 0.001)
        #expect(VideoFramePool.nextSampleTime(after: frame, interval: 0.5) == 16.5)
        #expect(VideoFramePool.nextSampleTime(after: 16.2, interval: 0.5) == 16.5)
        #expect(VideoFramePool.nextSampleTime(after: 0, interval: 0.5) == 0.5)
    }
}
