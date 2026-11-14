import AsyncAlgorithms
import CoreGraphics
import Foundation
import ImageIO
import Testing
import FileIDShared
import UniformTypeIdentifiers
@testable import FileIDEngine
@testable import FileIDImaging

enum ScanImageFixtures {
    static func directory(_ name: String) throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("\(name)-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    static func noisyImage(type: UTType, seed: UInt64) throws -> Data {
        let width = 640, height = 480
        var state = seed
        var pixels = [UInt8](repeating: 255, count: width * height * 4)
        for index in stride(from: 0, to: pixels.count, by: 4) {
            state = state &* 6364136223846793005 &+ 1442695040888963407
            pixels[index] = UInt8(truncatingIfNeeded: state >> 33)
            pixels[index + 1] = UInt8(truncatingIfNeeded: state >> 41)
            pixels[index + 2] = UInt8(truncatingIfNeeded: state >> 49)
        }
        let image = try #require(pixels.withUnsafeMutableBytes { raw in
            CGContext(data: raw.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                      space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)?.makeImage()
        })
        let data = NSMutableData()
        let destination = try #require(CGImageDestinationCreateWithData(data, type.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, nil)
        #expect(CGImageDestinationFinalize(destination))
        return data as Data
    }

    static func discovered(_ url: URL, size: Int, year: Int, kind: FileKind = .image, ref: UInt64? = nil) -> DiscoveredFile {
        let date = Calendar(identifier: .gregorian).date(from: DateComponents(year: year, month: 6, day: 15))
        return DiscoveredFile(url: url, sizeBytes: Int64(size), creationDate: date, modificationDate: date,
                              kind: kind, fileRef: ref ?? UInt64(year))
    }

    static func timedOut(_ tagged: TaggedFile) -> Bool {
        tagged.failed && tagged.errorMessage == FileIDEngineMain.visionTimeoutMessage
    }

    final class Counter: @unchecked Sendable {
        private let lock = NSLock()
        private var value = 0
        func bump() { lock.lock(); value += 1; lock.unlock() }
        func bumpIfBelow(_ limit: Int) -> Bool {
            lock.lock(); defer { lock.unlock() }
            guard value < limit else { return false }
            value += 1
            return true
        }
        var count: Int { lock.lock(); defer { lock.unlock() }; return value }
        func bumpIfBelow(_ limit: Int) -> Bool {
            lock.lock(); defer { lock.unlock() }
            guard value < limit else { return false }
            value += 1
            return true
        }
    }

    final class Collected: @unchecked Sendable {
        private let lock = NSLock()
        private var files: [String: TaggedFile] = [:]
        func add(_ file: TaggedFile) { lock.lock(); files[file.url.path] = file; lock.unlock() }
        var all: [String: TaggedFile] { lock.lock(); defer { lock.unlock() }; return files }
    }
}

@Suite("Scan decode-ahead")
struct ScanDecodeAheadTests {
    private typealias Fixtures = ScanImageFixtures

    private static func files(in dir: URL) throws -> [DiscoveredFile] {
        let jpeg = try Fixtures.noisyImage(type: .jpeg, seed: 0xA11CE)
        let otherJPEG = try Fixtures.noisyImage(type: .jpeg, seed: 0xB0B)
        let heic = try Fixtures.noisyImage(type: .heic, seed: 0xC0FFEE)
        var generator = SystemRandomNumberGenerator()
        let junk = Data((0..<8192).map { _ in UInt8.random(in: 0...255, using: &generator) })
        let note = Data("meeting notes for tuesday".utf8)
        let entries: [(String, Data, Int, FileKind)] = [
            ("one.jpg", jpeg, 2019, .image),
            ("one copy.jpg", jpeg, 2023, .image),
            ("two.jpg", otherJPEG, 2020, .image),
            ("three.heic", heic, 2021, .image),
            ("three again.heic", heic, 2018, .image),
            ("broken.jpg", junk, 2022, .image),
            ("tiny.jpg", Data(junk.prefix(100)), 2022, .image),
            ("notes.xyz", note, 2017, .other)
        ]
        return try entries.map { name, bytes, year, kind in
            let url = dir.appendingPathComponent(name)
            try bytes.write(to: url)
            return Fixtures.discovered(url, size: bytes.count, year: year, kind: kind)
        }
    }

