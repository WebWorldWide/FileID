import AVFoundation
import Foundation
import ImageIO
import Testing
import FileIDShared
@testable import FileID

@MainActor
@Suite("Engine query replies", .serialized)
struct EngineQueryRepliesTests {
    @MainActor
    private final class Sent {
        var count = 0
        var queryIDs: [String] = []
    }

    private func pair(_ source: Int64, _ destination: Int64) -> MergeSuggestionPair {
        MergeSuggestionPair(sourcePersonID: source, destinationPersonID: destination, similarity: 0.8,
                            sourceAnchorFaceID: source * 10, destinationAnchorFaceID: destination * 10,
                            sourceMemberCount: 1, destinationMemberCount: 4)
    }

    private func vector(_ value: Float) -> [Float] {
        [Float](repeating: value, count: CLIPEmbeddingSpace.dimension)
    }

    private func jpegData(width: Int, height: Int) throws -> Data {
        try makeJPEG(width: width, height: height)
    }

    private func waitFor(_ replies: EngineQueryReplies, count: Int) async {
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while replies.waiting < count, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(2))
        }
    }

    @Test func mergeSuggestionsReachEveryWaitingCaller() async {
        let replies = EngineQueryReplies()
        let sent = Sent()
        let first = Task { await replies.mergeSuggestions { sent.count += 1; return true } }
        let second = Task { await replies.mergeSuggestions { sent.count += 1; return true } }
        await waitFor(replies, count: 2)
        #expect(replies.waiting == 2)
        replies.deliver(MergeSuggestions(pairs: [pair(2, 1)]))
        #expect(await first.value?.map(\.id) == ["2:1"])
        #expect(await second.value?.map(\.id) == ["2:1"])
        #expect(sent.count == 2)
        #expect(replies.waiting == 0)
    }

    @Test func mergeSuggestionFailureEndsTheWaitButOtherErrorsDoNot() async {
        let replies = EngineQueryReplies()
        let pending = Task { await replies.mergeSuggestions { true } }
        await waitFor(replies, count: 1)
        replies.deliver(EngineError(kind: "db_unavailable", message: "x"))
        replies.deliver(EngineError(kind: "embed_failed", message: "x"))
        #expect(replies.waiting == 1)
        replies.deliver(EngineError(kind: "find_merge_suggestions_failed", message: "x"))
        #expect(await pending.value == nil)
        #expect(replies.waiting == 0)
    }

    @Test func textEmbeddingsAreMatchedByQueryID() async {
        let replies = EngineQueryReplies()
        let sent = Sent()
        let first = Task { await replies.textEmbedding { sent.queryIDs.append($0); return true } }
        await waitFor(replies, count: 1)
        let second = Task { await replies.textEmbedding { sent.queryIDs.append($0); return true } }
        await waitFor(replies, count: 2)
        #expect(sent.queryIDs.count == 2)
        #expect(Set(sent.queryIDs).count == 2)
        replies.deliver(ClipTextEmbedding(queryID: "someone-else", query: "x", embedding: vector(0.9)))
        #expect(replies.waiting == 2)
        replies.deliver(ClipTextEmbedding(queryID: sent.queryIDs[1], query: "cat", embedding: vector(0.2)))
        replies.deliver(ClipTextEmbedding(queryID: sent.queryIDs[0], query: "dog", embedding: vector(0.1)))
        #expect(await first.value?.first == 0.1)
        #expect(await second.value?.first == 0.2)
        #expect(replies.waiting == 0)
    }

    @Test func anEngineWithoutTheTextModelLeavesSearchOnKeywords() async {
        let replies = EngineQueryReplies()
        for embedding in [[Float](), [Float](repeating: 1, count: 8)] {
            let sent = Sent()
            let pending = Task { await replies.textEmbedding { sent.queryIDs.append($0); return true } }
            await waitFor(replies, count: 1)
            replies.deliver(ClipTextEmbedding(queryID: sent.queryIDs.first ?? "", query: "dog", embedding: embedding))
            #expect(await pending.value == nil)
            #expect(replies.waiting == 0)
        }
    }

    @Test func videoThumbnailsAreMatchedByPathAndRevision() async throws {
        let replies = EngineQueryReplies()
        let jpeg = try jpegData(width: 512, height: 288)
        let wanted = Task { await replies.videoThumbnail(path: "/v/a.mov", modifiedAt: 5, maxPixelSize: 512) { true } }
        let stale = Task { await replies.videoThumbnail(path: "/v/b.mov", modifiedAt: 7, maxPixelSize: 512) { true } }
        let garbled = Task { await replies.videoThumbnail(path: "/v/c.mov", modifiedAt: nil, maxPixelSize: 512) { true } }
        await waitFor(replies, count: 3)
        replies.deliver(ThumbnailGenerated(path: "/v/other.mov", modifiedAt: 5, bytes: jpeg.base64EncodedString()))
        #expect(replies.waiting == 3)
        replies.deliver(ThumbnailGenerated(path: "/v/a.mov", modifiedAt: 5, bytes: jpeg.base64EncodedString()))
        replies.deliver(ThumbnailGenerated(path: "/v/b.mov", modifiedAt: 6, bytes: jpeg.base64EncodedString()))
        replies.deliver(ThumbnailGenerated(path: "/v/c.mov", modifiedAt: nil, bytes: "%%%"))
        #expect(await wanted.value == jpeg)
        #expect(await stale.value == nil)
        #expect(await garbled.value == nil)
        #expect(replies.waiting == 0)
    }

    @Test func eachThumbnailSizeGetsItsOwnFrame() async throws {
        let replies = EngineQueryReplies()
        let grid = Task { await replies.videoThumbnail(path: "/v/a.mov", modifiedAt: 1, maxPixelSize: 512) { true } }
        let preview = Task { await replies.videoThumbnail(path: "/v/a.mov", modifiedAt: 1, maxPixelSize: 2048) { true } }
        await waitFor(replies, count: 2)
        let small = try jpegData(width: 512, height: 288)
        replies.deliver(ThumbnailGenerated(path: "/v/a.mov", modifiedAt: 1, bytes: small.base64EncodedString()))
        #expect(await grid.value == small)
        #expect(replies.waiting == 1)
        let whole = try jpegData(width: 1280, height: 720)
        replies.deliver(ThumbnailGenerated(path: "/v/a.mov", modifiedAt: 1, bytes: whole.base64EncodedString()))
        #expect(await preview.value == whole)
        #expect(replies.waiting == 0)
    }

    @Test func aSourceSmallerThanEveryRequestAnswersThemAll() async throws {
        let replies = EngineQueryReplies()
        let grid = Task { await replies.videoThumbnail(path: "/v/a.mov", modifiedAt: nil, maxPixelSize: 512) { true } }
        let preview = Task { await replies.videoThumbnail(path: "/v/a.mov", modifiedAt: nil, maxPixelSize: 1024) { true } }
        await waitFor(replies, count: 2)
        let tiny = try jpegData(width: 320, height: 180)
        replies.deliver(ThumbnailGenerated(path: "/v/a.mov", modifiedAt: nil, bytes: tiny.base64EncodedString()))
        #expect(await grid.value == tiny)
        #expect(await preview.value == tiny)
        #expect(replies.waiting == 0)
    }

    @Test func aCommandThatNeverLeftTheAppReturnsAtOnce() async {
        let replies = EngineQueryReplies()
        #expect(await replies.mergeSuggestions { false } == nil)
        #expect(await replies.textEmbedding { _ in false } == nil)
        #expect(await replies.videoThumbnail(path: "/v/a.mov", modifiedAt: nil, maxPixelSize: 512) { false } == nil)
        #expect(replies.waiting == 0)
    }

    @Test(.timeLimit(.minutes(1))) func silenceTimesOutWithoutLeavingAWaiter() async {
        let replies = EngineQueryReplies(mergeTimeout: .milliseconds(50), embeddingTimeout: .milliseconds(50),
                                         thumbnailTimeout: .milliseconds(50))
        #expect(await replies.mergeSuggestions { true } == nil)
        #expect(await replies.textEmbedding { _ in true } == nil)
        #expect(await replies.videoThumbnail(path: "/v/a.mov", modifiedAt: nil, maxPixelSize: 512) { true } == nil)
        #expect(replies.waiting == 0)
    }

    @Test(.timeLimit(.minutes(1))) func cancellingTheCallerReleasesItsWaiter() async {
        let replies = EngineQueryReplies()
        let merge = Task { await replies.mergeSuggestions { true } }
        let embed = Task { await replies.textEmbedding { _ in true } }
        let thumb = Task { await replies.videoThumbnail(path: "/v/a.mov", modifiedAt: nil, maxPixelSize: 512) { true } }
        await waitFor(replies, count: 3)
        #expect(replies.waiting == 3)
        merge.cancel(); embed.cancel(); thumb.cancel()
        #expect(await merge.value == nil)
        #expect(await embed.value == nil)
        #expect(await thumb.value == nil)
        #expect(replies.waiting == 0)
    }

    @Test func anAlreadyCancelledCallerNeverWaits() async {
        let replies = EngineQueryReplies()
        let sent = Sent()
        let task = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return await replies.mergeSuggestions { sent.count += 1; return true }
        }
        #expect(await task.value == nil)
        #expect(sent.count == 0)
        #expect(replies.waiting == 0)
    }

    @Test(.timeLimit(.minutes(1))) func anEngineExitReleasesEveryWaiter() async {
        let replies = EngineQueryReplies()
        let merge = Task { await replies.mergeSuggestions { true } }
        let embed = Task { await replies.textEmbedding { _ in true } }
        let thumb = Task { await replies.videoThumbnail(path: "/v/a.mov", modifiedAt: nil, maxPixelSize: 512) { true } }
        await waitFor(replies, count: 3)
        replies.abandonAll()
        #expect(await merge.value == nil)
        #expect(await embed.value == nil)
        #expect(await thumb.value == nil)
        #expect(replies.waiting == 0)
        replies.deliver(MergeSuggestions(pairs: [pair(2, 1)]))
        #expect(replies.waiting == 0)
    }

    @Test(.timeLimit(.minutes(1))) func anEngineClientWithNoEngineAnswersNilAtOnce() async {
        let engine = EngineClient()
        #expect(await engine.findMergeSuggestions() == nil)
        #expect(await engine.embedTextQuery("a dog on a beach") == nil)
        #expect(await engine.generateVideoThumbnail(path: "/v/a.mov", modifiedAt: 1, maxPixelSize: 1024) == nil)
    }
}

