import AVFoundation
import Foundation
import Speech
import FileIDShared

enum TimelineSpeechTranscription {
    struct Segment: Sendable, Equatable {
        let text: String
        let timestamp: Double
        let duration: Double
        let confidence: Double
    }

    struct Passage: Sendable, Equatable {
        let startSeconds: Double
        let endSeconds: Double
        let text: String
        let confidence: Double
    }

    struct Chunk: Sendable, Equatable {
        let extractStart: Double
        let extractEnd: Double
        let ownedStart: Double
        let ownedEnd: Double
    }

    static var modelVersion: String {
        let version = ProcessInfo.processInfo.operatingSystemVersion
        let locale = SFSpeechRecognizer()?.locale.identifier ?? Locale.current.identifier
        return "apple-speech-ondevice-v1/\(locale)/macOS-\(version.majorVersion).\(version.minorVersion).\(version.patchVersion)"
    }

    static func chunks(duration: Double, maximumCoreDuration: Double = 45, overlap: Double = 1.5) -> [Chunk] {
        guard duration.isFinite, duration > 0,
              maximumCoreDuration.isFinite, maximumCoreDuration > 0,
              overlap.isFinite, overlap >= 0 else { return [] }

        var result: [Chunk] = []
        var coreStart = 0.0
        while coreStart < duration {
            let coreEnd = min(coreStart + maximumCoreDuration, duration)
            result.append(Chunk(
                extractStart: max(0, coreStart - overlap),
                extractEnd: min(duration, coreEnd + overlap),
                ownedStart: coreStart,
                ownedEnd: coreEnd
            ))
            coreStart = coreEnd
        }
        return result
    }

    static func sweepAbandonedAudio(directory: URL = FileManager.default.temporaryDirectory, now: Date = Date()) {
        guard (try? ReadOnlyLocations.requireWritable(directory)) != nil,
              let entries = try? FileManager.default.contentsOfDirectory(
                at: directory,
                includingPropertiesForKeys: [.contentModificationDateKey, .isRegularFileKey]
              ) else { return }

        for entry in entries where entry.lastPathComponent.hasPrefix("fileid-speech-") && entry.pathExtension == "m4a" {
            let identifier = String(entry.deletingPathExtension().lastPathComponent.dropFirst("fileid-speech-".count))
            guard UUID(uuidString: identifier) != nil,
                  (try? ReadOnlyLocations.requireWritable(entry)) != nil,
                  let values = try? entry.resourceValues(forKeys: [.contentModificationDateKey, .isRegularFileKey]),
                  values.isRegularFile == true,
                  let modified = values.contentModificationDate,
                  now.timeIntervalSince(modified) > 300 else { continue }
            try? FileManager.default.removeItem(at: entry)
        }
    }

    static func passages(from segments: [Segment], chunk: Chunk, mediaDuration: Double) -> [Passage] {
        guard mediaDuration.isFinite, mediaDuration > 0 else { return [] }
        let sorted = segments.compactMap { segment -> Segment? in
            guard segment.timestamp.isFinite, segment.duration.isFinite,
                  segment.confidence.isFinite else { return nil }
            let text = segment.text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { return nil }
            let start = chunk.extractStart + max(0, segment.timestamp)
            let end = min(mediaDuration, chunk.extractStart + max(segment.timestamp, segment.timestamp + segment.duration))
            let midpoint = start + (end - start) / 2
            guard midpoint >= chunk.ownedStart, midpoint < chunk.ownedEnd,
                  start < mediaDuration, end >= start else { return nil }
            return Segment(
                text: text,
                timestamp: start,
                duration: max(0, end - start),
                confidence: min(1, max(0, segment.confidence))
            )
        }.sorted { $0.timestamp < $1.timestamp }

        var passages: [Passage] = []
        var group: [Segment] = []
        func flush() {
            guard let first = group.first, let last = group.last else { return }
            let text = group.reduce(into: "") { output, segment in
                if output.isEmpty { output = segment.text }
                else if segment.text.first?.isPunctuation == true { output += segment.text }
                else { output += " " + segment.text }
            }
            let end = min(mediaDuration, last.timestamp + max(last.duration, 0.001))
            passages.append(Passage(
                startSeconds: first.timestamp,
                endSeconds: max(first.timestamp + 0.001, end),
                text: text,
                confidence: group.reduce(0) { $0 + $1.confidence } / Double(group.count)
            ))
            group.removeAll(keepingCapacity: true)
        }

        for segment in sorted {
            if let previous = group.last {
                let gap = segment.timestamp - (previous.timestamp + previous.duration)
                let span = segment.timestamp - group[0].timestamp
                let length = group.reduce(segment.text.count) { $0 + $1.text.count }
                if gap > 1.25 || span > 8 || length > 280 { flush() }
            }
            group.append(segment)
        }
        flush()
        return passages
    }

