import Foundation
import AVFoundation
import Vision
import AppKit
import PDFKit
import CoreLocation
import SwiftData

// MARK: - Streaming File Scanner

/// Wraps FileManager.enumerator as a lazy actor — never builds a URL array.
/// Each call to next() returns one URL, keeping heap usage at O(1).
private actor FileStream {
    private let enumerator: FileManager.DirectoryEnumerator
    private let validExtensions: Set<String> = ["jpg","jpeg","png","heic","mp4","mov","pdf"]

    init?(url: URL) {
        guard let e = FileManager.default.enumerator(
            at: url,
            includingPropertiesForKeys: [.creationDateKey, .fileSizeKey, .contentTypeKey],
            options: [.skipsHiddenFiles, .skipsPackageDescendants]
        ) else { return nil }
        self.enumerator = e
    }

    /// Returns the next valid media URL, or nil when exhausted.
    func next() -> URL? {
        while let obj = enumerator.nextObject() as? URL {
            if validExtensions.contains(obj.pathExtension.lowercased()) { return obj }
        }
        return nil
    }
}

// MARK: - Sendable Result (crosses actor boundaries safely)

private struct FileResult: Sendable {
    let persistentID: PersistentIdentifier
    let tags: [String]
    let scenePrintData: Data?
    let cameraModel: String?
    let locationString: String?
    let hasFaces: Bool
    let identityIDs: [UUID]
    let aestheticScore: Double
    let failed: Bool
}

// MARK: - MediaProcessor