@MainActor
@Suite("Video thumbnails come from the engine first")
struct VideoThumbnailSourceTests {
    @MainActor
    private final class FakeSource {
        let answer: Data?
        var requests: [(String, Double?, Int)] = []
        init(answer: Data?) { self.answer = answer }
        var source: VideoThumbnailSource {
            { [self] path, modifiedAt, size in
                requests.append((path, modifiedAt, size))
                return answer
            }
        }
    }

    private final class QuickLookCalls: @unchecked Sendable {
        private let lock = NSLock()
        private var count = 0
        var value: Int { lock.lock(); defer { lock.unlock() }; return count }
        func record() { lock.lock(); count += 1; lock.unlock() }
    }

    private static let engineJPEG = Data([0xFF, 0xD8, 0x01, 0xFF, 0xD9])
    private static let quickLookJPEG = Data([0xFF, 0xD8, 0x02, 0xFF, 0xD9])

    private func render(_ url: URL, source: VideoThumbnailSource?, calls: QuickLookCalls) async -> Data? {
        let quickLook = Self.quickLookJPEG
        return await ThumbnailService.render(url: url, size: 256, scale: 2, modifiedAt: 42, videoSource: source) { _, _, _ in
            calls.record()
            return quickLook
        }
    }

    @Test func theEngineIsAskedForTheBucketsPixelSize() {
        #expect(ThumbnailService.enginePixelSize(size: 256, scale: 2) == 512)
        #expect(ThumbnailService.enginePixelSize(size: 512, scale: 2) == 1024)
        #expect(ThumbnailService.enginePixelSize(size: 1024, scale: 2) == 2048)
        #expect(ThumbnailService.enginePixelSize(size: 1600, scale: 2) == 2048)
        #expect(ThumbnailService.enginePixelSize(size: 256, scale: 1) == 256)
    }

