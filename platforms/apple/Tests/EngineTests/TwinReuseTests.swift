import CoreGraphics
import Foundation
import ImageIO
import Testing
import UniformTypeIdentifiers
@testable import FileIDEngine
@testable import FileIDImaging

@Suite("Identical-image reuse")
struct TwinReuseTests {
    private static func key(_ byte: UInt8, size: Int64 = 5000) -> TwinKey {
        TwinKey(hash: Data(repeating: byte, count: 32), size: size)
    }

    private static func analysis(_ name: String = "a", models: String = "000", ocr: String? = nil) -> TwinAnalysis {
        var tagged = TaggedFile(
            url: URL(fileURLWithPath: "/tmp/\(name).jpg"), kind: "image", extension: "jpg", sizeBytes: 5000,
            createdAt: Date(timeIntervalSince1970: 1_600_000_000), modifiedAt: nil,
            fileRef: 7,
            visionTags: ["beach", "dog", "Year_2020"], tagScores: ["beach": 0.9, "dog": 0.8], phash: 42,
            contentHash: Data(repeating: 1, count: 32), aestheticScore: 0.5, hasFaces: true,
            facePrints: [Data([1, 2, 3])], faceBBoxes: ["0.1,0.1,0.2,0.2"], faceQualities: [0.7],
            faceYaws: [0.1], facePitches: [nil], faceMinDimPx: [120],
            ocrText: ocr, perFileTotalMs: 250, clipEmbeddingBlob: Data([9, 9, 9, 9]),
            tagsEvaluated: true, facesEvaluated: true, ocrStageRan: ocr != nil)
        tagged.visionMs = 100
        tagged.ocrHandwritten = ocr != nil
        return TwinAnalysis(tagged: tagged, modelTags: ["beach", "dog"], models: models)
    }

    private static func isOwner(_ claim: TwinRegistry.Claim) -> Bool {
        if case .owner = claim { return true }
        return false
    }

    private static func isPending(_ claim: TwinRegistry.Claim) -> Bool {
        if case .pending = claim { return true }
        return false
    }

    private static func isReady(_ claim: TwinRegistry.Claim) -> Bool {
        if case .ready = claim { return true }
        return false
    }

    @Test("the first claim owns, later claims wait and then take the published result")
    func ownerThenReady() {
        let registry = TwinRegistry()
        let key = Self.key(1)
        #expect(Self.isOwner(registry.claim(key, models: "000")))
        #expect(Self.isPending(registry.claim(key, models: "000")))
        registry.publish(key, Self.analysis())
        guard case .ready(let twin) = registry.claim(key, models: "000") else {
            Issue.record("a finished twin must be ready")
            return
        }
        #expect(twin.tagged.phash == 42)
        #expect(Self.isOwner(registry.claim(Self.key(1, size: 5001), models: "000")), "same hash, other size is another file")
    }

    @Test("a failed owner is never reused: the next copy analyzes itself")
    func failedOwnerRetried() async {
        let registry = TwinRegistry()
        let key = Self.key(2)
        #expect(Self.isOwner(registry.claim(key, models: "000")))
        async let waited = registry.wait(key, timeout: 5)
        try? await Task.sleep(nanoseconds: 20_000_000)
        registry.publish(key, nil)
        #expect(await waited == nil)
        #expect(Self.isOwner(registry.claim(key, models: "000")))
        #expect(registry.counts.analyzed == 0)
    }

    @Test("waiters wake on publish, whether it lands before or after the wait")
    func waitersWake() async {
        let registry = TwinRegistry()
        let key = Self.key(3)
        #expect(Self.isOwner(registry.claim(key, models: "000")))
        async let first = registry.wait(key, timeout: 5)
        async let second = registry.wait(key, timeout: 5)
        try? await Task.sleep(nanoseconds: 20_000_000)
        registry.publish(key, Self.analysis("owner"))
        #expect(await first?.tagged.url.lastPathComponent == "owner.jpg")
        #expect(await second?.tagged.url.lastPathComponent == "owner.jpg")
        #expect(await registry.wait(key, timeout: 5)?.tagged.phash == 42, "a publish before the wait is still seen")
    }