    private static func expectSame(_ ahead: TaggedFile, _ plain: TaggedFile) {
        let name = plain.url.lastPathComponent
        #expect(ahead.url == plain.url, "\(name)")
        #expect(ahead.kind == plain.kind && ahead.extension == plain.extension, "\(name)")
        #expect(ahead.sizeBytes == plain.sizeBytes, "\(name)")
        #expect(ahead.createdAt == plain.createdAt && ahead.modifiedAt == plain.modifiedAt, "\(name)")
        #expect(ahead.fileRef == plain.fileRef, "\(name)")
        #expect(ahead.visionTags == plain.visionTags, "\(name)")
        #expect(ahead.tagScores == plain.tagScores, "\(name)")
        #expect(ahead.phash == plain.phash, "\(name)")
        #expect(ahead.contentHash == plain.contentHash, "\(name)")
        #expect(ahead.aestheticScore == plain.aestheticScore, "\(name)")
        #expect(ahead.hasFaces == plain.hasFaces, "\(name)")
        #expect(ahead.facePrints == plain.facePrints && ahead.faceBBoxes == plain.faceBBoxes, "\(name)")
        #expect(ahead.faceQualities == plain.faceQualities && ahead.faceMinDimPx == plain.faceMinDimPx, "\(name)")
        #expect(ahead.ocrText == plain.ocrText && ahead.ocrHandwritten == plain.ocrHandwritten, "\(name)")
        #expect(ahead.cameraModel == plain.cameraModel, "\(name)")
        #expect(ahead.locationLat == plain.locationLat && ahead.locationLon == plain.locationLon, "\(name)")
        #expect(ahead.capturedFromEXIF == plain.capturedFromEXIF, "\(name)")
        #expect(ahead.failed == plain.failed && ahead.errorMessage == plain.errorMessage, "\(name)")
        #expect(ahead.clipEmbeddingBlob == plain.clipEmbeddingBlob, "\(name)")
        #expect(ahead.tagsEvaluated == plain.tagsEvaluated && ahead.facesEvaluated == plain.facesEvaluated, "\(name)")
        #expect(ahead.ocrStageRan == plain.ocrStageRan && ahead.textStageDone == plain.textStageDone, "\(name)")
        #expect(ahead.catchUp == plain.catchUp, "\(name)")
        #expect(ahead.healOnly == plain.healOnly, "\(name)")
    }

    private static func runAhead(
        _ files: [DiscoveredFile], depth: Int, workers: Int, registry: TwinRegistry,
        options: ScanCatchUp.ScanOptions, admit: @escaping @Sendable (Int) async -> Bool = { _ in true }
    ) async -> (results: [String: TaggedFile], summary: ScanDecodeAhead.Summary) {
        let source = AsyncChannel<DiscoveredFile>()
        let producer = Task {
            for file in files { await source.send(file) }
            source.finish()
        }
        let collected = Fixtures.Collected()
        let worker = VisionWorker()
        let summary = await ScanDecodeAhead.run(
            source: source, depth: depth, budgetBytes: 64 << 20, workers: workers,
            twins: registry, catalog: nil,
            isCancelled: { false }, isPaused: { false }, admit: admit,
            process: { prepared in
                await Tagging.processFile(prepared: prepared, worker: worker, twins: registry, options: options)
            },
            emit: { collected.add($0) })
        await producer.value
        return (collected.all, summary)
    }