    @Test func onlyVideoExtensionsAskTheEngine() {
        #expect(ThumbnailService.isVideo(URL(fileURLWithPath: "/a/clip.MOV")))
        #expect(ThumbnailService.isVideo(URL(fileURLWithPath: "/a/clip.mp4")))
        #expect(!ThumbnailService.isVideo(URL(fileURLWithPath: "/a/photo.jpg")))
        #expect(!ThumbnailService.isVideo(URL(fileURLWithPath: "/a/notes.pdf")))
    }

    @Test func anEngineFrameSkipsQuickLook() async {
        let source = FakeSource(answer: Self.engineJPEG)
        let calls = QuickLookCalls()
        let data = await render(URL(fileURLWithPath: "/v/clip.mov"), source: source.source, calls: calls)
        #expect(data == Self.engineJPEG)
        #expect(calls.value == 0)
        #expect(source.requests.count == 1)
        #expect(source.requests.first?.0 == "/v/clip.mov")
        #expect(source.requests.first?.1 == 42)
        #expect(source.requests.first?.2 == 512)
    }

    @Test func aSilentOrRefusingEngineFallsBackToQuickLook() async {
        let source = FakeSource(answer: nil)
        let calls = QuickLookCalls()
        #expect(await render(URL(fileURLWithPath: "/v/clip.mov"), source: source.source, calls: calls) == Self.quickLookJPEG)
        #expect(calls.value == 1)
    }