actor MediaProcessor {
    private let viewModel: AppViewModel
    private let container: ModelContainer
    private let performanceProfile: Int

    // Hard-capped at active processor count for memory safety
    private var workerCap: Int {
        let cores = ProcessInfo.processInfo.activeProcessorCount
        switch performanceProfile {
        case 0:  return max(1, cores / 2)   // Low Power
        case 2:  return cores * 2            // Turbo
        default: return cores               // Balanced
        }
    }

    init(viewModel: AppViewModel, container: ModelContainer, performanceProfile: Int) {
        self.viewModel = viewModel
        self.container = container
        self.performanceProfile = performanceProfile
    }

    // MARK: - Main Entry Point

    func startDirectoryScan(url: URL) async {
        await viewModel.log("Scan started: \(url.lastPathComponent)")

        guard let stream = await FileStream(url: url) else {
            await viewModel.log("Error: Cannot enumerate directory.")
            await MainActor.run { viewModel.isProcessing = false }
            return
        }

        // Use an actor-isolated semaphore to enforce concurrency cap
        let semaphore = AsyncSemaphore(limit: workerCap)

        await withTaskGroup(of: FileResult?.self) { group in
            var totalSeen = 0
            var processedTotal = 0
            var resultBatch = 0
            var resultContext = ModelContext(container)
            
            // 1. Initial burst: Fill the pipeline up to workerCap
            for _ in 0..<workerCap {
                if let fileURL = await stream.next() {
                    totalSeen += 1
                    let record = FileRecord(url: fileURL, status: .pending)
                    resultContext.insert(record)
                    let recordID = record.persistentModelID
                    group.addTask { await self.processFile(fileURL: fileURL, recordID: recordID) }
                } else {
                    break
                }
            }
            try? resultContext.save()
            
            // 2. Interleaved: For every result that finishes, add one more file to the group
            for await result in group {
                guard let result else { continue }
                
                // Update record with results
                if let record = try? resultContext.model(for: result.persistentID) as? FileRecord {
                    record.status         = result.failed ? .failed : .namingRequired
                    record.aiTags         = result.tags
                    record.cameraModel    = result.cameraModel
                    record.locationString = result.locationString
                    record.hasFaces       = result.hasFaces
                    record.scenePrintData = result.scenePrintData
                    record.aestheticScore = result.aestheticScore
                }
                
                processedTotal += 1
                resultBatch += 1
                
                // Periodic save & UI update
                if resultBatch >= 50 {
                    try? resultContext.save()
                    if processedTotal % 1000 == 0 { resultContext = ModelContext(container) }
                    resultBatch = 0
                    let pt = processedTotal
                    await MainActor.run { viewModel.processedCount = pt }
                }
                
                // Add the next file to keep the pipeline full
                // Support Pause logic here
                while await viewModel.isPaused {
                    try? await Task.sleep(nanoseconds: 100_000_000)
                }

                if let fileURL = await stream.next() {
                    totalSeen += 1
                    let record = FileRecord(url: fileURL, status: .pending)
                    resultContext.insert(record)
                    let recordID = record.persistentModelID
                    group.addTask { await self.processFile(fileURL: fileURL, recordID: recordID) }
                    
                    if totalSeen % 100 == 0 {
                        let ts = totalSeen
                        await MainActor.run { viewModel.totalCount = ts }
                    }
                }
            }
            
            try? resultContext.save()
            await MainActor.run {
                viewModel.totalCount = totalSeen
                viewModel.processedCount = processedTotal
                viewModel.currentStatus = "Naming identities…"
                viewModel.isProcessing  = false
            }
        }
    }

    // MARK: - Single File Processor (nonisolated → runs on cooperative thread pool)

    nonisolated private func processFile(fileURL: URL, recordID: PersistentIdentifier) async -> FileResult {
        let ext  = fileURL.pathExtension.lowercased()
        let isVid = ext == "mp4" || ext == "mov"
        let isPDF = ext == "pdf"

        var tags: [String] = []
        var scenePrintData: Data?
        var cameraModel: String?
        var locationString: String?
        var hasFaces = false
        var aestheticScore = 0.5
        var identityIDs: [UUID] = []

        do {
            if isVid {
                tags = (try? await processVideo(at: fileURL)) ?? ["Video"]
            } else if isPDF {
                tags = (try? await processPDF(at: fileURL)) ?? ["PDF"]
            } else {
                // Load image ONCE — shared CGImage passed to every processor
                guard let cgImage = VisionProcessor.shared.loadImage(from: fileURL) else {
                    return failed(recordID)
                }

                // Run classification + animal detection in ONE handler.perform call
                tags = (try? await VisionProcessor.shared.classifyImage(cgImage)) ?? ["Unclassified"]

                // OCR only for document-like content
                if tags.contains(where: { ["Document","Screenshot","Receipt","Text","Presentation"].contains($0) }) {
                    if let ocrTags = try? await VisionProcessor.shared.extractTextAndEntities(from: cgImage) {
                        tags.append(contentsOf: ocrTags)
                    }
                }

                // Aesthetic scoring
                let rv = try? fileURL.resourceValues(forKeys: [.fileSizeKey])
                let sizeMB = Double(rv?.fileSize ?? 0) / (1024 * 1024)
                aestheticScore = await VisionProcessor.shared.evaluateAesthetics(cgImage, fileSizeMB: sizeMB)

                // Scene print (duplicate detection)
                if let sp = try? await VisionProcessor.shared.generateScenePrint(from: cgImage) {
                    scenePrintData = try? NSKeyedArchiver.archivedData(withRootObject: sp, requiringSecureCoding: true)
                }

                // Face detection
                let facePrints = (try? await VisionProcessor.shared.generateFacePrints(from: cgImage)) ?? []
                if !facePrints.isEmpty {
                    hasFaces = true
                    // Use a fresh lightweight context per worker to avoid cross-actor sharing
                    let faceContext = ModelContext(container)
                    for (print, crop, _) in facePrints {
                        if let id = try? await FaceClusteringService.shared.cluster(
                            facePrint: print, crop: crop, fileURL: fileURL, context: faceContext
                        ) {
                            identityIDs.append(id.id)
                        }
                    }
                    try? faceContext.save()
                }

                // EXIF (no extra file open — reads from already-opened CGImageSource internally)
                let exif = VisionProcessor.shared.readEXIF(from: fileURL)
                cameraModel = exif.cameraModel
                if let lat = exif.latitude, let lon = exif.longitude {
                    locationString = await reverseGeocode(lat: lat, lon: lon,
                                                          latRef: exif.latRef, lonRef: exif.lonRef)
                }
            }

            // Creation date tag
            if let rv = try? fileURL.resourceValues(forKeys: [.creationDateKey]),
               let date = rv.creationDate {
                let f = DateFormatter()
                f.dateFormat = "yyyy_MM"
                tags.append(f.string(from: date))
            }

            tags = Array(Set(tags))  // deduplicate in-place

            return FileResult(persistentID: recordID, tags: tags, scenePrintData: scenePrintData,
                              cameraModel: cameraModel, locationString: locationString,
                              hasFaces: hasFaces, identityIDs: identityIDs, 
                              aestheticScore: aestheticScore, failed: false)
        } catch {
            return failed(recordID)
        }
    }

    nonisolated private func failed(_ id: PersistentIdentifier) -> FileResult {
        FileResult(persistentID: id, tags: [], scenePrintData: nil,
                   cameraModel: nil, locationString: nil,
                   hasFaces: false, identityIDs: [], 
                   aestheticScore: 0.0, failed: true)
    }

    // MARK: - Video

    nonisolated private func processVideo(at url: URL) async throws -> [String] {
        let asset = AVAsset(url: url)
        guard let duration = try? await asset.load(.duration) else { return ["Video"] }
        let mid = CMTimeMultiplyByFloat64(duration, multiplier: 0.5)
        let gen = AVAssetImageGenerator(asset: asset)
        gen.appliesPreferredTrackTransform = true
        gen.maximumSize = CGSize(width: 512, height: 512)

        guard let cgImage = try? gen.copyCGImage(at: mid, actualTime: nil) else { return ["Video"] }
        var tags = (try? await VisionProcessor.shared.classifyImage(cgImage)) ?? []
        tags.append("Video")
        return tags
    }

    // MARK: - PDF

    nonisolated private func processPDF(at url: URL) async throws -> [String] {
        guard let doc = PDFDocument(url: url), let page = doc.page(at: 0) else { return ["PDF"] }
        let rect = page.bounds(for: .mediaBox)
        guard rect.width > 0, rect.height > 0,
              let ctx = CGContext(data: nil, width: Int(rect.width), height: Int(rect.height),
                                  bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
            return ["PDF"]
        }
        ctx.setFillColor(CGColor(gray: 1, alpha: 1))
        ctx.fill(CGRect(origin: .zero, size: rect.size))
        page.draw(with: .mediaBox, to: ctx)
        guard let cg = ctx.makeImage() else { return ["PDF"] }
        var tags = (try? await VisionProcessor.shared.extractTextAndEntities(from: cg)) ?? []
        tags.append("PDF")
        return tags
    }

    // MARK: - Geocoding (deferred, low priority)

    nonisolated private func reverseGeocode(lat: Double, lon: Double, latRef: String?, lonRef: String?) async -> String? {
        let finalLat = (latRef ?? "N") == "S" ? -lat : lat
        let finalLon = (lonRef ?? "W") == "W" ? -lon : lon
        let location = CLLocation(latitude: finalLat, longitude: finalLon)
        guard let place = try? await CLGeocoder().reverseGeocodeLocation(location).first else { return nil }
        var parts: [String] = []
        if let city = place.locality { parts.append(city) }
        if let state = place.administrativeArea { parts.append(state) }
        return parts.isEmpty ? nil : parts.joined(separator: ", ")
    }

    // MARK: - Post-Processing

    func preparePreviewNames() async {
        let context = ModelContext(container)
        var descriptor = FetchDescriptor<FileRecord>(
            predicate: #Predicate { $0.statusValue == "namingRequired" },
            sortBy: [SortDescriptor(\.creationDate)]
        )
        descriptor.fetchLimit = 5000
        guard let files = try? context.fetch(descriptor) else { return }

        for file in files {
            let newName = generateFilename(original: file.filename, tags: file.aiTags)
            file.proposedFilename = newName
            file.status = .reviewRequired
        }
        try? context.save()
        await runDuplicateDetection(context: context)
    }

    private func runDuplicateDetection(context: ModelContext) async {
        var descriptor = FetchDescriptor<FileRecord>(sortBy: [SortDescriptor(\.creationDate)])
        descriptor.propertiesToFetch = [\.id, \.scenePrintData, \.creationDate]
        guard let files = try? context.fetch(descriptor) else { return }

        let window = 20
        var groups: [UUID: UUID] = [:]  // fileID → groupID

        for i in 0..<files.count {
            guard let dataA = files[i].scenePrintData,
                  let printA = try? NSKeyedUnarchiver.unarchivedObject(ofClass: VNFeaturePrintObservation.self, from: dataA)
            else { continue }

            for j in (i+1)..<min(i+window, files.count) {
                guard let dataB = files[j].scenePrintData,
                      let printB = try? NSKeyedUnarchiver.unarchivedObject(ofClass: VNFeaturePrintObservation.self, from: dataB)
                else { continue }
                var dist: Float = 0
                try? printA.computeDistance(&dist, to: printB)
                if dist < 10.0 {
                    let gid = groups[files[i].id] ?? UUID()
                    groups[files[i].id] = gid
                    groups[files[j].id] = gid
                }
            }
        }

        for file in files {
            if let gid = groups[file.id] { file.duplicateGroupUUID = gid }
        }
        try? context.save()
    }

    func applyIdentityNames(folderURL: URL) async {
        let context = ModelContext(container)
        let doRename = await viewModel.applyFilenameRename
        let doEXIF   = await viewModel.applyEXIFWrite

        let descriptor = FetchDescriptor<FileRecord>(predicate: #Predicate { $0.statusValue == "reviewRequired" })
        guard let files = try? context.fetch(descriptor) else { return }

        for file in files {
            guard file.isSelectedForRename else { file.status = .completed; continue }
            let src = file.url
            let dst = doRename
                ? src.deletingLastPathComponent().appendingPathComponent(file.proposedFilename ?? file.filename)
                : src

            if doEXIF { writeTagsToEXIF(src: src, dst: dst, tags: file.aiTags) }
            else if src != dst { try? FileManager.default.moveItem(at: src, to: dst) }

            file.url      = dst
            file.filename = dst.lastPathComponent
            file.status   = .completed
        }
        try? context.save()
    }

    // MARK: - Helpers

    private func generateFilename(original: String, tags: [String]) -> String {
        let parts = original.split(separator: ".")
        let base  = String(parts.first ?? "File")
        let ext   = String(parts.last  ?? "")
        let tagStr = tags.prefix(3)
            .map { $0.replacingOccurrences(of: " ", with: "_") }
            .joined(separator: "_")
        let name = tagStr.isEmpty ? base : "\(base)_\(tagStr)"
        return ext.isEmpty ? name : "\(name).\(ext)"
    }

    private func writeTagsToEXIF(src: URL, dst: URL, tags: [String]) {
        let tmp = src.deletingLastPathComponent()
            .appendingPathComponent(UUID().uuidString)
            .appendingPathExtension(src.pathExtension)
        guard let source = CGImageSourceCreateWithURL(src as CFURL, nil),
              let type   = CGImageSourceGetType(source),
              let dest   = CGImageDestinationCreateWithURL(tmp as CFURL, type, 1, nil) else { return }
        var props = (CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]) ?? [:]
        var iptc  = props[kCGImagePropertyIPTCDictionary] as? [CFString: Any] ?? [:]
        iptc[kCGImagePropertyIPTCKeywords] = tags
        props[kCGImagePropertyIPTCDictionary] = iptc
        CGImageDestinationAddImageFromSource(dest, source, 0, props as CFDictionary)
        CGImageDestinationFinalize(dest)
        try? FileManager.default.moveItem(at: tmp, to: dst)
        if src != dst { try? FileManager.default.removeItem(at: src) }
    }

    func applyFolderStructure(root: URL) async {
        let context = ModelContext(container)
        let descriptor = FetchDescriptor<FileRecord>(predicate: #Predicate {
            $0.statusValue == "completed" || $0.statusValue == "reviewRequired"
        })
        guard let files = try? context.fetch(descriptor) else { return }

        var moved = 0
        for file in files {
            let cat = fileIDCategory(for: file)  // nonisolated helper — avoids @MainActor call
            let cal = Calendar.current
            let yr  = cal.component(.year,  from: file.creationDate)
            let mo  = String(format: "%02d", cal.component(.month, from: file.creationDate))
            let dst = root.appendingPathComponent("\(yr)/\(mo)/\(cat)/\(file.url.lastPathComponent)")
            guard file.url != dst else { continue }
            do {
                try FileManager.default.createDirectory(at: dst.deletingLastPathComponent(), withIntermediateDirectories: true)
                try FileManager.default.moveItem(at: file.url, to: dst)
                file.url = dst; file.filename = dst.lastPathComponent
                moved += 1
            } catch { await viewModel.log("Move failed: \(error.localizedDescription)") }
        }
        try? context.save()
        let movedCount = moved
        await viewModel.log("Moved \(movedCount) files.")
    }

    /// Process a single new file detected by Watch Mode
    func processSingleNewFile(url: URL) async {
        let context = ModelContext(container)
        
        // Skip if already exists
        let path = url.path
        let descriptor = FetchDescriptor<FileRecord>(predicate: #Predicate { $0.url.path == path })
        if let existing = try? context.fetch(descriptor), !existing.isEmpty { return }
        
        let record = FileRecord(url: url, status: .pending)
        context.insert(record)
        try? context.save()
        
        let id = record.persistentModelID
        await processFile(fileURL: url, recordID: id)
        
        await MainActor.run {
            viewModel.totalCount += 1
            viewModel.processedCount += 1
            viewModel.log("New file detected & processed: \(url.lastPathComponent)")
        }
    }
}