    static func hasAudioTrack(url: URL) async -> Bool {
        let asset = AVURLAsset(url: url)
        return (try? await asset.loadTracks(withMediaType: .audio).isEmpty) == false
    }

    static func isAvailableOnDevice() async -> Bool {
        guard await requestAuthorization(), let recognizer = SFSpeechRecognizer() else { return false }
        return recognizer.isAvailable && recognizer.supportsOnDeviceRecognition
    }

    static func transcribe(videoURL: URL, chunk: Chunk, mediaDuration: Double) async -> [Passage]? {
        guard await requestAuthorization(), !Task.isCancelled else { return nil }
        guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable,
              recognizer.supportsOnDeviceRecognition else { return nil }

        let audioURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-speech-\(UUID().uuidString)")
            .appendingPathExtension("m4a")
        guard (try? ReadOnlyLocations.requireWritable(audioURL)) != nil else { return nil }
        guard !FileManager.default.fileExists(atPath: audioURL.path) else { return nil }
        defer { try? FileManager.default.removeItem(at: audioURL) }

        let asset = AVURLAsset(url: videoURL)
        guard let exporter = AVAssetExportSession(asset: asset, presetName: AVAssetExportPresetAppleM4A) else { return nil }
        exporter.timeRange = CMTimeRange(
            start: CMTime(seconds: chunk.extractStart, preferredTimescale: 600),
            duration: CMTime(seconds: chunk.extractEnd - chunk.extractStart, preferredTimescale: 600)
        )
        do {
            try await exporter.export(to: audioURL, as: .m4a)
        } catch {
            return nil
        }
        guard !Task.isCancelled else { return nil }
        let segments = await recognize(audioURL: audioURL, recognizer: recognizer)
        guard let segments else { return nil }
        return passages(from: segments, chunk: chunk, mediaDuration: mediaDuration)
    }

    private static func requestAuthorization() async -> Bool {
        if SFSpeechRecognizer.authorizationStatus() == .authorized { return true }
        return await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
        }
    }

    private static func recognize(audioURL: URL, recognizer: SFSpeechRecognizer) async -> [Segment]? {
        let recognizerBox = SpeechRecognizerBox(recognizer)
        return await withTaskGroup(of: [Segment]?.self) { group in
            group.addTask {
                let state = SpeechSegmentsRecognitionState()
                let recognizer = recognizerBox.value
                let request = SFSpeechURLRecognitionRequest(url: audioURL)
                request.requiresOnDeviceRecognition = true
                request.shouldReportPartialResults = false
                return await withTaskCancellationHandler {
                    await withCheckedContinuation { continuation in
                        state.install(continuation)
                        let task = recognizer.recognitionTask(with: request) { result, error in
                            if let result, result.isFinal {
                                let segments = result.bestTranscription.segments.map {
                                    Segment(text: $0.substring, timestamp: $0.timestamp, duration: $0.duration, confidence: Double($0.confidence))
                                }
                                state.finish(segments)
                            } else if error != nil {
                                state.finish(nil)
                            }
                        }
                        state.install(task)
                    }
                } onCancel: {
                    state.cancel()
                }
            }
            group.addTask {
                try? await Task.sleep(for: .seconds(90))
                return nil
            }
            let result = await group.next() ?? nil
            group.cancelAll()
            return result ?? nil
        }
    }
}

private final class SpeechRecognizerBox: @unchecked Sendable {
    let value: SFSpeechRecognizer
    init(_ value: SFSpeechRecognizer) { self.value = value }
}

private final class SpeechSegmentsRecognitionState: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<[TimelineSpeechTranscription.Segment]?, Never>?
    private var task: SFSpeechRecognitionTask?
    private var finished = false

    func install(_ continuation: CheckedContinuation<[TimelineSpeechTranscription.Segment]?, Never>) {
        lock.lock()
        guard !finished else {
            lock.unlock()
            continuation.resume(returning: nil)
            return
        }
        self.continuation = continuation
        lock.unlock()
    }

    func install(_ task: SFSpeechRecognitionTask) {
        lock.lock()
        guard !finished else {
            lock.unlock()
            task.cancel()
            return
        }
        self.task = task
        lock.unlock()
    }

    func finish(_ result: [TimelineSpeechTranscription.Segment]?) {
        lock.lock()
        guard !finished else {
            lock.unlock()
            return
        }
        finished = true
        let continuation = continuation
        self.continuation = nil
        let task = task
        self.task = nil
        lock.unlock()
        task?.cancel()
        continuation?.resume(returning: result)
    }

    func cancel() {
        lock.lock()
        guard !finished else {
            lock.unlock()
            return
        }
        finished = true
        let continuation = continuation
        self.continuation = nil
        let task = task
        self.task = nil
        lock.unlock()
        task?.cancel()
        continuation?.resume(returning: nil)
    }
}