    @Test func noEngineMeansQuickLook() async {
        let calls = QuickLookCalls()
        #expect(await render(URL(fileURLWithPath: "/v/photo.jpg"), source: nil, calls: calls) == Self.quickLookJPEG)
        #expect(calls.value == 1)
    }

    @Test(.timeLimit(.minutes(1))) func anEngineThatIsNotRunningFallsBackToQuickLook() async {
        let calls = QuickLookCalls()
        let engine = EngineClient()
        let data = await render(URL(fileURLWithPath: "/v/clip.mov"), source: { path, modifiedAt, size in
            await engine.generateVideoThumbnail(path: path, modifiedAt: modifiedAt, maxPixelSize: size)
        }, calls: calls)
        #expect(data == Self.quickLookJPEG)
        #expect(calls.value == 1)
    }
}

/// Drives the real engine binary against a scratch catalog and routes its
/// replies through the app's waiters.
@MainActor
@Suite("Engine query replies (real engine)", .serialized)
struct EngineQueryRepliesProcessTests {
    private final class Harness: @unchecked Sendable {
        let process = Process()
        let input = Pipe()
        let root: URL

        @MainActor
        init(binary: URL, replies: EngineQueryReplies) throws {
            root = FileManager.default.temporaryDirectory
                .appendingPathComponent("FileIDQueryReplies-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            var env = ProcessInfo.processInfo.environment
            env["FILEID_DATABASE_PATH"] = root.appendingPathComponent("fileid.sqlite").path
            process.executableURL = binary
            process.environment = env
            let output = Pipe(), events = Pipe()
            process.standardInput = input
            process.standardOutput = output
            process.standardError = events
            output.fileHandleForReading.readabilityHandler = { handle in
                if handle.availableData.isEmpty { handle.readabilityHandler = nil }
            }
            let buffer = MutexBox(Data())
            events.fileHandleForReading.readabilityHandler = { handle in
                let chunk = handle.availableData
                guard !chunk.isEmpty else { handle.readabilityHandler = nil; return }
                let lines: [Data] = buffer.withLock { data in
                    data.append(chunk)
                    var out: [Data] = []
                    while let newline = data.firstIndex(of: 0x0A) {
                        out.append(data.subdata(in: data.startIndex..<newline))
                        data.removeSubrange(data.startIndex...newline)
                    }
                    return out
                }
                for line in lines where line.first == 0x7B {
                    guard let event = try? IPCCoder.decoder.decode(IPCEvent.self, from: line) else { continue }
                    Task { @MainActor in
                        switch event.payload {
                        case .mergeSuggestions(let reply): replies.deliver(reply)
                        case .clipTextEmbedding(let reply): replies.deliver(reply)
                        case .thumbnailGenerated(let reply): replies.deliver(reply)
                        case .error(let error): replies.deliver(error)
                        default: break
                        }
                    }
                }
            }
            try process.run()
        }

        func send(_ payload: IPCCommand.Payload) -> Bool {
            guard process.isRunning, let line = try? IPCCoder.encodeLine(IPCCommand(payload: payload)) else { return false }
            return (try? input.fileHandleForWriting.write(contentsOf: line)) != nil
        }

        func stop() {
            _ = send(.shutdown)
            let deadline = Date().addingTimeInterval(5)
            while process.isRunning, Date() < deadline { usleep(20_000) }
            if process.isRunning { process.terminate() }
            try? FileManager.default.removeItem(at: root)
        }
    }

    nonisolated private static var engineBinary: URL? {
        guard let path = ProcessInfo.processInfo.environment["FILEID_TEST_ENGINE_PATH"],
              FileManager.default.isExecutableFile(atPath: path) else { return nil }
        return URL(fileURLWithPath: path)
    }

