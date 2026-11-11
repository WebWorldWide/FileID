import AVFoundation
import Foundation
import Speech

/// On-device Apple Speech for timeline transcripts; the Swift engine's
/// `TimelineSpeechTranscription` and `SpeechPermission`. Passage grouping and
/// persistence stay in the Rust engine.
enum MediaSpeech {
    struct Segment: Sendable {
        let text: String
        let timestamp: Double
        let duration: Double
        let confidence: Double
    }

    enum Status: Int32 {
        case unavailable = 0
        case ready = 1
        /// No `NSSpeechRecognitionUsageDescription` in the host bundle: macOS
        /// kills a process that asks, so FileID never asks (a bare engine binary).
        case cannotAsk = -1
    }

    static func canAsk(bundle: Bundle = .main) -> Bool {
        (bundle.object(forInfoDictionaryKey: "NSSpeechRecognitionUsageDescription") as? String)?.isEmpty == false
    }

    /// Asks once when the user has not decided and `ask` is set; the system shows
    /// the permission prompt on behalf of FileID.app.
    static func authorized(ask: Bool) async -> Status {
        let status = SFSpeechRecognizer.authorizationStatus()
        if status == .authorized { return .ready }
        guard status == .notDetermined else { return .unavailable }
        guard canAsk() else { return .cannotAsk }
        guard ask else { return .unavailable }
        let granted = await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
        }
        return granted ? .ready : .unavailable
    }

    static func recognizer() -> SFSpeechRecognizer? {
        guard let recognizer = SFSpeechRecognizer() else { return nil }
        // The default delegate queue is the main queue, which the Rust host never runs.
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        recognizer.queue = queue
        return recognizer
    }

    static func status(ask: Bool) async -> Status {
        let authorization = await authorized(ask: ask)
        guard authorization == .ready else { return authorization }
        guard let recognizer = recognizer(), recognizer.isAvailable, recognizer.supportsOnDeviceRecognition else { return .unavailable }
        return .ready
    }

    static var modelVersion: String {
        let version = ProcessInfo.processInfo.operatingSystemVersion
        let locale = SFSpeechRecognizer()?.locale.identifier ?? Locale.current.identifier
        return "apple-speech-ondevice-v1/\(locale)/macOS-\(version.majorVersion).\(version.minorVersion).\(version.patchVersion)"
    }

    /// Segments of `[start, end)` of the first audio track, timestamps relative to
    /// `start`. Throws when the chunk could not be read or recognised.
    static func transcribe(source: URL, start: Double, end: Double, cancel: CancelFlag) async throws -> [Segment] {
        guard start.isFinite, end.isFinite, start >= 0, end > start, end - start <= 600 else { throw MediaFailure("Invalid speech interval.") }
        guard await authorized(ask: false) == .ready, let recognizer = recognizer(), recognizer.isAvailable,
              recognizer.supportsOnDeviceRecognition else { throw MediaFailure("On-device speech recognition is unavailable.") }
        let audio = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-speech-\(UUID().uuidString)").appendingPathExtension("m4a")
        defer { try? FileManager.default.removeItem(at: audio) }
        guard let exporter = AVAssetExportSession(asset: AVURLAsset(url: source), presetName: AVAssetExportPresetAppleM4A) else {
            throw MediaFailure("The audio could not be prepared for speech recognition.")
        }
        exporter.timeRange = CMTimeRange(start: CMTime(seconds: start, preferredTimescale: 600),
                                         duration: CMTime(seconds: end - start, preferredTimescale: 600))
        try await exporter.export(to: audio, as: .m4a)
        guard !cancel.isSet else { throw MediaFailure("Speech recognition cancelled.") }
        return try await recognize(audio: audio, recognizer: recognizer, cancel: cancel)
    }

    private final class Recognition: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<[Segment], Error>?
        private var task: SFSpeechRecognitionTask?
        private var finished = false

        func install(_ continuation: CheckedContinuation<[Segment], Error>) {
            lock.lock(); defer { lock.unlock() }
            self.continuation = continuation
        }

        func install(_ task: SFSpeechRecognitionTask) {
            lock.lock()
            if finished { lock.unlock(); task.cancel(); return }
            self.task = task
            lock.unlock()
        }

        func finish(_ result: Result<[Segment], Error>) {
            lock.lock()
            guard !finished else { lock.unlock(); return }
            finished = true
            let continuation = continuation
            let task = task
            self.continuation = nil
            self.task = nil
            lock.unlock()
            task?.cancel()
            continuation?.resume(with: result)
        }
    }

    private static func recognize(audio: URL, recognizer: SFSpeechRecognizer, cancel: CancelFlag) async throws -> [Segment] {
        let state = Recognition()
        let box = RecognizerBox(value: recognizer)
        let watchdog = Task.detached {
            let deadline = Date().addingTimeInterval(90)
            while !Task.isCancelled {
                if cancel.isSet { state.finish(.failure(MediaFailure("Speech recognition cancelled."))); return }
                if Date() > deadline { state.finish(.failure(MediaFailure("Speech recognition timed out."))); return }
                try? await Task.sleep(for: .milliseconds(200))
            }
        }
        defer { watchdog.cancel() }
        return try await withCheckedThrowingContinuation { continuation in
            state.install(continuation)
            let request = SFSpeechURLRecognitionRequest(url: audio)
            request.requiresOnDeviceRecognition = true
            request.shouldReportPartialResults = false
            let task = box.value.recognitionTask(with: request) { result, error in
                if let result, result.isFinal {
                    state.finish(.success(result.bestTranscription.segments.map {
                        Segment(text: $0.substring, timestamp: $0.timestamp, duration: $0.duration, confidence: Double($0.confidence))
                    }))
                } else if let error {
                    state.finish(.failure(error))
                }
            }
            state.install(task)
        }
    }

    private struct RecognizerBox: @unchecked Sendable {
        let value: SFSpeechRecognizer
    }
}
