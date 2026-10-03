import AVFoundation
import AudioToolbox
import CoreVideo
import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Native video export", .serialized)
struct VideoConversionTests {
    private func movie(at url: URL) async throws {
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: 96, AVVideoHeightKey: 54])
        input.transform = CGAffineTransform(a: 0, b: 1, c: -1, d: 0, tx: 54, ty: 0)
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: 96, kCVPixelBufferHeightKey as String: 54])
        writer.add(input)
        #expect(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        for frame in 0..<30 {
            let deadline = ContinuousClock.now.advanced(by: .seconds(10))
            while !input.isReadyForMoreMediaData {
                guard ContinuousClock.now < deadline else { throw MediaTools.Failure(text: "Fixture encoder timed out.") }
                try await Task.sleep(for: .milliseconds(5))
            }
            var buffer: CVPixelBuffer?
            #expect(CVPixelBufferCreate(kCFAllocatorDefault, 96, 54, kCVPixelFormatType_32BGRA, nil, &buffer) == kCVReturnSuccess)
            let pixels = try #require(buffer)
            CVPixelBufferLockBaseAddress(pixels, [])
            let base = try #require(CVPixelBufferGetBaseAddress(pixels))
            memset(base, Int32(frame * 5), CVPixelBufferGetBytesPerRow(pixels) * 54)
            CVPixelBufferUnlockBaseAddress(pixels, [])
            #expect(adaptor.append(pixels, withPresentationTime: CMTime(value: Int64(frame), timescale: 30)))
        }
        input.markAsFinished()
        await writer.finishWriting()
        #expect(writer.status == .completed)
    }

    private func movieWithTone(in root: URL) async throws -> URL {
        let silent = root.appendingPathComponent("Silent.mov")
        try await movie(at: silent)
        var wave = Data()
        func number<T: FixedWidthInteger>(_ value: T) {
            var little = value.littleEndian
            withUnsafeBytes(of: &little) { wave.append(contentsOf: $0) }
        }
        wave.append(Data("RIFF".utf8)); number(UInt32(36 + 96_000))
        wave.append(Data("WAVEfmt ".utf8)); number(UInt32(16)); number(UInt16(1)); number(UInt16(1))
        number(UInt32(48_000)); number(UInt32(96_000)); number(UInt16(2)); number(UInt16(16))
        wave.append(Data("data".utf8)); number(UInt32(96_000))
        for sample in 0..<48_000 { number(Int16(sin(Double(sample) * 2 * .pi * 440 / 48_000) * 1000)) }
        let audioURL = root.appendingPathComponent("Tone.wav")
        try wave.write(to: audioURL)
        let movie = AVURLAsset(url: silent)
        let audio = AVURLAsset(url: audioURL)
        let videoSource = try #require(try await movie.loadTracks(withMediaType: .video).first)
        let audioSource = try #require(try await audio.loadTracks(withMediaType: .audio).first)
        let composition = AVMutableComposition()
        let videoTrack = try #require(composition.addMutableTrack(withMediaType: .video, preferredTrackID: kCMPersistentTrackID_Invalid))
        let audioTrack = try #require(composition.addMutableTrack(withMediaType: .audio, preferredTrackID: kCMPersistentTrackID_Invalid))
        let range = CMTimeRange(start: .zero, duration: CMTime(value: 1, timescale: 1))
        try videoTrack.insertTimeRange(range, of: videoSource, at: .zero)
        videoTrack.preferredTransform = try await videoSource.load(.preferredTransform)
        try audioTrack.insertTimeRange(range, of: audioSource, at: .zero)
        let output = root.appendingPathComponent("PortraitWithTone.mov")
        let exporter = try #require(AVAssetExportSession(asset: composition, presetName: AVAssetExportPresetHighestQuality))
        let location = AVMutableMetadataItem()
        location.identifier = .quickTimeMetadataLocationISO6709
        location.value = "+10.0000+020.0000/" as NSString
        location.dataType = kCMMetadataBaseDataType_UTF8 as String
        exporter.metadata = [location]
        try await exporter.export(to: output, as: .mov)
        return output
    }

    @Test func videoAndAudioTimingSurviveNativeConversion() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try await movieWithTone(in: root)
        let original = try MediaTools.hash(source)
        let sourceMetadata = try await AVURLAsset(url: source).load(.metadata)
        #expect(sourceMetadata.contains { $0.identifier == .quickTimeMetadataLocationISO6709 })
        let recipe = ToolRecipe(kind: "video", format: "mp4", maxDimension: 1920)
        let before = try await VideoConversionWorker.request(source: source, recipe: recipe)
        let output = root.appendingPathComponent(".FileIDExport-\(UUID().uuidString).part")
        let after = try await VideoConversionWorker.request(source: source, output: output, recipe: recipe)
        #expect(before.audioCount == 1 && after.audioCount == 1)
        #expect(after.audioCodec == kAudioFormatMPEG4AAC)
        let exportedAsset = AVURLAsset(url: output, options: [AVURLAssetOverrideMIMETypeKey: "video/mp4", AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue])
        let exportedMetadata = try await exportedAsset.load(.metadata)
        #expect(!exportedMetadata.contains { $0.identifier == .quickTimeMetadataLocationISO6709 })
        #expect(abs(after.duration - before.duration) < 0.1)
        #expect(after.height > after.width)
        #expect(try MediaTools.hash(source) == original)
        #expect(FileManager.default.fileExists(atPath: output.path))
    }

    @Test func portraitExportReopensPreservesOriginalAndSupportsUndo() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Portrait.mov")
        try await movie(at: source)
        let original = try MediaTools.hash(source)
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,?,1,100,0,'video','mov')", arguments: [source.path])
        }
        let tools = MediaTools()
        let recipe = ToolRecipe(kind: "video", format: "mp4", maxDimension: 1280)
        let preview = await tools.handle(ToolRequest(requestID: "preview", action: "preview", fileIDs: [1], destination: root.path, recipe: recipe), database: database)
        #expect(preview.status == "ok", Comment(rawValue: preview.message))
        let id = try #require(preview.operationID)
        let exported = await tools.handle(ToolRequest(requestID: "execute", action: "execute", destination: root.path, operationID: id), database: database)
        #expect(exported.status == "ok", Comment(rawValue: exported.message))
        let output = URL(fileURLWithPath: try #require(exported.outputs.first).outputPath)
        let probe = try await VideoConversionWorker.request(source: output, recipe: recipe)
        #expect(probe.height > probe.width)
        #expect(probe.audioCount == 0)
        #expect(abs(probe.duration - 1) < 0.1)
        #expect(try MediaTools.hash(source) == original)
        #expect(try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT kind FROM files WHERE path_text=?", arguments: [output.path]) } == "video")
        let undone = await tools.handle(ToolRequest(requestID: "undo", action: "undo", destination: root.path, operationID: id), database: database)
        #expect(undone.status == "ok")
        #expect(!FileManager.default.fileExists(atPath: output.path))
        #expect(try MediaTools.hash(source) == original)
    }

    @Test func workerRejectsProtectedOutputBeforeSourceAccessAndRejectsBrokenInput() async throws {
        #expect(await VideoConversionWorker.run(arguments: ["export", "/missing/source.mov", "1280", "/Volumes/Adlon/export.mp4"]) != 0)
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Broken.mov")
        try Data("Not a video".utf8).write(to: source)
        do {
            _ = try await VideoConversionWorker.request(source: source, recipe: ToolRecipe(kind: "video", format: "mp4", maxDimension: 1280))
            Issue.record("Broken video must not produce a valid probe.")
        } catch {}
        do {
            _ = try await VideoConversionWorker.request(source: source, recipe: ToolRecipe(kind: "video", format: "mp4", maxDimension: 4096))
            Issue.record("Unsupported video resolution must fail.")
        } catch {}
    }
}