    @Test("Real engine replies reach the waiting caller",
          .enabled(if: engineBinary != nil && ProcessInfo.processInfo.environment["GITHUB_ACTIONS"] == nil,
                   "needs FILEID_TEST_ENGINE_PATH; process-spawning tests hang the GitHub macOS runner"),
          .timeLimit(.minutes(3)))
    func realEngineRepliesAreRouted() async throws {
        let binary = try #require(Self.engineBinary)
        let replies = EngineQueryReplies(mergeTimeout: .seconds(60), embeddingTimeout: .seconds(90),
                                         thumbnailTimeout: .seconds(5))
        let harness = try Harness(binary: binary, replies: replies)
        defer { harness.stop() }

        let pairs = await replies.mergeSuggestions { harness.send(.findMergeSuggestions) }
        #expect(pairs?.isEmpty == true)

        let started = ContinuousClock.now
        let vector = await replies.textEmbedding { harness.send(.embedTextQuery(query: "a dog on a beach", queryID: $0)) }
        #expect(vector == nil || vector?.count == CLIPEmbeddingSpace.dimension)
        #expect(ContinuousClock.now - started < .seconds(90))

        let movie = harness.root.appendingPathComponent("clip.mov")
        try await writeMovie(to: movie, width: 1280, height: 720)
        for (requested, expected) in [(nil, 192), (512, 512), (1024, 1024)] as [(Int?, Int)] {
            let size = requested ?? 192
            let jpeg = await replies.videoThumbnail(path: movie.path, modifiedAt: 12.5, maxPixelSize: size) {
                harness.send(.generateVideoThumbnail(path: movie.path, modifiedAt: 12.5, maxPixelSize: requested))
            }
            #expect(EngineQueryReplies.longSide(of: try #require(jpeg)) == expected)
        }

        let broken = harness.root.appendingPathComponent("broken.mp4")
        try Data("not a movie".utf8).write(to: broken)
        let silent = await replies.videoThumbnail(path: broken.path, modifiedAt: nil, maxPixelSize: 512) {
            harness.send(.generateVideoThumbnail(path: broken.path, modifiedAt: nil))
        }
        #expect(silent == nil)
        #expect(replies.waiting == 0)
    }

    private func writeMovie(to url: URL, width: Int, height: Int) async throws {
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: width, AVVideoHeightKey: height])
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: width, kCVPixelBufferHeightKey as String: height])
        writer.add(input)
        #expect(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        for frame in 0..<12 {
            let deadline = ContinuousClock.now.advanced(by: .seconds(10))
            while !input.isReadyForMoreMediaData {
                guard ContinuousClock.now < deadline else { throw CancellationError() }
                try await Task.sleep(for: .milliseconds(5))
            }
            var buffer: CVPixelBuffer?
            #expect(CVPixelBufferCreate(kCFAllocatorDefault, width, height, kCVPixelFormatType_32BGRA, nil, &buffer) == kCVReturnSuccess)
            let pixels = try #require(buffer)
            CVPixelBufferLockBaseAddress(pixels, [])
            let base = try #require(CVPixelBufferGetBaseAddress(pixels))
            memset(base, Int32(frame * 20), CVPixelBufferGetBytesPerRow(pixels) * height)
            CVPixelBufferUnlockBaseAddress(pixels, [])
            #expect(adaptor.append(pixels, withPresentationTime: CMTime(value: Int64(frame), timescale: 6)))
        }
        input.markAsFinished()
        await writer.finishWriting()
        #expect(writer.status == .completed)
    }
}

func makeJPEG(width: Int, height: Int) throws -> Data {
    let context = try #require(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                         space: CGColorSpaceCreateDeviceRGB(),
                                         bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
    context.setFillColor(red: 0.2, green: 0.4, blue: 0.6, alpha: 1)
    context.fill(CGRect(x: 0, y: 0, width: width, height: height))
    let image = try #require(context.makeImage())
    let data = NSMutableData()
    let destination = try #require(CGImageDestinationCreateWithData(data, "public.jpeg" as CFString, 1, nil))
    CGImageDestinationAddImage(destination, image, nil)
    #expect(CGImageDestinationFinalize(destination))
    return data as Data
}

@Suite("The app leaves the CLIP text model to the engine")
struct AppCLIPTextSessionTests {
    @Test func noAppSourceBuildsATextEncoderSession() throws {
        let sources = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("app/Sources/FileID", isDirectory: true)
        let files = try #require(FileManager.default.enumerator(at: sources, includingPropertiesForKeys: nil))
            .compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" }
        #expect(files.count > 20)
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for forbidden in ["CLIPTextEncoder.shared", "CLIPTextEncoder(", "CLIPTokenizer.shared", "ORTSession("] {
                #expect(!text.contains(forbidden), "\(file.lastPathComponent) uses \(forbidden)")
            }
        }
    }
}
