import Accelerate
import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

/// Decodes the primary image of an HEIF/HEIC file through VideoToolbox, bounded to
/// `maxPixelSize` on the long side with `irot`/`imir` applied, matching ImageIO's
/// thumbnail size and orientation.
///
/// ImageIO's HEIC path tops out at ~25-28 photos/s on an M1 Pro however many threads
/// call it; decoding the same tiles straight through VideoToolbox's hardware HEVC
/// decoder reaches ~120/s at ~30 ms CPU each (VideoToolbox falls back to its software
/// decoder only when there is no hardware one). Only the layout iPhones and Macs write
/// is handled: an `hvc1` image or a `grid` of `hvc1` tiles sharing one 8-bit 4:2:0
/// configuration, sRGB or Display P3 colour, and no clean-aperture crop. Anything else
/// returns nil and the caller uses ImageIO.
package enum HEIFDecoder {
    package static func decode(path: String, maxPixelSize: Int) -> CGImage? {
        guard maxPixelSize > 0, let file = try? read(path) else { return nil }
        return withExtendedLifetime(file) {
            try? decode(bytes: UnsafeBufferPointer(start: file.bytes, count: file.count), maxPixelSize: maxPixelSize)
        } ?? nil
    }

    /// Decodes a file the caller already read (the scan hashes the bytes first).
    package static func decode(file: FileBytes, maxPixelSize: Int) -> CGImage? {
        guard maxPixelSize > 0 else { return nil }
        return withExtendedLifetime(file) {
            try? decode(bytes: UnsafeBufferPointer(start: file.buffer.bytes, count: file.count), maxPixelSize: maxPixelSize)
        } ?? nil
    }

    /// A whole file in memory; `count` is the file's size (the buffer may be larger).
    package struct FileBytes: Sendable {
        package let buffer: PageBuffer
        package let count: Int

        package var raw: UnsafeRawBufferPointer { UnsafeRawBufferPointer(start: buffer.base, count: count) }
    }

    package static func load(path: String) -> FileBytes? {
        guard let buffer = try? read(path) else { return nil }
        return FileBytes(buffer: buffer, count: buffer.count)
    }

    /// Anonymous memory straight from the kernel, unmapped when released. Large malloc
    /// blocks freed after each photo stayed resident in the allocator's cache (+420 MB of
    /// footprint during a 1,500-photo HEIC scan).
    package final class PageBuffer: @unchecked Sendable {
        package let base: UnsafeMutableRawPointer
        package let count: Int

        package init(count: Int) throws {
            guard count > 0, let pointer = mmap(nil, count, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0),
                  pointer != MAP_FAILED else { throw Unsupported() }
            base = pointer
            self.count = count
        }

        deinit { munmap(base, count) }

        package var bytes: UnsafeMutablePointer<UInt8> { base.assumingMemoryBound(to: UInt8.self) }
    }

    /// Keeps up to `limit` bytes of released buffers for the next photo. Fresh pages cost a
    /// zero-fill fault each (a HEIC scan ran 47 instead of 55 photos/s); the cap keeps the
    /// cache from growing like malloc's.
    package final class BufferPool: @unchecked Sendable {
        package static let shared = BufferPool(limitBytes: 128 << 20)
        private let lock = NSLock()
        private let limit: Int
        private var cached: [PageBuffer] = []
        private var cachedBytes = 0

        package init(limitBytes: Int) { limit = limitBytes }

        package func take(_ count: Int) throws -> PageBuffer {
            lock.lock()
            if let index = cached.indices.filter({ cached[$0].count >= count && cached[$0].count <= count * 2 })
                .min(by: { cached[$0].count < cached[$1].count }) {
                let buffer = cached.remove(at: index)
                cachedBytes -= buffer.count
                lock.unlock()
                return buffer
            }
            lock.unlock()
            let granule = 1 << 20
            return try PageBuffer(count: (count + granule - 1) / granule * granule)
        }

        package func give(_ buffer: PageBuffer) {
            guard buffer.count <= limit else { return }
            lock.lock()
            cached.append(buffer)
            cachedBytes += buffer.count
            while cachedBytes > limit { cachedBytes -= cached.removeFirst().count }
            lock.unlock()
        }

        package var bytesCached: Int {
            lock.lock(); defer { lock.unlock() }
            return cachedBytes
        }
    }

    /// Reads into memory rather than mapping the file: a mapped file that shrinks while
    /// it is read (another app saving over it) faults the whole process.
    private static func read(_ path: String) throws -> PageBuffer {
        let descriptor = open(path, O_RDONLY | O_CLOEXEC)
        guard descriptor >= 0 else { throw Unsupported() }
        defer { close(descriptor) }
        var info = stat()
        guard fstat(descriptor, &info) == 0, info.st_size > 0, info.st_size <= 512 << 20 else { throw Unsupported() }
        let size = Int(info.st_size)
        let buffer = try PageBuffer(count: size)
        var done = 0
        while done < size {
            let count = Darwin.read(descriptor, buffer.base + done, size - done)
            if count < 0 && errno == EINTR { continue }
            guard count > 0 else { throw Unsupported() }
            done += count
        }
        return buffer
    }

    package struct Unsupported: Error {}

    /// Caps the full-resolution planes held at once (1.5 bytes per pixel): a 48 MP photo
    /// needs ~73 MB while its tiles are assembled, and a scan decodes up to 14 at a time.
    /// 160 MB measured as fast as 320 MB on 12-24 MP iPhone photos (96 MB was 15% slower).
    /// One file always proceeds, however large.
    package final class PlaneBudget: @unchecked Sendable {
        package static let shared = PlaneBudget(limitBytes: 160 << 20)
        private let condition = NSCondition()
        private let limit: Int
        private var used = 0

        package init(limitBytes: Int) { limit = limitBytes }

        package func acquire(_ bytes: Int) {
            condition.lock()
            while used > 0 && used + bytes > limit { condition.wait() }
            used += bytes
            condition.unlock()
        }

        package func release(_ bytes: Int) {
            condition.lock()
            used -= bytes
            condition.broadcast()
            condition.unlock()
        }

        package var inUse: Int {
            condition.lock(); defer { condition.unlock() }
            return used
        }
    }

    // MARK: - Container

    private struct Box {
        package let type: UInt32
        package let start: Int
        package let end: Int
    }

    private struct Cursor {
        package let bytes: UnsafeBufferPointer<UInt8>
        package var position: Int
        package let end: Int

        package init(_ bytes: UnsafeBufferPointer<UInt8>, _ start: Int, _ end: Int) {
            self.bytes = bytes; position = start; self.end = end
        }

        package mutating func uint(_ count: Int) throws -> Int {
            guard count >= 0, count <= 8, position + count <= end else { throw Unsupported() }
            var value: UInt64 = 0
            for index in 0..<count { value = value << 8 | UInt64(bytes[position + index]) }
            position += count
            guard value <= UInt64(Int.max) else { throw Unsupported() }
            return Int(value)
        }

        package mutating func u8() throws -> Int { try uint(1) }
        package mutating func u16() throws -> Int { try uint(2) }
        package mutating func u32() throws -> Int { try uint(4) }

        package mutating func skip(_ count: Int) throws {
            guard count >= 0, position + count <= end else { throw Unsupported() }
            position += count
        }
    }

    private static func fourCC(_ text: StaticString) -> UInt32 {
        text.withUTF8Buffer { $0.reduce(0) { $0 << 8 | UInt32($1) } }
    }

    private static func children(_ bytes: UnsafeBufferPointer<UInt8>, _ start: Int, _ end: Int) throws -> [Box] {
        var boxes: [Box] = []
        var cursor = Cursor(bytes, start, end)
        while cursor.position + 8 <= end {
            let boxStart = cursor.position
            var size = try cursor.u32()
            let type = UInt32(try cursor.u32())
            if size == 1 { size = try cursor.uint(8) } else if size == 0 { size = end - boxStart }
            let header = cursor.position - boxStart
            guard size >= header, size <= end - boxStart else { throw Unsupported() }
            boxes.append(Box(type: type, start: cursor.position, end: boxStart + size))
            cursor.position = boxStart + size
        }
        return boxes
    }

    private struct Location {
        package var method = 0
        package var extents: [(offset: Int, length: Int)] = []
    }

    private struct Container {
        package var primary = -1
        package var types: [Int: UInt32] = [:]
        package var locations: [Int: Location] = [:]
        package var tiles: [Int: [Int]] = [:]
        package var properties: [Box] = []
        package var associations: [Int: [Int]] = [:]
        package var idat: Box?
    }

    private static func parse(_ bytes: UnsafeBufferPointer<UInt8>) throws -> Container {
        guard let meta = try children(bytes, 0, bytes.count).first(where: { $0.type == fourCC("meta") }) else { throw Unsupported() }
        var container = Container()
        for box in try children(bytes, meta.start + 4, meta.end) {
            var cursor = Cursor(bytes, box.start, box.end)
            switch box.type {
            case fourCC("pitm"):
                let version = try cursor.u8(); try cursor.skip(3)
                container.primary = version == 0 ? try cursor.u16() : try cursor.u32()
            case fourCC("iinf"):
                let version = try cursor.u8(); try cursor.skip(3)
                _ = version == 0 ? try cursor.u16() : try cursor.u32()
                for entry in try children(bytes, cursor.position, box.end) where entry.type == fourCC("infe") {
                    var item = Cursor(bytes, entry.start, entry.end)
                    let entryVersion = try item.u8(); try item.skip(3)
                    guard entryVersion >= 2 else { throw Unsupported() }
                    let id = entryVersion == 2 ? try item.u16() : try item.u32()
                    try item.skip(2)
                    container.types[id] = UInt32(try item.u32())
                }
            case fourCC("iloc"):
                let version = try cursor.u8(); try cursor.skip(3)
                let sizes = try cursor.u8(), more = try cursor.u8()
                let offsetSize = sizes >> 4, lengthSize = sizes & 15, baseSize = more >> 4
                let indexSize = version == 1 || version == 2 ? more & 15 : 0
                let count = version < 2 ? try cursor.u16() : try cursor.u32()
                for _ in 0..<count {
                    let id = version < 2 ? try cursor.u16() : try cursor.u32()
                    var location = Location()
                    if version == 1 || version == 2 { location.method = try cursor.u16() & 15 }
                    try cursor.skip(2)
                    let base = try cursor.uint(baseSize)
                    for _ in 0..<(try cursor.u16()) {
                        try cursor.skip(indexSize)
                        let offset = try cursor.uint(offsetSize), length = try cursor.uint(lengthSize)
                        let (start, overflow) = base.addingReportingOverflow(offset)
                        guard !overflow else { throw Unsupported() }
                        location.extents.append((start, length))
                    }
                    container.locations[id] = location
                }
            case fourCC("iref"):
                let version = try cursor.u8(); try cursor.skip(3)
                for reference in try children(bytes, cursor.position, box.end) {
                    var item = Cursor(bytes, reference.start, reference.end)
                    let from = version == 0 ? try item.u16() : try item.u32()
                    var targets: [Int] = []
                    for _ in 0..<(try item.u16()) { targets.append(version == 0 ? try item.u16() : try item.u32()) }
                    if reference.type == fourCC("dimg") { container.tiles[from] = targets }
                }
            case fourCC("iprp"):
                for child in try children(bytes, box.start, box.end) {
                    if child.type == fourCC("ipco") {
                        container.properties = try children(bytes, child.start, child.end)
                    } else if child.type == fourCC("ipma") {
                        var item = Cursor(bytes, child.start, child.end)
                        let version = try item.u8(); try item.skip(2)
                        let wide = try item.u8() & 1 == 1
                        for _ in 0..<(try item.u32()) {
                            let id = version < 1 ? try item.u16() : try item.u32()
                            var indices: [Int] = []
                            for _ in 0..<(try item.u8()) {
                                indices.append(wide ? try item.u16() & 0x7fff : try item.u8() & 0x7f)
                            }
                            container.associations[id] = indices
                        }
                    }
                }
            case fourCC("idat"):
                container.idat = box
            default:
                break
            }
        }
        return container
    }

    private static func itemData(_ id: Int, _ container: Container, _ bytes: UnsafeBufferPointer<UInt8>) throws -> [UInt8] {
        guard let location = container.locations[id] else { throw Unsupported() }
        var data: [UInt8] = []
        for extent in location.extents {
            let base: Int, limit: Int
            switch location.method {
            case 0: base = 0; limit = bytes.count
            case 1:
                guard let idat = container.idat else { throw Unsupported() }
                base = idat.start; limit = idat.end
            default: throw Unsupported()
            }
            let (start, overflow) = base.addingReportingOverflow(extent.offset)
            guard !overflow, start >= base, start <= limit else { throw Unsupported() }
            let length = extent.length == 0 ? limit - start : extent.length
            guard length >= 0, length <= limit - start else { throw Unsupported() }
            data.append(contentsOf: bytes[start..<(start + length)])
        }
        return data
    }

    private static func properties(of id: Int, _ container: Container) throws -> [Box] {
        try (container.associations[id] ?? []).compactMap { index in
            guard index > 0 else { return nil }
            guard index <= container.properties.count else { throw Unsupported() }
            return container.properties[index - 1]
        }
    }

    // MARK: - Colour

    private struct Colour {
        package var matrix: Int?
        package var primaries: Int?
        package var transfer: Int?
        package var icc: Data?
    }

    private static func colour(_ boxes: [Box], _ bytes: UnsafeBufferPointer<UInt8>, into result: inout Colour) throws {
        for box in boxes where box.type == fourCC("colr") {
            var cursor = Cursor(bytes, box.start, box.end)
            let kind = UInt32(try cursor.u32())
            if kind == fourCC("nclx") {
                // 2 means "unspecified" in H.273: keep what another colour box or the stream says.
                let primaries = try cursor.u16(), transfer = try cursor.u16(), matrix = try cursor.u16()
                if primaries != 2 { result.primaries = primaries }
                if transfer != 2 { result.transfer = transfer }
                if matrix != 2 { result.matrix = matrix }
            } else if kind == fourCC("prof") || kind == fourCC("rICC") {
                result.icc = Data(bytes[cursor.position..<box.end])
            }
        }
    }

    private static func outputSpace(_ colour: Colour) -> CGColorSpace? {
        if let icc = colour.icc { return CGColorSpace(iccData: icc as CFData) }
        switch (colour.primaries, colour.transfer) {
        case (12?, 13?), (12?, 1?), (12?, 6?): return CGColorSpace(name: CGColorSpace.displayP3)
        // Unspecified primaries and transfer with no ICC profile (what ImageIO's own HEIC
        // writer stores for sRGB input) read as sRGB, as ImageIO reads them.
        case (1?, 13?), (1?, 1?), (1?, 6?), (nil, nil): return CGColorSpace(name: CGColorSpace.sRGB)
        default: return nil
        }
    }

    /// Kr and Kb for the H.273 matrix codes FileID accepts.
    private static func lumaWeights(_ matrix: Int) -> (Float, Float)? {
        switch matrix {
        case 1: return (0.2126, 0.0722)
        case 5, 6: return (0.299, 0.114)
        case 9: return (0.2627, 0.0593)
        default: return nil
        }
    }

    private static func matrixCode(_ pixelBuffer: CVPixelBuffer) -> Int? {
        guard let value = CVBufferCopyAttachment(pixelBuffer, kCVImageBufferYCbCrMatrixKey, nil) as? String else { return nil }
        if value == (kCVImageBufferYCbCrMatrix_ITU_R_709_2 as String) { return 1 }
        if value == (kCVImageBufferYCbCrMatrix_ITU_R_601_4 as String) { return 6 }
        if value == (kCVImageBufferYCbCrMatrix_ITU_R_2020 as String) { return 9 }
        return nil
    }

    // MARK: - Decode

    private struct Configuration {
        package let parameterSets: [[UInt8]]
        package let lengthSize: Int
    }

    private static func configuration(_ box: Box, _ bytes: UnsafeBufferPointer<UInt8>) throws -> Configuration {
        var cursor = Cursor(bytes, box.start, box.end)
        try cursor.skip(16)
        let chroma = try cursor.u8() & 3, lumaDepth = try cursor.u8() & 7, chromaDepth = try cursor.u8() & 7
        guard chroma == 1, lumaDepth == 0, chromaDepth == 0 else { throw Unsupported() }
        try cursor.skip(2)
        let lengthSize = (try cursor.u8() & 3) + 1
        var sets: [[UInt8]] = []
        for _ in 0..<(try cursor.u8()) {
            let type = try cursor.u8() & 0x3f
            for _ in 0..<(try cursor.u16()) {
                let length = try cursor.u16()
                guard cursor.position + length <= box.end else { throw Unsupported() }
                if (32...34).contains(type) { sets.append(Array(bytes[cursor.position..<(cursor.position + length)])) }
                try cursor.skip(length)
            }
        }
        guard sets.count >= 3 else { throw Unsupported() }
        return Configuration(parameterSets: sets, lengthSize: lengthSize)
    }

    private static func formatDescription(_ configuration: Configuration) throws -> CMFormatDescription {
        let pointers = configuration.parameterSets.map { set -> UnsafeMutablePointer<UInt8> in
            let pointer = UnsafeMutablePointer<UInt8>.allocate(capacity: set.count)
            pointer.initialize(from: set, count: set.count)
            return pointer
        }
        defer { pointers.forEach { $0.deallocate() } }
        var description: CMFormatDescription?
        let status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
            allocator: nil, parameterSetCount: pointers.count,
            parameterSetPointers: pointers.map { UnsafePointer($0) },
            parameterSetSizes: configuration.parameterSets.map(\.count),
            nalUnitHeaderLength: Int32(configuration.lengthSize), extensions: nil,
            formatDescriptionOut: &description)
        guard status == noErr, let description else { throw Unsupported() }
        return description
    }

    private final class Output: @unchecked Sendable {
        package var buffer: CVPixelBuffer?
        package var status: OSStatus = noErr
    }

    private static func decodeTile(_ data: [UInt8], session: VTDecompressionSession, format: CMFormatDescription) throws -> CVPixelBuffer {
        guard !data.isEmpty else { throw Unsupported() }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(allocator: nil, memoryBlock: nil, blockLength: data.count,
                                                 blockAllocator: nil, customBlockSource: nil, offsetToData: 0,
                                                 dataLength: data.count, flags: kCMBlockBufferAssureMemoryNowFlag,
                                                 blockBufferOut: &block) == noErr, let block,
              data.withUnsafeBytes({ CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block,
                                                                   offsetIntoDestination: 0, dataLength: data.count) }) == noErr
        else { throw Unsupported() }
        var sample: CMSampleBuffer?
        var size = data.count
        guard CMSampleBufferCreateReady(allocator: nil, dataBuffer: block, formatDescription: format, sampleCount: 1,
                                        sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1,
                                        sampleSizeArray: &size, sampleBufferOut: &sample) == noErr, let sample
        else { throw Unsupported() }
        let output = Output()
        let status = VTDecompressionSessionDecodeFrame(session, sampleBuffer: sample, flags: [], infoFlagsOut: nil) {
            status, _, image, _, _ in
            output.status = status
            output.buffer = image
        }
        VTDecompressionSessionWaitForAsynchronousFrames(session)
        guard status == noErr, output.status == noErr, let buffer = output.buffer else { throw Unsupported() }
        return buffer
    }

    private static func decode(bytes: UnsafeBufferPointer<UInt8>, maxPixelSize: Int) throws -> CGImage? {
        let container = try parse(bytes)
        let primary = container.primary
        guard let primaryType = container.types[primary] else { throw Unsupported() }
        let primaryProperties = try properties(of: primary, container)
        guard !primaryProperties.contains(where: { $0.type == fourCC("clap") }) else { throw Unsupported() }

        var columns = 1, rows = 1, width = 0, height = 0
        let tileIDs: [Int]
        if primaryType == fourCC("grid") {
            let grid = try itemData(primary, container, bytes)
            try grid.withUnsafeBufferPointer { buffer in
                var cursor = Cursor(buffer, 0, buffer.count)
                try cursor.skip(1)
                let wide = try cursor.u8() & 1 == 1
                rows = try cursor.u8() + 1
                columns = try cursor.u8() + 1
                width = wide ? try cursor.u32() : try cursor.u16()
                height = wide ? try cursor.u32() : try cursor.u16()
            }
            tileIDs = container.tiles[primary] ?? []
            guard tileIDs.count == rows * columns else { throw Unsupported() }
        } else if primaryType == fourCC("hvc1") {
            tileIDs = [primary]
        } else {
            throw Unsupported()
        }
        guard tileIDs.allSatisfy({ container.types[$0] == fourCC("hvc1") }) else { throw Unsupported() }

        var colourInfo = Colour()
        let firstTileProperties = try properties(of: tileIDs[0], container)
        try colour(firstTileProperties, bytes, into: &colourInfo)
        try colour(primaryProperties, bytes, into: &colourInfo)
        guard let space = outputSpace(colourInfo) else { throw Unsupported() }

        guard let hvcC = firstTileProperties.first(where: { $0.type == fourCC("hvcC") }) else { throw Unsupported() }
        let configurationBytes = Array(bytes[hvcC.start..<hvcC.end])
        for id in tileIDs.dropFirst() {
            guard let other = try properties(of: id, container).first(where: { $0.type == fourCC("hvcC") }),
                  bytes[other.start..<other.end].elementsEqual(configurationBytes) else { throw Unsupported() }
        }
        let format = try formatDescription(try configuration(hvcC, bytes))

        var session: VTDecompressionSession?
        let specification = [kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true] as CFDictionary
        let attributes = [kCVPixelBufferPixelFormatTypeKey: [kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                                                             kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]] as CFDictionary
        guard VTDecompressionSessionCreate(allocator: nil, formatDescription: format, decoderSpecification: specification,
                                           imageBufferAttributes: attributes, outputCallback: nil,
                                           decompressionSessionOut: &session) == noErr, let session else { throw Unsupported() }
        defer { VTDecompressionSessionInvalidate(session) }

        let chromaStrideOut = { ((width + 1) / 2) * 2 }
        var lumaPlane: PageBuffer?
        var chromaPlane: PageBuffer?
        var tileWidth = 0, tileHeight = 0
        var reserved = 0
        defer { if reserved > 0 { PlaneBudget.shared.release(reserved) } }
        var fullRange = true
        var matrix = colourInfo.matrix
        for (index, id) in tileIDs.enumerated() {
            let tile = try decodeTile(try itemData(id, container, bytes), session: session, format: format)
            let pixelFormat = CVPixelBufferGetPixelFormatType(tile)
            guard pixelFormat == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
                    || pixelFormat == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange else { throw Unsupported() }
            if index == 0 {
                tileWidth = CVPixelBufferGetWidth(tile); tileHeight = CVPixelBufferGetHeight(tile)
                if primaryType == fourCC("hvc1") { width = tileWidth; height = tileHeight }
                guard width > 0, height > 0, tileWidth % 2 == 0 || tileIDs.count == 1, tileHeight % 2 == 0 || tileIDs.count == 1,
                      columns * tileWidth >= width, rows * tileHeight >= height,
                      width <= 16_384, height <= 16_384 else { throw Unsupported() }
                fullRange = pixelFormat == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
                if matrix == nil { matrix = matrixCode(tile) }
                reserved = width * height + chromaStrideOut() * ((height + 1) / 2)
                PlaneBudget.shared.acquire(reserved)
                lumaPlane = try BufferPool.shared.take(width * height)
                chromaPlane = try BufferPool.shared.take(chromaStrideOut() * ((height + 1) / 2))
            }
            guard let lumaPlane, let chromaPlane,
                  CVPixelBufferGetWidth(tile) == tileWidth, CVPixelBufferGetHeight(tile) == tileHeight,
                  (pixelFormat == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange) == fullRange else { throw Unsupported() }
            let left = (index % columns) * tileWidth, top = (index / columns) * tileHeight
            let copyWidth = min(tileWidth, width - left), copyHeight = min(tileHeight, height - top)
            guard copyWidth > 0, copyHeight > 0 else { continue }
            CVPixelBufferLockBaseAddress(tile, .readOnly)
            defer { CVPixelBufferUnlockBaseAddress(tile, .readOnly) }
            guard let luma = CVPixelBufferGetBaseAddressOfPlane(tile, 0),
                  let chroma = CVPixelBufferGetBaseAddressOfPlane(tile, 1) else { throw Unsupported() }
            let lumaStride = CVPixelBufferGetBytesPerRowOfPlane(tile, 0)
            let chromaStride = CVPixelBufferGetBytesPerRowOfPlane(tile, 1)
            for row in 0..<copyHeight {
                memcpy(lumaPlane.base + (top + row) * width + left, luma + row * lumaStride, copyWidth)
            }
            let chromaRows = (copyHeight + 1) / 2, chromaBytes = ((copyWidth + 1) / 2) * 2
            let stride = chromaStrideOut()
            for row in 0..<chromaRows {
                memcpy(chromaPlane.base + (top / 2 + row) * stride + left, chroma + row * chromaStride, chromaBytes)
            }
        }
        guard let matrixCode = matrix, let weights = lumaWeights(matrixCode),
              let fullLuma = lumaPlane, let fullChroma = chromaPlane else { throw Unsupported() }

        let scale = min(1, Double(maxPixelSize) / Double(max(width, height)))
        // ImageIO rounds HEIC thumbnail sides to the nearest even number (708.55 → 708).
        var targetWidth = scale < 1 ? max(2, 2 * Int((Double(width) * scale / 2).rounded())) : width
        var targetHeight = scale < 1 ? max(2, 2 * Int((Double(height) * scale / 2).rounded())) : height
        // The 4:2:0 conversion needs even sizes: scale into an even buffer whose extra
        // column and row repeat the edge, then crop it off.
        let evenWidth = targetWidth + (targetWidth & 1), evenHeight = targetHeight + (targetHeight & 1)

        let scaledLuma = try BufferPool.shared.take(evenWidth * evenHeight)
        let scaledChroma = try BufferPool.shared.take(evenWidth * evenHeight / 2)
        var from = vImage_Buffer(data: fullLuma.base, height: vImagePixelCount(height),
                                 width: vImagePixelCount(width), rowBytes: width)
        var to = vImage_Buffer(data: scaledLuma.base, height: vImagePixelCount(targetHeight),
                               width: vImagePixelCount(targetWidth), rowBytes: evenWidth)
        guard vImageScale_Planar8(&from, &to, nil, vImage_Flags(kvImageNoFlags)) == kvImageNoError else { throw Unsupported() }
        let lumaRows = scaledLuma.bytes
        if evenWidth != targetWidth {
            for row in 0..<targetHeight { lumaRows[row * evenWidth + targetWidth] = lumaRows[row * evenWidth + targetWidth - 1] }
        }
        if evenHeight != targetHeight {
            memcpy(lumaRows + targetHeight * evenWidth, lumaRows + (targetHeight - 1) * evenWidth, evenWidth)
        }
        let chromaWidth = (width + 1) / 2, chromaHeight = (height + 1) / 2
        var chromaFrom = vImage_Buffer(data: fullChroma.base, height: vImagePixelCount(chromaHeight),
                                       width: vImagePixelCount(chromaWidth), rowBytes: chromaWidth * 2)
        var chromaTo = vImage_Buffer(data: scaledChroma.base, height: vImagePixelCount(evenHeight / 2),
                                     width: vImagePixelCount(evenWidth / 2), rowBytes: evenWidth)
        guard vImageScale_CbCr8(&chromaFrom, &chromaTo, nil, vImage_Flags(kvImageNoFlags)) == kvImageNoError else { throw Unsupported() }
        lumaPlane = nil
        chromaPlane = nil
        BufferPool.shared.give(fullLuma)
        BufferPool.shared.give(fullChroma)
        if reserved > 0 { PlaneBudget.shared.release(reserved); reserved = 0 }

        let (kr, kb) = weights
        let kg = 1 - kr - kb
        var coefficients = vImage_YpCbCrToARGBMatrix(Yp: 1, Cr_R: 2 - 2 * kr, Cr_G: -(2 - 2 * kr) * kr / kg,
                                                     Cb_G: -(2 - 2 * kb) * kb / kg, Cb_B: 2 - 2 * kb)
        var range = fullRange
            ? vImage_YpCbCrPixelRange(Yp_bias: 0, CbCr_bias: 128, YpRangeMax: 255, CbCrRangeMax: 255,
                                      YpMax: 255, YpMin: 0, CbCrMax: 255, CbCrMin: 0)
            : vImage_YpCbCrPixelRange(Yp_bias: 16, CbCr_bias: 128, YpRangeMax: 235, CbCrRangeMax: 240,
                                      YpMax: 255, YpMin: 0, CbCrMax: 255, CbCrMin: 0)
        var conversion = vImage_YpCbCrToARGB()
        guard vImageConvert_YpCbCrToARGB_GenerateConversion(&coefficients, &range, &conversion, kvImage420Yp8_CbCr8,
                                                            kvImageARGB8888, vImage_Flags(kvImageNoFlags)) == kvImageNoError
        else { throw Unsupported() }

        var pixels = try BufferPool.shared.take(evenWidth * evenHeight * 4)
        var lumaBuffer = vImage_Buffer(data: scaledLuma.base, height: vImagePixelCount(evenHeight),
                                       width: vImagePixelCount(evenWidth), rowBytes: evenWidth)
        var chromaBuffer = vImage_Buffer(data: scaledChroma.base, height: vImagePixelCount(evenHeight / 2),
                                         width: vImagePixelCount(evenWidth / 2), rowBytes: evenWidth)
        var argb = vImage_Buffer(data: pixels.base, height: vImagePixelCount(evenHeight),
                                 width: vImagePixelCount(evenWidth), rowBytes: evenWidth * 4)
        let rgbx: [UInt8] = [1, 2, 3, 0]
        guard vImageConvert_420Yp8_CbCr8ToARGB8888(&lumaBuffer, &chromaBuffer, &argb, &conversion, rgbx, 255,
                                                   vImage_Flags(kvImageNoFlags)) == kvImageNoError
        else { throw Unsupported() }
        BufferPool.shared.give(scaledLuma)
        BufferPool.shared.give(scaledChroma)

        if evenWidth != targetWidth || evenHeight != targetHeight {
            let cropped = try BufferPool.shared.take(targetWidth * targetHeight * 4)
            for row in 0..<targetHeight {
                memcpy(cropped.base + row * targetWidth * 4, pixels.base + row * evenWidth * 4, targetWidth * 4)
            }
            BufferPool.shared.give(pixels)
            pixels = cropped
        }

        for property in primaryProperties {
            if property.type == fourCC("irot") {
                guard property.end > property.start else { throw Unsupported() }
                let quarterTurns = Int(bytes[property.start] & 3)
                if quarterTurns != 0 {
                    let rotated = try rotate(pixels, &targetWidth, &targetHeight, counterClockwise: quarterTurns)
                    BufferPool.shared.give(pixels)
                    pixels = rotated
                }
            } else if property.type == fourCC("imir") {
                guard property.end > property.start else { throw Unsupported() }
                let mirrored = try mirror(pixels, targetWidth, targetHeight, verticalAxis: bytes[property.start] & 1 == 0)
                BufferPool.shared.give(pixels)
                pixels = mirrored
            }
        }

        let owner = Unmanaged.passRetained(pixels).toOpaque()
        guard let provider = CGDataProvider(dataInfo: owner, data: pixels.base, size: targetWidth * targetHeight * 4,
                                            releaseData: { info, _, _ in
                                                guard let info else { return }
                                                BufferPool.shared.give(Unmanaged<PageBuffer>.fromOpaque(info).takeRetainedValue())
                                            }) else {
            Unmanaged<PageBuffer>.fromOpaque(owner).release()
            return nil
        }
        return CGImage(width: targetWidth, height: targetHeight, bitsPerComponent: 8, bitsPerPixel: 32,
                       bytesPerRow: targetWidth * 4, space: space,
                       bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue),
                       provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
    }

    private static func rotate(_ pixels: PageBuffer, _ width: inout Int, _ height: inout Int, counterClockwise: Int) throws -> PageBuffer {
        let swapped = counterClockwise % 2 == 1
        let newWidth = swapped ? height : width, newHeight = swapped ? width : height
        let rotated = try BufferPool.shared.take(width * height * 4)
        let constant: UInt8 = counterClockwise == 1 ? UInt8(kRotate90DegreesCounterClockwise)
            : counterClockwise == 2 ? UInt8(kRotate180DegreesCounterClockwise) : UInt8(kRotate270DegreesCounterClockwise)
        var background: [UInt8] = [0, 0, 0, 255]
        var from = vImage_Buffer(data: pixels.base, height: vImagePixelCount(height),
                                 width: vImagePixelCount(width), rowBytes: width * 4)
        var to = vImage_Buffer(data: rotated.base, height: vImagePixelCount(newHeight),
                               width: vImagePixelCount(newWidth), rowBytes: newWidth * 4)
        guard vImageRotate90_ARGB8888(&from, &to, constant, &background, vImage_Flags(kvImageNoFlags)) == kvImageNoError
        else { throw Unsupported() }
        width = newWidth; height = newHeight
        return rotated
    }

    private static func mirror(_ pixels: PageBuffer, _ width: Int, _ height: Int, verticalAxis: Bool) throws -> PageBuffer {
        let mirrored = try BufferPool.shared.take(width * height * 4)
        var from = vImage_Buffer(data: pixels.base, height: vImagePixelCount(height),
                                 width: vImagePixelCount(width), rowBytes: width * 4)
        var to = vImage_Buffer(data: mirrored.base, height: vImagePixelCount(height),
                               width: vImagePixelCount(width), rowBytes: width * 4)
        let error = verticalAxis
            ? vImageHorizontalReflect_ARGB8888(&from, &to, vImage_Flags(kvImageNoFlags))
            : vImageVerticalReflect_ARGB8888(&from, &to, vImage_Flags(kvImageNoFlags))
        guard error == kvImageNoError else { throw Unsupported() }
        return mirrored
    }
}