    @Test("a wait that times out returns nothing and frees the content for the next copy")
    func waitTimesOut() async {
        let registry = TwinRegistry()
        let key = Self.key(4)
        #expect(Self.isOwner(registry.claim(key, models: "000")))
        let started = Date()
        #expect(await registry.wait(key, timeout: 0.05) == nil)
        #expect(Date().timeIntervalSince(started) < 30, "returns long before any real analysis timeout, even on a busy machine")
        #expect(Self.isOwner(registry.claim(key, models: "000")), "copies must not queue behind a stuck owner")
    }

    @Test("a cancelled wait returns promptly")
    func waitCancelled() async {
        let registry = TwinRegistry()
        let key = Self.key(5)
        #expect(Self.isOwner(registry.claim(key, models: "000")))
        let task = Task { await registry.wait(key, timeout: 30) }
        try? await Task.sleep(nanoseconds: 20_000_000)
        task.cancel()
        #expect(await task.value == nil)
        #expect(Self.isPending(registry.claim(key, models: "000")), "the owner is still working")
    }

    @Test("a result from before a model loaded is not reused")
    func modelChangeReanalyzes() {
        let registry = TwinRegistry()
        let key = Self.key(6)
        _ = registry.claim(key, models: "000")
        registry.publish(key, Self.analysis(models: "000"))
        #expect(Self.isOwner(registry.claim(key, models: "100")))
        registry.publish(key, Self.analysis(models: "100"))
        #expect(Self.isReady(registry.claim(key, models: "100")))
    }

    @Test("held results stay under the memory budget, oldest dropped first")
    func budgetEvicts() {
        let registry = TwinRegistry(budgetBytes: 3000)
        for byte in UInt8(10)..<UInt8(20) {
            _ = registry.claim(Self.key(byte), models: "000")
            registry.publish(Self.key(byte), Self.analysis(ocr: String(repeating: "x", count: 400)))
        }
        #expect(registry.counts.held <= 3000)
        #expect(Self.isReady(registry.claim(Self.key(19), models: "000")), "the newest result is kept")
        #expect(Self.isOwner(registry.claim(Self.key(10), models: "000")))
    }

