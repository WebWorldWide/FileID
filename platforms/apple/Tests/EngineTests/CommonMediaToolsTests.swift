import AVFoundation
import AudioToolbox
import CoreVideo
import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Common media tools", .serialized)
struct CommonMediaToolsTests {
    private func silentClip(at url: URL, seconds: Int, portrait: Bool) async throws {
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: 96, AVVideoHeightKey: 54,
            AVVideoCompressionPropertiesKey: [AVVideoMaxKeyFrameIntervalKey: 15],
        ])
        if portrait { input.transform = CGAffineTransform(a: 0, b: 1, c: -1, d: 0, tx: 54, ty: 0) }
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: 96, kCVPixelBufferHeightKey as String: 54])
        writer.add(input)
        #expect(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        for frame in 0..<(seconds * 30) {
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
            for row in 0..<54 {
                let line = base.advanced(by: row * CVPixelBufferGetBytesPerRow(pixels)).assumingMemoryBound(to: UInt8.self)
                for column in 0..<96 {
                    let offset = column * 4
                    let value = UInt8((frame * 3) % 256)
                    line[offset] = column < 48 ? value : 255 - value
                    line[offset + 1] = value
                    line[offset + 2] = row < 27 ? value : 0
                    line[offset + 3] = 255
                }
            }
            CVPixelBufferUnlockBaseAddress(pixels, [])
            #expect(adaptor.append(pixels, withPresentationTime: CMTime(value: Int64(frame), timescale: 30)))
        }
        input.markAsFinished()
        await writer.finishWriting()
        #expect(writer.status == .completed)
    }

    private func clipWithTone(in root: URL, seconds: Int, portrait: Bool) async throws -> URL {
        let silent = root.appendingPathComponent("Silent-\(UUID().uuidString).mov")
        try await silentClip(at: silent, seconds: seconds, portrait: portrait)
        var wave = Data()
        func number<T: FixedWidthInteger>(_ value: T) {
            var little = value.littleEndian
            withUnsafeBytes(of: &little) { wave.append(contentsOf: $0) }
        }
        let samples = 48_000 * seconds
        wave.append(Data("RIFF".utf8)); number(UInt32(36 + samples * 2))
        wave.append(Data("WAVEfmt ".utf8)); number(UInt32(16)); number(UInt16(1)); number(UInt16(1))
        number(UInt32(48_000)); number(UInt32(96_000)); number(UInt16(2)); number(UInt16(16))
        wave.append(Data("data".utf8)); number(UInt32(samples * 2))
        for sample in 0..<samples { number(Int16(sin(Double(sample) * 2 * .pi * 440 / 48_000) * 1000)) }
        let audioURL = root.appendingPathComponent("Tone-\(UUID().uuidString).wav")
        try wave.write(to: audioURL)
        defer { try? FileManager.default.removeItem(at: audioURL); try? FileManager.default.removeItem(at: silent) }
        let movie = AVURLAsset(url: silent)
        let audio = AVURLAsset(url: audioURL)
        let videoSource = try #require(try await movie.loadTracks(withMediaType: .video).first)
        let audioSource = try #require(try await audio.loadTracks(withMediaType: .audio).first)
        let composition = AVMutableComposition()
        let videoTrack = try #require(composition.addMutableTrack(withMediaType: .video, preferredTrackID: kCMPersistentTrackID_Invalid))
        let audioTrack = try #require(composition.addMutableTrack(withMediaType: .audio, preferredTrackID: kCMPersistentTrackID_Invalid))
        let range = CMTimeRange(start: .zero, duration: CMTime(value: CMTimeValue(seconds), timescale: 1))
        try videoTrack.insertTimeRange(range, of: videoSource, at: .zero)
        videoTrack.preferredTransform = try await videoSource.load(.preferredTransform)
        try audioTrack.insertTimeRange(range, of: audioSource, at: .zero)
        let output = root.appendingPathComponent(portrait ? "Portrait.mov" : "Landscape.mov")
        let exporter = try #require(AVAssetExportSession(asset: composition, presetName: AVAssetExportPresetHighestQuality))
        let location = AVMutableMetadataItem()
        location.identifier = .quickTimeMetadataLocationISO6709
        location.value = "+10.0000+020.0000/" as NSString
        location.dataType = kCMMetadataBaseDataType_UTF8 as String
        exporter.metadata = [location]
        try await exporter.export(to: output, as: .mov)
        return output
    }

    private func catalog(_ root: URL, source: URL) async throws -> FileIDEngine.Database {
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,?,1,100,0,'video',?)", arguments: [source.path, source.pathExtension])
        }
        return database
    }

    private func scratch() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-media-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }

    private struct Run {
        var output: URL
        var operationID: String
        var message: String
    }

    private func run(_ recipe: ToolRecipe, root: URL, database: FileIDEngine.Database, tools: MediaTools) async throws -> Run {
        let preview = await tools.handle(ToolRequest(requestID: "preview", action: "preview", fileIDs: [1], destination: root.path, recipe: recipe), database: database)
        #expect(preview.status == "ok", Comment(rawValue: preview.message))
        let id = try #require(preview.operationID)
        let planned = try #require(preview.outputs.first)
        #expect(!FileManager.default.fileExists(atPath: planned.outputPath))
        let executed = await tools.handle(ToolRequest(requestID: "execute", action: "execute", destination: root.path, operationID: id), database: database)
        #expect(executed.status == "ok", Comment(rawValue: executed.message))
        let output = try #require(executed.outputs.first)
        #expect(output.state == "completed")
        #expect(output.outputPath == planned.outputPath)
        return Run(output: URL(fileURLWithPath: output.outputPath), operationID: id, message: output.message)
    }

    private func refusal(_ recipe: ToolRecipe, root: URL, database: FileIDEngine.Database) async -> String {
        let response = await MediaTools().handle(ToolRequest(requestID: "refuse", action: "preview", fileIDs: [1], destination: root.path, recipe: recipe), database: database)
        #expect(response.status == "error")
        #expect(response.operationID == nil)
        return response.message
    }

    @Test func recipeValidationRefusesWithSharedMessages() throws {
        let cases: [(ToolRecipe, String)] = [
            (ToolRecipe(kind: "trim", format: "mp4", maxDimension: 1920, videoAspectRatio: "9:16", trimStartSeconds: 0, trimEndSeconds: 1, trimMode: "keyframe"), CommonMediaTools.Message.videoOnlyFields),
            (ToolRecipe(kind: "remux", format: "mp4", maxDimension: 1920, trimStartSeconds: 0), CommonMediaTools.Message.trimOnlyFields),
            (ToolRecipe(kind: "audio", format: "m4a", maxDimension: 1920, rotationDegrees: 90), CommonMediaTools.Message.rotateOnlyFields),
            (ToolRecipe(kind: "trim", format: "m4a", maxDimension: 1920, trimStartSeconds: 0, trimEndSeconds: 1, trimMode: "keyframe"), CommonMediaTools.Message.trimFormat),
            (ToolRecipe(kind: "trim", format: "mp4", maxDimension: 1920, trimStartSeconds: 0, trimEndSeconds: 1), CommonMediaTools.Message.trimMode),
            (ToolRecipe(kind: "trim", format: "mov", maxDimension: 1920, trimStartSeconds: 0, trimEndSeconds: 1, trimMode: "exact"), CommonMediaTools.Message.exactTrimFormat),
            (ToolRecipe(kind: "trim", format: "mp4", maxDimension: 1920, trimStartSeconds: 1, trimMode: "keyframe"), CommonMediaTools.Message.trimTimes),
            (ToolRecipe(kind: "trim", format: "mp4", maxDimension: 1920, trimStartSeconds: -1, trimEndSeconds: 1, trimMode: "keyframe"), CommonMediaTools.Message.trimNegative),
            (ToolRecipe(kind: "trim", format: "mp4", maxDimension: 1920, trimStartSeconds: 2, trimEndSeconds: 2.05, trimMode: "exact"), CommonMediaTools.Message.trimRange),
            (ToolRecipe(kind: "remux", format: "m4a", maxDimension: 1920), CommonMediaTools.Message.remuxFormat),
            (ToolRecipe(kind: "audio", format: "mp4", maxDimension: 1920), CommonMediaTools.Message.audioFormat),
            (ToolRecipe(kind: "proxy", format: "mp4", maxDimension: 1280), CommonMediaTools.Message.proxyFormat),
            (ToolRecipe(kind: "rotate", format: "mp4", maxDimension: 1920, rotationDegrees: 45), CommonMediaTools.Message.rotation),
            (ToolRecipe(kind: "rotate", format: "m4a", maxDimension: 1920, rotationDegrees: 90), CommonMediaTools.Message.rotateFormat),
        ]
        for (recipe, message) in cases {
            #expect(throws: MediaTools.Failure.self) { try CommonMediaTools.validate(recipe) }
            do { try CommonMediaTools.validate(recipe); Issue.record("\(recipe) must be refused") } catch { #expect(error.localizedDescription == message) }
            #expect(!MediaTools.supports(recipe))
        }
        for recipe in [ToolRecipe(kind: "photo", format: "png", maxDimension: 64, trimMode: "exact"),
                       ToolRecipe(kind: "video", format: "mp4", maxDimension: 1920, rotationDegrees: 90)] {
            #expect(!MediaTools.supports(recipe))
        }
        for recipe in [ToolRecipe(kind: "trim", format: "mov", maxDimension: 1920, trimStartSeconds: 0.5, trimEndSeconds: 1, trimMode: "keyframe"),
                       ToolRecipe(kind: "remux", format: "mp4", maxDimension: 1920), ToolRecipe(kind: "audio", format: "m4a", maxDimension: 1920),
                       ToolRecipe(kind: "proxy", format: "mp4", maxDimension: 640), ToolRecipe(kind: "rotate", format: "mov", maxDimension: 1920, rotationDegrees: 270)] {
            #expect(MediaTools.supports(recipe))
        }
        #expect(MediaTools.capabilities.filter { CommonMediaTools.kinds.contains($0.id) && $0.available }.count == 5)
    }

    @Test func verificationRejectsChangedStreamsTimingSyncAndOrientation() throws {
        let source = CommonMediaTools.Probe(duration: 3, videoCount: 1, audioCount: 1, otherTrackCount: 0, width: 96, height: 54, rotation: 0,
                                            videoCodec: kCMVideoCodecType_H264, audioCodec: kAudioFormatMPEG4AAC, videoStart: 0, audioStart: 0, audioDuration: 3, hdr: false)
        let remux = ToolRecipe(kind: "remux", format: "mp4", maxDimension: 1920)
        try CommonMediaTools.verify(remux, source: source, output: source)
        var output = source
        output.videoCodec = kCMVideoCodecType_HEVC
        #expect(throws: MediaTools.Failure.self) { try CommonMediaTools.verify(remux, source: source, output: output) }
        output = source; output.duration = 2.5
        #expect(throws: MediaTools.Failure.self) { try CommonMediaTools.verify(remux, source: source, output: output) }
        output = source; output.audioStart = 0.3
        #expect(throws: MediaTools.Failure.self) { try CommonMediaTools.verify(remux, source: source, output: output) }
        output = source; output.audioCount = 0; output.audioCodec = nil
        #expect(throws: MediaTools.Failure.self) { try CommonMediaTools.verify(remux, source: source, output: output) }
        let rotate = ToolRecipe(kind: "rotate", format: "mov", maxDimension: 1920, rotationDegrees: 90)
        #expect(throws: MediaTools.Failure.self) { try CommonMediaTools.verify(rotate, source: source, output: source) }
        output = source; output.rotation = 90; output.width = 54; output.height = 96
        try CommonMediaTools.verify(rotate, source: source, output: output)
        let trim = ToolRecipe(kind: "trim", format: "mp4", maxDimension: 1920, trimStartSeconds: 1, trimEndSeconds: 2.5, trimMode: "keyframe")
        output = source; output.duration = 1.5; output.audioDuration = 1.5
        try CommonMediaTools.verify(trim, source: source, output: output)
        output.duration = 3
        #expect(throws: MediaTools.Failure.self) { try CommonMediaTools.verify(trim, source: source, output: output) }
    }

    @Test func remuxCopiesStreamsStripsLocationAndUndoRestores() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try await clipWithTone(in: root, seconds: 2, portrait: true)
        let original = try MediaTools.hash(source)
        let database = try await catalog(root, source: source)
        let tools = MediaTools()
        #expect(await refusal(ToolRecipe(kind: "remux", format: "mov", maxDimension: 1920), root: root, database: database) == CommonMediaTools.Message.alreadyContainer("mov"))
        let result = try await run(ToolRecipe(kind: "remux", format: "mp4", maxDimension: 1920), root: root, database: database, tools: tools)
        #expect(result.output.lastPathComponent == "Portrait - Remux.mp4")
        let before = try await CommonMediaTools.inspect(source)
        let after = try await CommonMediaTools.inspect(result.output)
        #expect(after.videoCodec == before.videoCodec && after.audioCodec == before.audioCodec)
        #expect(after.rotation == before.rotation && after.height > after.width)
        #expect(abs(after.duration - before.duration) <= 0.1)
        let metadata = try await AVURLAsset(url: result.output).load(.metadata)
        #expect(!metadata.contains { $0.identifier == .quickTimeMetadataLocationISO6709 })
        #expect(try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT role FROM catalog_assets WHERE original_id=1") } == "conversion")
        #expect(try MediaTools.hash(source) == original)
        let undone = await tools.handle(ToolRequest(requestID: "undo", action: "undo", destination: root.path, operationID: result.operationID), database: database)
        #expect(undone.status == "ok", Comment(rawValue: undone.message))
        #expect(!FileManager.default.fileExists(atPath: result.output.path))
        #expect(try MediaTools.hash(source) == original)
    }

    @Test(arguments: ["keyframe", "exact"]) func trimKeepsTheIntervalAndAudioSync(mode: String) async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try await clipWithTone(in: root, seconds: 3, portrait: false)
        let original = try MediaTools.hash(source)
        let database = try await catalog(root, source: source)
        #expect(await refusal(ToolRecipe(kind: "trim", format: "mp4", maxDimension: 1920, trimStartSeconds: 1, trimEndSeconds: 9, trimMode: mode), root: root, database: database) == CommonMediaTools.Message.trimPastEnd)
        let recipe = ToolRecipe(kind: "trim", format: "mp4", maxDimension: 1920, trimStartSeconds: 1, trimEndSeconds: 2.5, trimMode: mode)
        let result = try await run(recipe, root: root, database: database, tools: MediaTools())
        #expect(result.output.lastPathComponent == "Landscape - Trim.mp4")
        let before = try await CommonMediaTools.inspect(source)
        let after = try await CommonMediaTools.inspect(result.output)
        #expect(abs(after.duration - 1.5) <= 0.25)
        #expect(after.audioCount == 1 && after.audioCodec == kAudioFormatMPEG4AAC)
        #expect(abs((after.audioStart ?? 9) - (after.videoStart ?? 0)) <= 0.1)
        #expect(after.videoCodec == (mode == "exact" ? kCMVideoCodecType_H264 : before.videoCodec))
        #expect(after.width > after.height && after.rotation == 0)
        #expect(result.message.contains(mode == "exact" ? "frame-accurate" : "without re-encoding"))
        #expect(try MediaTools.hash(source) == original)
    }

    @Test func audioExtractionWritesAACAndRefusesSilentVideo() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try await clipWithTone(in: root, seconds: 2, portrait: false)
        let original = try MediaTools.hash(source)
        let database = try await catalog(root, source: source)
        let result = try await run(ToolRecipe(kind: "audio", format: "m4a", maxDimension: 1920), root: root, database: database, tools: MediaTools())
        #expect(result.output.pathExtension == "m4a")
        let after = try await CommonMediaTools.inspect(result.output)
        #expect(after.videoCount == 0 && after.audioCount == 1 && after.audioCodec == kAudioFormatMPEG4AAC)
        #expect(abs((after.audioDuration ?? 0) - 2) <= 0.25)
        #expect(try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT kind FROM files WHERE path_text=?", arguments: [result.output.path]) } == "audio")
        #expect(try MediaTools.hash(source) == original)

        let silentRoot = try scratch()
        defer { try? FileManager.default.removeItem(at: silentRoot) }
        let silent = silentRoot.appendingPathComponent("Silent.mov")
        try await silentClip(at: silent, seconds: 1, portrait: false)
        let silentDatabase = try await catalog(silentRoot, source: silent)
        #expect(await refusal(ToolRecipe(kind: "audio", format: "m4a", maxDimension: 1920), root: silentRoot, database: silentDatabase) == CommonMediaTools.Message.noAudio)
    }

    @Test func proxyIsSmallH264LinkedToItsSourceAndKeepsOrientation() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try await clipWithTone(in: root, seconds: 2, portrait: true)
        let original = try MediaTools.hash(source)
        let database = try await catalog(root, source: source)
        let result = try await run(ToolRecipe(kind: "proxy", format: "mp4", maxDimension: 640), root: root, database: database, tools: MediaTools())
        #expect(result.output.lastPathComponent == "Portrait - Proxy.mp4")
        let after = try await CommonMediaTools.inspect(result.output, mimeType: "video/mp4")
        #expect(after.videoCodec == kCMVideoCodecType_H264 && after.audioCodec == kAudioFormatMPEG4AAC)
        #expect(max(after.width, after.height) <= 640 && after.height > after.width)
        #expect(abs(after.duration - 2) <= 0.25)
        let link = try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT original_id || ':' || role FROM catalog_assets") }
        #expect(link == "1:proxy")
        #expect(try MediaTools.hash(source) == original)
    }

    @Test(arguments: [(false, 90), (true, 90), (false, 180), (true, 270)]) func rotationChangesOrientationWithoutReencoding(portrait: Bool, degrees: Int) async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try await clipWithTone(in: root, seconds: 1, portrait: portrait)
        let original = try MediaTools.hash(source)
        let database = try await catalog(root, source: source)
        let result = try await run(ToolRecipe(kind: "rotate", format: "mov", maxDimension: 1920, rotationDegrees: degrees), root: root, database: database, tools: MediaTools())
        #expect(result.output.lastPathComponent == (portrait ? "Portrait" : "Landscape") + " - Rotated \(degrees).mov")
        let before = try await CommonMediaTools.inspect(source)
        let after = try await CommonMediaTools.inspect(result.output)
        #expect(after.rotation == (before.rotation + degrees) % 360)
        #expect(after.videoCodec == before.videoCodec && after.audioCodec == before.audioCodec)
        #expect((after.width > after.height) == (degrees == 180 ? before.width > before.height : before.height > before.width))
        let generator = AVAssetImageGenerator(asset: AVURLAsset(url: result.output))
        generator.appliesPreferredTrackTransform = true
        let frame = try generator.copyCGImage(at: CMTime(value: 1, timescale: 2), actualTime: nil)
        #expect((frame.width > frame.height) == (after.width > after.height))
        #expect(try MediaTools.hash(source) == original)
    }

    @Test func lossyAndLosslessRecipesRefuseUnsupportedSourcesBeforeWriting() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Notes.txt")
        try Data("not media".utf8).write(to: source)
        let database = try await catalog(root, source: source)
        #expect(await refusal(ToolRecipe(kind: "remux", format: "mp4", maxDimension: 1920), root: root, database: database) == CommonMediaTools.Message.input)
        let broken = root.appendingPathComponent("Broken.mov")
        try Data("not media".utf8).write(to: broken)
        try await database.pool.write { db in try db.execute(sql: "UPDATE files SET path_text=? WHERE id=1", arguments: [broken.path]) }
        #expect(await refusal(ToolRecipe(kind: "proxy", format: "mp4", maxDimension: 960), root: root, database: database) != "")
        let contents = try FileManager.default.contentsOfDirectory(atPath: root.path).filter { $0.contains(" - ") || $0.hasPrefix(".FileIDExport-") }
        #expect(contents.isEmpty)
        #expect(try await database.pool.read { db in try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM catalog_operations") } == 0)
    }
}