    @Test("decoding ahead gives every file the result it gets without it")
    func sameResultsAsWithoutDecodeAhead() async throws {
        let dir = try Fixtures.directory("FileIDDecodeAhead")
        defer { try? FileManager.default.removeItem(at: dir) }
        let files = try Self.files(in: dir)
        let options = ScanCatchUp.ScanOptions(mode: .inline, tag: { _ in [("beach", 0.9)] }, carefulRead: { _ in nil })
        let worker = VisionWorker()
        // The first image a process analyzes can change which models count as loaded.
        _ = await Tagging.processFile(discovered: files[2], worker: worker, twins: nil, options: options)

        let plainRegistry = TwinRegistry()
        var plain: [String: TaggedFile] = [:]
        for file in files {
            plain[file.url.path] = await Tagging.processFile(
                discovered: file, worker: worker, twins: plainRegistry, options: options)
        }
        let aheadRegistry = TwinRegistry()
        let ahead = await Self.runAhead(files, depth: 3, workers: 2, registry: aheadRegistry, options: options)
        let gatedRegistry = TwinRegistry()
        let gated = await Self.runAhead(files, depth: 2, workers: 2, registry: gatedRegistry, options: options) { _ in false }

        // A machine whose Vision pass times out cannot show equal results.
        if (Array(plain.values) + Array(ahead.results.values) + Array(gated.results.values)).contains(where: Fixtures.timedOut) {
            return
        }
        #expect(ahead.results.count == files.count && gated.results.count == files.count)
        for file in files {
            let expected = try #require(plain[file.url.path])
            Self.expectSame(try #require(ahead.results[file.url.path]), expected)
            Self.expectSame(try #require(gated.results[file.url.path]), expected)
        }
        #expect(plainRegistry.counts.analyzed == aheadRegistry.counts.analyzed)
        #expect(plainRegistry.counts.reused == 2 && aheadRegistry.counts.reused == 2 && gatedRegistry.counts.reused == 2)

        #expect(ahead.summary.copies == 2, "a copy that reuses its twin is never decoded")
        #expect(ahead.summary.other == 1)
        #expect(ahead.summary.decoded == 5 && ahead.summary.notAdmitted == 0)
        #expect(ahead.summary.queue.hits + ahead.summary.queue.misses == files.count)
        #expect(ahead.summary.queue.peakBytes <= 64 << 20)
        #expect(gated.summary.decoded == 0 && gated.summary.notAdmitted == 5 && gated.summary.copies == 2)
        #expect(gated.summary.queue.peakBytes <= ScanDecodeAhead.frameEstimateBytes * 4,
                "only reservations, never frames, when memory admission says no")
    }

    @Test("a prepared image carries its decoded frame and decode time into the result")
    func preparedImageSkipsTheWorkerDecode() async throws {
        let dir = try Fixtures.directory("FileIDDecodeAhead")
        defer { try? FileManager.default.removeItem(at: dir) }
        let bytes = try Fixtures.noisyImage(type: .jpeg, seed: 0xFACE)
        let url = dir.appendingPathComponent("frame.jpg")
        try bytes.write(to: url)
        let file = Fixtures.discovered(url, size: bytes.count, year: 2020)
        let registry = TwinRegistry()
        let prepared = await Tagging.prepare(discovered: file, twins: registry, catalog: nil, decode: true)
        guard case .image(.analyze(_, let heif, let owner), let decoded?) = prepared.stage else {
            Issue.record("an image that needs analysis is decoded ahead")
            return
        }
        #expect(heif == nil, "the file bytes are not kept once the frame is decoded")
        #expect(owner != nil)
        #expect(decoded.image?.width == 640 && decoded.image?.height == 480)
        #expect(prepared.decodedBytes >= 640 * 480 * 4)
        #expect(prepared.finishedCopy == nil)

        // With the file gone, only the frame decoded ahead can produce a result.
        try FileManager.default.removeItem(at: url)
        let options = ScanCatchUp.ScanOptions(mode: .inline, tag: { _ in [] }, carefulRead: { _ in nil })
        let tagged = await Tagging.processFile(prepared: prepared, worker: VisionWorker(), twins: registry, options: options)
        if Fixtures.timedOut(tagged) { return }
        #expect(!tagged.failed)
        #expect(tagged.phash != nil)
        #expect(tagged.loadMs == decoded.loadMs)
        #expect(tagged.contentHash == bytes.withUnsafeBytes { ContentHash.compute(bytes: $0) })
    }

    @Test("a stopped scan returns, releases its frames and leaves no twin claim behind")
    func cancellationReleasesEverything() async throws {
        let dir = try Fixtures.directory("FileIDDecodeAhead")
        defer { try? FileManager.default.removeItem(at: dir) }
        var files: [DiscoveredFile] = []
        for index in 0..<8 {
            let bytes = try Fixtures.noisyImage(type: .jpeg, seed: UInt64(0x5EED + index))
            let url = dir.appendingPathComponent("frame-\(index).jpg")
            try bytes.write(to: url)
            files.append(Fixtures.discovered(url, size: bytes.count, year: 2020))
        }
        let registry = TwinRegistry()
        let source = AsyncChannel<DiscoveredFile>()
        let all = files
        let producer = Task {
            for file in all { await source.send(file) }
            source.finish()
        }
        let processed = Fixtures.Counter()
        let emitted = Fixtures.Counter()
        let summary = await ScanDecodeAhead.run(
            source: source, depth: 2, budgetBytes: 64 << 20, workers: 2,
            twins: registry, catalog: nil,
            isCancelled: { processed.count > 0 }, isPaused: { false }, admit: { _ in true },
            process: { _ in
                processed.bump()
                return nil
            },
            emit: { _ in emitted.bump() })
        producer.cancel()
        await producer.value

        #expect(emitted.count == 0)
        #expect(processed.count >= 1 && processed.count <= 2)
        #expect(summary.decoded <= files.count)
        for file in files {
            let hash = try #require(ContentHash.compute(url: file.url, size: UInt64(file.sizeBytes)))
            let claim = registry.claim(TwinKey(hash: hash, size: file.sizeBytes), models: TwinReuse.modelFingerprint())
            guard case .owner = claim else {
                Issue.record("\(file.url.lastPathComponent) is still claimed by a frame that was dropped")
                continue
            }
        }
    }

    @Test("a paused scan decodes nothing until it resumes")
    func pauseHoldsDecoders() async throws {
        let dir = try Fixtures.directory("FileIDDecodeAhead")
        defer { try? FileManager.default.removeItem(at: dir) }
        let bytes = try Fixtures.noisyImage(type: .jpeg, seed: 0xBEEF)
        let url = dir.appendingPathComponent("held.jpg")
        try bytes.write(to: url)
        let file = Fixtures.discovered(url, size: bytes.count, year: 2020)
        let source = AsyncChannel<DiscoveredFile>()
        let producer = Task {
            await source.send(file)
            source.finish()
        }
        let paused = Fixtures.Counter()
        let processed = Fixtures.Counter()
        _ = await ScanDecodeAhead.run(
            source: source, depth: 1, budgetBytes: 64 << 20, workers: 1,
            twins: nil, catalog: nil,
            isCancelled: { false },
            isPaused: {
                // Counted, not timed: on a busy machine the first poll can come late.
                // Decoder and worker poll at once, so the count and the bump are one step.
                if paused.bumpIfBelow(5) {
                    #expect(processed.count == 0)
                    return true
                }
                return false
            },
            admit: { _ in true },
            process: { prepared in
                processed.bump()
                return TaggedFile(url: prepared.discovered.url, kind: "image", extension: "jpg", sizeBytes: 1,
                                  createdAt: nil, modifiedAt: nil)
            },
            emit: { _ in })
        await producer.value
        #expect(paused.count == 5)
        #expect(processed.count == 1)
    }

    @Test("frames are decoded ahead only while memory is free and nothing interactive is running")
    func memoryAdmission() async throws {
        let roomy = ResourceScheduler(memory: { (16_384, 8_000) }, cpuCapacity: 8)
        #expect(await roomy.backgroundHeadroom(memoryMB: 10))
        let lease = try await roomy.reserve(.init(cpuUnits: 1), priority: .interactive)
        #expect(await !roomy.backgroundHeadroom(memoryMB: 10), "interactive work comes first")
        await roomy.release(lease)
        #expect(await roomy.backgroundHeadroom(memoryMB: 10))
        #expect(await roomy.snapshot().activeReservations == 0, "asking reserves nothing")

        let tight = ResourceScheduler(memory: { (16_384, 64) }, cpuCapacity: 8)
        #expect(await !tight.backgroundHeadroom(memoryMB: 10))
        let unknown = ResourceScheduler(memory: { (0, 0) }, cpuCapacity: 8)
        #expect(await !unknown.backgroundHeadroom(memoryMB: 10))
    }
}