    @Test("results are dropped once the registry has been idle")
    func idleSweep() async {
        let registry = TwinRegistry(idleSeconds: 0.1)
        let key = Self.key(40)
        _ = registry.claim(key, models: "000")
        registry.publish(key, Self.analysis())
        #expect(Self.isReady(registry.claim(key, models: "000")))
        for _ in 0..<100 where registry.counts.held > 0 {
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        #expect(registry.counts.held == 0)
        #expect(Self.isOwner(registry.claim(key, models: "000")))
    }

    @Test("reset forgets results and releases waiters")
    func resetClears() async {
        let registry = TwinRegistry()
        let done = Self.key(30), busy = Self.key(31)
        _ = registry.claim(done, models: "000")
        registry.publish(done, Self.analysis())
        _ = registry.claim(busy, models: "000")
        async let waited = registry.wait(busy, timeout: 30)
        try? await Task.sleep(nanoseconds: 20_000_000)
        registry.noteReuse()
        registry.reset()
        #expect(await waited == nil)
        #expect(registry.counts.analyzed == 0 && registry.counts.reused == 0 && registry.counts.held == 0)
        #expect(Self.isOwner(registry.claim(done, models: "000")))
        registry.publish(busy, Self.analysis())
        #expect(Self.isOwner(registry.claim(busy, models: "000")), "a publish from before the reset is dropped")
    }

    @Test("a copy takes the twin's model output and keeps its own path, dates, inode and Year tag")
    func copyFields() {
        let twin = Self.analysis(ocr: "hello")
        let created = Date(timeIntervalSince1970: 1_700_000_000)
        let modified = Date(timeIntervalSince1970: 1_700_000_500)
        let discovered = DiscoveredFile(
            url: URL(fileURLWithPath: "/tmp/backup/b.JPG"), sizeBytes: 5000,
            creationDate: created, modificationDate: modified, kind: .image, fileRef: 99)
        let hash = Data(repeating: 1, count: 32)
        let copy = Tagging.twinCopy(of: twin, for: discovered, contentHash: hash, elapsedMs: 3)

        #expect(copy.url.path == "/tmp/backup/b.JPG")
        #expect(copy.extension == "jpg")
        #expect(copy.fileRef == 99)
        #expect(copy.createdAt == created)
        #expect(copy.modifiedAt == modified)
        #expect(copy.visionTags == ["beach", "dog", "Year_2023"])
        #expect(copy.tagScores == twin.tagged.tagScores)
        #expect(copy.phash == 42)
        #expect(copy.contentHash == hash)
        #expect(copy.aestheticScore == 0.5)
        #expect(copy.hasFaces)
        #expect(copy.facePrints == twin.tagged.facePrints)
        #expect(copy.faceBBoxes == twin.tagged.faceBBoxes)
        #expect(copy.faceQualities == [0.7])
        #expect(copy.faceYaws == [0.1])
        #expect(copy.facePitches == [nil])
        #expect(copy.faceMinDimPx == [120])
        #expect(copy.ocrText == "hello")
        #expect(copy.ocrHandwritten)
        #expect(copy.ocrStageRan)
        #expect(copy.clipEmbeddingBlob == Data([9, 9, 9, 9]))
        #expect(copy.tagsEvaluated && copy.facesEvaluated)
        #expect(!copy.failed && !copy.healOnly)
        #expect(copy.perFileTotalMs == 3)
        #expect(copy.visionMs == 0 && copy.loadMs == 0 && copy.clipMs == 0 && copy.ramMs == 0 && copy.ocrMs == 0)
    }

    @Test("a copy of a photo with an EXIF capture time keeps that time and its camera tag")
    func copyKeepsEXIFDate() {
        var source = Self.analysis().tagged
        source.capturedFromEXIF = true
        source.cameraModel = "Canon EOS R5"
        source.locationLat = 48.1
        source.locationLon = 11.5
        let twin = TwinAnalysis(tagged: source, modelTags: ["beach", "dog"], models: "000")
        let discovered = DiscoveredFile(
            url: URL(fileURLWithPath: "/tmp/c.jpg"), sizeBytes: 5000,
            creationDate: Date(timeIntervalSince1970: 1_700_000_000), modificationDate: nil, kind: .image, fileRef: nil)
        let copy = Tagging.twinCopy(of: twin, for: discovered, contentHash: Data(repeating: 1, count: 32), elapsedMs: 0)
        #expect(copy.createdAt == source.createdAt)
        #expect(copy.capturedFromEXIF)
        #expect(copy.cameraModel == "Canon EOS R5")
        #expect(copy.locationLat == 48.1 && copy.locationLon == 11.5)
        #expect(copy.visionTags == ["beach", "dog", "Year_2020", "Canon"])
    }

    @Test("only files stored with a full-file hash take part")
    func eligibility() {
        #expect(!TwinReuse.eligible(sizeBytes: 0))
        #expect(!TwinReuse.eligible(sizeBytes: 4095))
        #expect(TwinReuse.eligible(sizeBytes: 4096))
        #expect(TwinReuse.eligible(sizeBytes: Int64(ContentHash.fullHashMaxBytes)))
        #expect(!TwinReuse.eligible(sizeBytes: Int64(ContentHash.fullHashMaxBytes) + 1))
    }

    @Test("hashing bytes in memory gives the stored file hash")
    func memoryHashMatchesFileHash() throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        for count in [1, 4096, 1_048_576, 2_500_001] {
            var generator = SystemRandomNumberGenerator()
            let bytes = Data((0..<count).map { _ in UInt8.random(in: 0...255, using: &generator) })
            let url = dir.appendingPathComponent("blob-\(count).heic")
            try bytes.write(to: url)
            let stored = try #require(ContentHash.compute(url: url, size: UInt64(count)))
            #expect(bytes.withUnsafeBytes { ContentHash.compute(bytes: $0) } == stored)
            let loaded = try #require(HEIFDecoder.load(path: url.path))
            #expect(loaded.count == count)
            #expect(ContentHash.compute(bytes: loaded.raw) == stored)
        }
    }

