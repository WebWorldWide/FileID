import Foundation
import AVFoundation
import CoreImage
import Vision
import AppKit
import PDFKit
import CoreLocation

actor MediaProcessor {
    let viewModel: AppViewModel
    
    struct FileResult {
        let index: Int
        let tags: [String]
        let identities: [PersonIdentity]
        let scenePrint: VNFeaturePrintObservation?
        let thumbURL: URL?
        let error: Bool
        let hasFaces: Bool
        let cameraModel: String?
        let locationString: String?
    }
    
    init(viewModel: AppViewModel) {
        self.viewModel = viewModel
    }
    
    func startDirectoryScan(url: URL) async {
        await viewModel.log("Starting directory scan at \(url.lastPathComponent)")
        
        let fileManager = FileManager.default
        guard let enumerator = fileManager.enumerator(at: url, includingPropertiesForKeys: [.creationDateKey, .contentTypeKey], options: [.skipsHiddenFiles]) else {
            await viewModel.log("Error: Could not read directory contents.")
            await MainActor.run { viewModel.isProcessing = false }
            return
        }
        
        let validExtensions = ["jpg", "jpeg", "png", "heic", "mp4", "mov", "pdf"]
        var mediaFiles: [URL] = []
        var mappedFiles: [AppViewModel.FileStatus] = []
        
        while let fileURL = enumerator.nextObject() as? URL {
            if validExtensions.contains(fileURL.pathExtension.lowercased()) {
                mediaFiles.append(fileURL)
                mappedFiles.append(AppViewModel.FileStatus(filename: fileURL.lastPathComponent, url: fileURL, status: .pending))
                
                // Chunk UI updates to prevent MainActor locking on external drives
                if mappedFiles.count % 500 == 0 {
                    let currentBatch = mappedFiles
                    let currentCount = mediaFiles.count
                    await MainActor.run {
                        viewModel.activeFiles = currentBatch
                        viewModel.totalCount = currentCount
                    }
                }
            }
        }
        
        // Final update
        let finalBatch = mappedFiles
        let finalCount = mediaFiles.count
        await MainActor.run {
            viewModel.activeFiles = finalBatch
            viewModel.totalCount = finalCount
            viewModel.currentStatus = "Processing media via ANE..."
        }
        
        // Squeeze every ounce out of Apple Silicon intelligently
        await withTaskGroup(of: FileResult.self) { group in
            let cores = ProcessInfo.processInfo.activeProcessorCount
            let ramGB = Int(ProcessInfo.processInfo.physicalMemory / (1024 * 1024 * 1024))
            let maxConcurrent = min(cores * 2, ramGB, 64) // Safe auto-scaling limit
            var batch: [FileResult] = []
            var submitted = 0
            
            // Initial pipeline flood
            while submitted < min(maxConcurrent, mediaFiles.count) {
                let index = submitted
                let fileURL = mediaFiles[index]
                group.addTask { await self.processSingleFile(fileURL: fileURL, index: index) }
                submitted += 1
            }
            
            for await result in group {
                batch.append(result)
                
                // Keep pipeline saturated
                if submitted < mediaFiles.count {
                    let index = submitted
                    let fileURL = mediaFiles[index]
                    group.addTask { await self.processSingleFile(fileURL: fileURL, index: index) }
                    submitted += 1
                }
                
                // Batch UI updates every 100 files to completely prevent MainActor deadlocks
                if batch.count >= 100 || submitted == mediaFiles.count {
                    let currentBatch = batch
                    batch = []
                    
                    await MainActor.run {
                        for res in currentBatch {
                            let fileStatus = viewModel.activeFiles[res.index]
                            if res.error {
                                fileStatus.status = .failed
                            } else {
                                fileStatus.status = .namingRequired
                                fileStatus.aiTags = res.tags
                                fileStatus.scenePrint = res.scenePrint
                                fileStatus.thumbnailURL = res.thumbURL
                                fileStatus.hasFaces = res.hasFaces
                                fileStatus.cameraModel = res.cameraModel
                                fileStatus.locationString = res.locationString
                                
                                for id in res.identities {
                                    if !fileStatus.aiTags.contains(id.id.uuidString) {
                                        fileStatus.aiTags.append(id.id.uuidString)
                                    }
                                }
                            }
                        }
                        viewModel.processedCount += currentBatch.count
                    }
                }
            }
        }
        
        await MainActor.run {
            viewModel.currentStatus = "Naming Identities..."
            viewModel.isProcessing = false
        }
    }
    
    // Extracted CGImage loading with Zero-Copy and Scale decimation
    nonisolated private func cgImage(from url: URL) -> CGImage? {
        // Use Zero-Copy memory mapping for initial read
        guard let data = try? Data(contentsOf: url, options: .mappedIfSafe),
              let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        
        // Final optimization: Scale decimation during read to prevent SSD thrashing
        let opts = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceThumbnailMaxPixelSize: 512,
            kCGImageSourceCreateThumbnailWithTransform: true
        ] as CFDictionary
        
        return CGImageSourceCreateThumbnailAtIndex(source, 0, opts)
    }
    
    nonisolated private func processSingleFile(fileURL: URL, index: Int) async -> FileResult {
        let ext = fileURL.pathExtension.lowercased()
        let isVideo = ["mp4", "mov"].contains(ext)
        let isPDF = ext == "pdf"
        var tags: [String] = []
        var identities: [PersonIdentity] = []
        var extractedScenePrint: VNFeaturePrintObservation?
        var cameraModel: String? = nil
        var locationString: String? = nil
        
        do {
            if isVideo {
                let res = try await processVideo(at: fileURL)
                tags = res.0
            } else if isPDF {
                if let document = PDFDocument(url: fileURL), let page = document.page(at: 0) {
                    let rect = page.bounds(for: .mediaBox)
                    let width = Int(rect.width)
                    let height = Int(rect.height)
                    
                    if width > 0 && height > 0,
                       let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) {
                        
                        context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
                        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
                        page.draw(with: .mediaBox, to: context)
                        
                        if let cgImage = context.makeImage() {
                            tags = try await VisionProcessor.shared.extractTextAndEntities(from: cgImage)
                        }
                    }
                }
            } else {
                let res = try await VisionProcessor.shared.processImage(at: fileURL)
                tags = res.0
                
                if let image = cgImage(from: fileURL) {
                    // Librarian AI: Scan Images for Text ONLY if it looks like a document to save massive OCR ML cycles!
                    let isDocumentLikely = tags.contains("Document") || tags.contains("Screenshot") || tags.contains("Receipt") || tags.contains("Text") || tags.contains("Presentation")
                    
                    if isDocumentLikely {
                        if let ocrTags = try? await VisionProcessor.shared.extractTextAndEntities(from: image) {
                            tags.append(contentsOf: ocrTags)
                        }
                    }
                    
                    extractedScenePrint = try? await VisionProcessor.shared.generateScenePrint(from: image)
                    let prints = try await VisionProcessor.shared.generateFacePrints(from: image)
                    for (print, crop) in prints {
                        let identity = await FaceClusteringService.shared.cluster(facePrint: print, crop: crop)
                        identities.append(identity)
                    }
                }
            }
            
            // EXIF Extraction
            if !isPDF {
                if let source = CGImageSourceCreateWithURL(fileURL as CFURL, nil),
                   let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] {
                    
                    if let tiff = props[kCGImagePropertyTIFFDictionary] as? [CFString: Any],
                       let model = tiff[kCGImagePropertyTIFFModel] as? String {
                        cameraModel = model
                    }
                    
                    if let gps = props[kCGImagePropertyGPSDictionary] as? [CFString: Any],
                       let lat = gps[kCGImagePropertyGPSLatitude] as? Double,
                       let lon = gps[kCGImagePropertyGPSLongitude] as? Double {
                        
                        let latRef = gps[kCGImagePropertyGPSLatitudeRef] as? String ?? "N"
                        let lonRef = gps[kCGImagePropertyGPSLongitudeRef] as? String ?? "W"
                        
                        let finalLat = latRef == "S" ? -lat : lat
                        let finalLon = lonRef == "W" ? -lon : lon
                        
                        let location = CLLocation(latitude: finalLat, longitude: finalLon)
                        if let placemarks = try? await CLGeocoder().reverseGeocodeLocation(location),
                           let place = placemarks.first {
                            var locParts: [String] = []
                            if let city = place.locality { locParts.append(city) }
                            if let state = place.administrativeArea { locParts.append(state) }
                            if !locParts.isEmpty {
                                locationString = locParts.joined(separator: ", ")
                            }
                        }
                    }
                }
            }
            
            // Append formatted creation date
            if let resourceValues = try? fileURL.resourceValues(forKeys: [.creationDateKey]),
               let date = resourceValues.creationDate {
                let formatter = DateFormatter()
                formatter.dateFormat = "MM_dd_yyyy"
                tags.append(formatter.string(from: date))
            }
            
            // We do NOT rename the file yet. We just record what we found.
            
            let finalTagsToSave = tags
            let identitiesToSave = identities
            let localScenePrint = extractedScenePrint
            
            return FileResult(index: index, tags: finalTagsToSave, identities: identitiesToSave, scenePrint: localScenePrint, thumbURL: nil, error: false, hasFaces: !identitiesToSave.isEmpty, cameraModel: cameraModel, locationString: locationString)
            
        } catch {
            return FileResult(index: index, tags: [], identities: [], scenePrint: nil, thumbURL: nil, error: true, hasFaces: false, cameraModel: nil, locationString: nil)
        }
    }
    
    func preparePreviewNames() async {
        let identities = await FaceClusteringService.shared.allIdentities()
        let count = await viewModel.activeFiles.count
        
        for index in 0..<count {
            let fileStatus = await viewModel.activeFiles[index]
            if fileStatus.status == .namingRequired {
                
                var finalTags: [String] = []
                for tag in fileStatus.aiTags {
                    if let id = UUID(uuidString: tag), let identity = identities.first(where: { $0.id == id }) {
                        if !finalTags.contains(identity.name ?? "Unknown") {
                            finalTags.append(identity.name ?? "Unknown")
                        }
                    } else {
                        finalTags.append(tag)
                    }
                }
                
                let newName = generateNewFilename(original: fileStatus.filename, tags: finalTags)
                
                await MainActor.run {
                    fileStatus.status = .reviewRequired
                    fileStatus.proposedFilename = newName
                    fileStatus.aiTags = finalTags
                }
            }
        }
        
        // Semantic Duplicate Detection O(N) Sliding Window (PhD Optimization)
        var processedIndices = Set<Int>()
        var fileList: [(index: Int, file: AppViewModel.FileStatus, date: Date)] = []
        for i in 0..<count {
            let f = await viewModel.activeFiles[i]
            let date = (try? f.url.resourceValues(forKeys: [.creationDateKey]).creationDate) ?? Date.distantPast
            fileList.append((i, f, date))
        }
        
        fileList.sort { $0.date < $1.date }
        let windowSize = 20
        
        for idx in 0..<fileList.count {
            let i = fileList[idx].index
            if processedIndices.contains(i) { continue }
            guard let printA = fileList[idx].file.scenePrint else { continue }
            
            var groupUUID: UUID?
            let limit = min(idx + windowSize, fileList.count)
            
            for jdx in (idx + 1)..<limit {
                let j = fileList[jdx].index
                guard let printB = fileList[jdx].file.scenePrint else { continue }
                
                var distance: Float = 0
                try? printA.computeDistance(&distance, to: printB)
                
                if distance < 10.0 { // Very strict threshold for effectively identical photos
                    if groupUUID == nil {
                        groupUUID = UUID()
                        if let gid = groupUUID { await MainActor.run { fileList[idx].file.duplicateGroupUUID = gid } }
                    }
                    if let gid = groupUUID { await MainActor.run { fileList[jdx].file.duplicateGroupUUID = gid } }
                    processedIndices.insert(j)
                }
            }
        }
    }
    
    func applyIdentityNames(folderURL: URL) async {
        let count = await viewModel.activeFiles.count
        let doRename = await viewModel.applyFilenameRename
        let doEXIF = await viewModel.applyEXIFWrite
        
        for index in 0..<count {
            let fileStatus = await viewModel.activeFiles[index]
            if fileStatus.status == .reviewRequired, fileStatus.isSelectedForRename {
                
                let originalURL = fileStatus.url
                let newURL = doRename ? originalURL.deletingLastPathComponent().appendingPathComponent(fileStatus.proposedFilename ?? fileStatus.filename) : originalURL
                
                if doEXIF {
                    writeTagsToEXIF(originalURL: originalURL, newURL: newURL, tags: fileStatus.aiTags)
                } else if originalURL != newURL {
                    do {
                        try FileManager.default.moveItem(at: originalURL, to: newURL)
                        await MainActor.run { fileStatus.url = newURL }
                    } catch {
                        await viewModel.log("Rename failed: \(error)")
                    }
                }
                
                if originalURL != newURL || doEXIF {
                    await viewModel.log("Processed \(newURL.lastPathComponent)")
                }
                
                let resultingFileName = newURL.lastPathComponent
                await MainActor.run {
                    fileStatus.status = .completed
                    fileStatus.filename = resultingFileName
                    fileStatus.url = newURL
                }
            } else if fileStatus.status == .reviewRequired && !fileStatus.isSelectedForRename {
                await MainActor.run {
                    fileStatus.status = .completed
                }
            }
        }
    }
    
    private func writeTagsToEXIF(originalURL: URL, newURL: URL, tags: [String]) {
        let tempURL = originalURL.deletingLastPathComponent().appendingPathComponent(UUID().uuidString).appendingPathExtension(originalURL.pathExtension)
        
        guard let source = CGImageSourceCreateWithURL(originalURL as CFURL, nil),
              let type = CGImageSourceGetType(source),
              let destination = CGImageDestinationCreateWithURL(tempURL as CFURL, type, 1, nil) else {
            // Fallback to straight move if it fails parsing image data
            if originalURL != newURL { try? FileManager.default.moveItem(at: originalURL, to: newURL) }
            return
        }
        
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] ?? [:]
        var mutableProperties = properties
        
        var iptc = mutableProperties[kCGImagePropertyIPTCDictionary] as? [CFString: Any] ?? [:]
        iptc[kCGImagePropertyIPTCKeywords] = tags
        mutableProperties[kCGImagePropertyIPTCDictionary] = iptc
        
        CGImageDestinationAddImageFromSource(destination, source, 0, mutableProperties as CFDictionary)
        CGImageDestinationFinalize(destination)
        
        do {
            if FileManager.default.fileExists(atPath: newURL.path) && originalURL == newURL {
                _ = try FileManager.default.replaceItemAt(newURL, withItemAt: tempURL)
            } else {
                try FileManager.default.moveItem(at: tempURL, to: newURL)
                if originalURL != newURL { try? FileManager.default.removeItem(at: originalURL) }
            }
        } catch { }
    }
    
    nonisolated private func processVideo(at url: URL) async throws -> ([String], URL?) {
        // Extract a single frame from the middle of the video to classify the whole video
        let asset = AVAsset(url: url)
        guard let duration = try? await asset.load(.duration) else { return (["Video"], nil) }
        
        let midTime = CMTimeMultiplyByFloat64(duration, multiplier: 0.5)
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        
        do {
            let cgImage = try generator.copyCGImage(at: midTime, actualTime: nil)
            
            let tags = try await VisionProcessor.shared.extractTextAndEntities(from: cgImage)
            return (tags, nil)
            
        } catch {
            return (["Video"], nil)
        }
    }
    
    private func generateNewFilename(original: String, tags: [String]) -> String {
        let base = original.components(separatedBy: ".").first ?? "File"
        let ext = original.components(separatedBy: ".").last ?? ""
        let validTags = tags.filter { !$0.isEmpty }.prefix(3)
        let joined = validTags.joined(separator: "_").replacingOccurrences(of: " ", with: "_")
        
        // Security Audit: Sanitize AI outputs to prevent path traversal injections
        let sanitizedBase = base.replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: "\\", with: "-")
        let sanitizedJoined = joined.replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: "\\", with: "-")
        
        let safeName = sanitizedJoined.isEmpty ? sanitizedBase : "\(sanitizedBase)_\(sanitizedJoined)"
        return "\(safeName).\(ext)"
    }
}
