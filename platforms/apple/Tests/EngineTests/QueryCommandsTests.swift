import Testing
import Foundation
import AVFoundation
import ImageIO
@testable import FileIDEngine
import FileIDShared

@Suite("Engine query commands", .serialized)
struct QueryCommandsTests {
    private func withDatabase(_ body: (Database, URL) async throws -> Void) async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDQueryCommands-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        try await body(try Database(at: root.appendingPathComponent("test.sqlite")), root)
    }

    @Test("findMergeSuggestions answers an empty library with no pairs")
    func emptyLibraryHasNoSuggestions() async throws {
        try await withDatabase { database, _ in
            guard case .mergeSuggestions(let reply) = await QueryCommands.mergeSuggestions(database: database) else {
                Issue.record("expected mergeSuggestions"); return
            }
            #expect(reply.pairs.isEmpty)
        }
    }

    @Test("findMergeSuggestions without a database emits the Rust engine's terminal error")
    func mergeSuggestionsWithoutDatabase() async throws {
        let capture = WireCapture()
        await FileIDEngineMain.dispatch(IPCCommand(payload: .findMergeSuggestions), coordinator: ScanCoordinator(),
                                        sink: capture.sink, database: nil)
        await capture.finish()
        let text = String(decoding: capture.bytes(), as: UTF8.self)
        #expect(text.contains("\"find_merge_suggestions_failed\""))
        #expect(!text.contains("not_implemented_yet"))
    }

    @Test("embedImageQuery returns the stored CLIP vector and an empty one when it is missing or malformed")
    func imageEmbeddingSeeds() async throws {
        try await withDatabase { database, root in
            let vector = (0..<CLIPEmbeddingSpace.dimension).map { $0 == 3 ? Float(1) : 0 }
            try await database.pool.write { db in
                for id in 1...3 {
                    try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?,?,?,1,0,'image','jpg')",
                                   arguments: [id, root.appendingPathComponent("\(id).jpg").path, id])
                }
                try db.execute(sql: "INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(1,?,'any'),(3,?,'any')",
                               arguments: [FloatBlob.encode(vector), FloatBlob.encode([1, 0])])
            }
            for (fileID, expected) in [(Int64(1), vector), (2, []), (3, [])] {
                guard case .clipTextEmbedding(let reply) = await QueryCommands.imageEmbedding(database: database, fileID: fileID, queryID: "q\(fileID)") else {
                    Issue.record("expected clipTextEmbedding"); return
                }
                #expect(reply.queryID == "q\(fileID)")
                #expect(reply.query == "file:\(fileID)")
                #expect(reply.embedding == expected)
            }
        }
    }

    @Test("embedTextQuery always answers with its query id")
    func textEmbeddingAnswers() {
        guard case .clipTextEmbedding(let reply) = QueryCommands.textEmbedding(query: "a dog on a beach", queryID: "t1") else {
            Issue.record("expected clipTextEmbedding"); return
        }
        #expect(reply.queryID == "t1")
        #expect(reply.query == "a dog on a beach")
        #expect(reply.embedding.isEmpty || reply.embedding.count == CLIPEmbeddingSpace.dimension)
    }

    @Test("generateVideoThumbnail returns a 192px JPEG and nothing for an unreadable video")
    func videoThumbnail() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDVideoThumb-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let movie = root.appendingPathComponent("clip.mov")
        try await writeMovie(to: movie, width: 384, height: 216)

        let reply = await QueryCommands.videoThumbnail(path: movie.path, modifiedAt: 12.5)
        guard case .thumbnailGenerated(let thumbnail) = reply else { Issue.record("expected thumbnailGenerated"); return }
        #expect(thumbnail.path == movie.path)
        #expect(thumbnail.modifiedAt == 12.5)
        let jpeg = try #require(Data(base64Encoded: thumbnail.bytes))
        let source = try #require(CGImageSourceCreateWithData(jpeg as CFData, nil))
        #expect(CGImageSourceGetType(source) as String? == "public.jpeg")
        let image = try #require(CGImageSourceCreateImageAtIndex(source, 0, nil))
        #expect(max(image.width, image.height) == 192)

        let garbage = root.appendingPathComponent("broken.mp4")
        try Data("not a movie".utf8).write(to: garbage)
        #expect(await QueryCommands.videoThumbnail(path: garbage.path, modifiedAt: nil) == nil)
        #expect(await QueryCommands.videoThumbnail(path: root.appendingPathComponent("missing.mp4").path, modifiedAt: nil) == nil)
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