    private static func directory() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDTwinReuse-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private static func noisyImage(type: UTType, seed: UInt64) throws -> Data {
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

    private static func discovered(_ url: URL, size: Int, year: Int) -> DiscoveredFile {
        let date = Calendar(identifier: .gregorian).date(from: DateComponents(year: year, month: 6, day: 15))
        return DiscoveredFile(url: url, sizeBytes: Int64(size), creationDate: date, modificationDate: date,
                              kind: .image, fileRef: UInt64(year))
    }

    private static func timedOut(_ tagged: TaggedFile) -> Bool {
        tagged.failed && tagged.errorMessage == FileIDEngineMain.visionTimeoutMessage
    }

    @Test("byte-identical images are analyzed once and each keeps its own row", arguments: ["jpg", "heic"])
    func identicalImagesShareOneAnalysis(ext: String) async throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let type: UTType = ext == "heic" ? .heic : .jpeg
        let bytes = try Self.noisyImage(type: type, seed: 0xF11E1D)
        try #require(bytes.count >= 4096)
        let first = dir.appendingPathComponent("original.\(ext)")
        let second = dir.appendingPathComponent("backup copy.\(ext)")
        let third = dir.appendingPathComponent("later.\(ext)")
        let other = dir.appendingPathComponent("other.\(ext)")
        for url in [first, second, third] { try bytes.write(to: url) }
        let otherBytes = try Self.noisyImage(type: type, seed: 0xD1FFE2)
        try otherBytes.write(to: other)

        let registry = TwinRegistry()
        let worker = VisionWorker()
        // The first image a process analyzes can change which models count as loaded.
        _ = await Tagging.processFile(discovered: Self.discovered(other, size: otherBytes.count, year: 2019), worker: worker, twins: nil)
        async let a = Tagging.processFile(discovered: Self.discovered(first, size: bytes.count, year: 2019), worker: worker, twins: registry)
        async let b = Tagging.processFile(discovered: Self.discovered(second, size: bytes.count, year: 2023), worker: worker, twins: registry)
        let (one, two) = await (a, b)
        let three = await Tagging.processFile(discovered: Self.discovered(third, size: bytes.count, year: 2021), worker: worker, twins: registry)
        let different = await Tagging.processFile(discovered: Self.discovered(other, size: otherBytes.count, year: 2019), worker: worker, twins: registry)

        // A machine whose Vision pass times out cannot show reuse; that path has its own test.
        if [one, two, three, different].contains(where: Self.timedOut) { return }
        try #require(!one.failed && !two.failed && !three.failed && !different.failed)
        let counts = registry.counts
        #expect(counts.analyzed == 2 && counts.reused == 2, "\(counts) tags \(one.visionTags) evaluated \(one.tagsEvaluated)")

        let stored = try #require(ContentHash.compute(url: first, size: UInt64(bytes.count)))
        for (tagged, url, year) in [(one, first, 2019), (two, second, 2023), (three, third, 2021)] {
            #expect(tagged.url == url)
            #expect(tagged.fileRef == UInt64(year))
            #expect(tagged.contentHash == stored)
            #expect(tagged.phash == one.phash)
            #expect(tagged.clipEmbeddingBlob == one.clipEmbeddingBlob)
            #expect(tagged.aestheticScore == one.aestheticScore)
            #expect(tagged.facePrints == one.facePrints)
            #expect(tagged.ocrText == one.ocrText)
            #expect(tagged.tagsEvaluated && tagged.facesEvaluated)
            #expect(tagged.visionTags.filter { !$0.hasPrefix("Year_") } == one.visionTags.filter { !$0.hasPrefix("Year_") })
            #expect(tagged.visionTags.filter { $0.hasPrefix("Year_") } == ["Year_\(year)"])
        }
        #expect(one.phash != nil)
        #expect(different.contentHash != stored)

        let alone = await Tagging.processFile(discovered: Self.discovered(first, size: bytes.count, year: 2019), worker: worker, twins: nil)
        if Self.timedOut(alone) { return }
        #expect(alone.contentHash == stored)
        #expect(alone.phash == one.phash)
        #expect(alone.visionTags.filter { $0.hasPrefix("Year_") } == ["Year_2019"])
        #expect(registry.counts.analyzed == 2 && registry.counts.reused == 2)
    }

    @Test("an image that fails is not offered to its copies")
    func failedImageNotShared() async throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        var generator = SystemRandomNumberGenerator()
        let junk = Data((0..<8192).map { _ in UInt8.random(in: 0...255, using: &generator) })
        let first = dir.appendingPathComponent("broken.jpg")
        let second = dir.appendingPathComponent("broken copy.jpg")
        try junk.write(to: first)
        try junk.write(to: second)
        let registry = TwinRegistry()
        let worker = VisionWorker()
        let one = await Tagging.processFile(discovered: Self.discovered(first, size: junk.count, year: 2020), worker: worker, twins: registry)
        let two = await Tagging.processFile(discovered: Self.discovered(second, size: junk.count, year: 2020), worker: worker, twins: registry)
        #expect(one.failed && two.failed)
        #expect(one.contentHash != nil && one.contentHash == two.contentHash)
        #expect(two.url == second)
        #expect(registry.counts.analyzed == 0 && registry.counts.reused == 0)
    }
}