// MARK: - Nonisolated category helper (mirrors FolderOrganizationView.categoryName)

nonisolated func fileIDCategory(for file: FileRecord) -> String {
    let ext = file.url.pathExtension.lowercased()
    if ext == "pdf" {
        if file.aiTags.contains("Invoice") { return "Invoices" }
        if file.aiTags.contains("Receipt") { return "Receipts" }
        if file.aiTags.contains("Tax_Document") { return "Taxes" }
        return "Documents"
    }
    if file.aiTags.contains("Screenshot") { return "Screenshots" }
    if ext == "mp4" || ext == "mov"        { return "Videos" }
    if file.hasFaces                       { return "People" }
    if file.aiTags.contains(where: { ["Landscape","Outdoor","Nature","Mountain","Beach","Sky"].contains($0) }) { return "Nature" }
    if file.aiTags.contains(where: { ["Food","Cooking"].contains($0) }) { return "Food" }
    if file.aiTags.contains(where: { ["Dog","Cat","Animal"].contains($0) }) { return "Animals" }
    return "Photos"
}

// MARK: - AsyncSemaphore (backpressure primitive)

/// A simple actor-based counting semaphore for capping task group concurrency.
actor AsyncSemaphore {
    private var count: Int
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(limit: Int) { self.count = limit }

    func wait() async {
        if count > 0 { count -= 1; return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func signal() {
        if let first = waiters.first {
            waiters.removeFirst()
            first.resume()
        } else {
            count += 1
        }
    }
}
