import Foundation
import ImageIO
import UniformTypeIdentifiers
import CryptoKit
import GRDB
import FileIDShared

public actor MediaTools {
    public static let shared = MediaTools()
    private var activeOperations = Set<String>()
    private var executions: [String: Task<ToolResponse, Error>] = [:]
    struct Item: Codable, Sendable {
        var output: ToolOutput
        var sourceHash: String
        var chapters: [CatalogChapter]
    }
    struct Plan: Codable, Sendable {
        var version = 1
        var type = "export"
        var recipe: ToolRecipe
        var items: [Item]
        var stagePaths: [String]? = nil
    }
    struct Receipt: Codable, Sendable {
        var output: ToolOutput
        var hash: String
        var derivedID: Int64?
        var recoveryPath: String?
    }
    struct Failure: LocalizedError {
        var text: String
        var errorDescription: String? { text }
    }

    public func handle(_ request: ToolRequest, database: Database) async -> ToolResponse {
        if request.action == "cancel" {
            guard let id = request.operationID, let task = executions[id] else { return ToolResponse(requestID: request.requestID, status: "error", message: "No running export matches this request.") }
            task.cancel()
            return ToolResponse(requestID: request.requestID, message: "Stopping the current worker. Completed exports remain recoverable.", operationID: id)
        }
        if let id = request.operationID, !activeOperations.insert(id).inserted { return ToolResponse(requestID: request.requestID, status: "error", message: "This operation is already running.") }
        defer { if let id = request.operationID { activeOperations.remove(id) } }
        do {
            guard !request.requestID.isEmpty, request.requestID.count <= 200 else { throw Failure(text: "Invalid tools request.") }
            switch request.action {
            case "capabilities":
                return ToolResponse(requestID: request.requestID, capabilities: Self.capabilities)
            case "history":
                let id = try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT id FROM catalog_operations WHERE json_extract(plan_json,'$.type')='export' AND state IN ('completed','failed') ORDER BY rowid DESC LIMIT 1") }
                guard let id else { return ToolResponse(requestID: request.requestID, message: "No completed export history.") }
                let (_, receipts, _) = try await load(id, database: database)
                return ToolResponse(requestID: request.requestID, message: "Last export operation.", operationID: id, outputs: receipts.map(\.output))
            case "preview": return try await preview(request, database: database)
            case "execute":
                guard executions.isEmpty else { throw Failure(text: "Another export is running. Wait for it or cancel it before starting this plan.") }
                guard let id = request.operationID else { throw Failure(text: "Preview an export first.") }
                let task = Task { try await self.execute(request, database: database) }
                executions[id] = task
                defer { executions.removeValue(forKey: id) }
                return try await task.value
            case "undo": return try await undo(request, database: database)
            default: throw Failure(text: "Unsupported tools action.")
            }
        } catch {
            return ToolResponse(requestID: request.requestID, status: "error", message: error.localizedDescription, operationID: request.operationID)
        }
    }

    static var capabilities: [ToolCapability] {
        [ToolCapability(id: "photo", available: true, inputFormats: ["png", "jpeg", "tiff", "heic"], outputFormats: ["png", "jpeg", "tiff"], detail: "Single-image conversion and bounded downsize. Orientation is applied; location and camera metadata are stripped. Output is 8-bit SDR; this is not AI enhancement. JPEG transparency is flattened onto white."),
         ToolCapability(id: "chapters", available: true, inputFormats: ["catalog chapters"], outputFormats: ["json", "vtt"], detail: "Export non-stale chapter markers. WebVTT is a chapter cue list, not a speech transcript."),
         ToolCapability(id: "videoEnhancement", available: false, inputFormats: [], outputFormats: [], detail: "Stabilization, AI upscaling, and tracked reframing are not installed yet.")]
    }

    public func recover(database: Database) async {
        let plans: [String] = (try? await database.pool.write { db in
            try db.execute(sql: "UPDATE catalog_operations SET state='failed' WHERE state='running' AND json_extract(plan_json,'$.type')='export'")
            return try String.fetchAll(db, sql: "SELECT plan_json FROM catalog_operations WHERE state='failed' AND json_extract(plan_json,'$.type')='export'")
        }) ?? []
        let abandoned = plans.compactMap { try? JSONDecoder().decode(Plan.self, from: Data($0.utf8)) }
        guard !abandoned.isEmpty else { return }
        Task.detached {
            try? await Task.sleep(for: .seconds(2))
            for plan in abandoned {
                for path in plan.stagePaths ?? [] {
                    let url = URL(fileURLWithPath: path)
                    let name = url.deletingPathExtension().lastPathComponent
                    guard url.pathExtension == "part", name.hasPrefix(".FileIDExport-"), UUID(uuidString: String(name.dropFirst(".FileIDExport-".count))) != nil,
                          plan.items.contains(where: { URL(fileURLWithPath: $0.output.outputPath).deletingLastPathComponent() == url.deletingLastPathComponent() }),
                          (try? ReadOnlyLocations.requireWritable(url)) != nil,
                          let values = try? url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey]), values.isRegularFile == true, values.isSymbolicLink != true else { continue }
                    try? FileManager.default.removeItem(at: url)
                }
            }
        }
    }

    private func preview(_ request: ToolRequest, database: Database) async throws -> ToolResponse {
        guard let ids = request.fileIDs, !ids.isEmpty, ids.count <= 100, Set(ids).count == ids.count,
              let destination = request.destination, destination.hasPrefix("/"),
              let recipe = request.recipe, (1...8192).contains(recipe.maxDimension),
              (recipe.kind == "photo" && ["png", "jpeg", "tiff"].contains(recipe.format)) || (recipe.kind == "chapters" && ["json", "vtt"].contains(recipe.format)) else { throw Failure(text: "Choose files, an output folder, and a supported recipe.") }
        let directory = URL(fileURLWithPath: destination).standardizedFileURL
        try ReadOnlyLocations.requireSourceMutation(directory)
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: directory.path, isDirectory: &isDirectory), isDirectory.boolValue else { throw Failure(text: "The output folder must already exist.") }
        let id = UUID().uuidString
        var items: [Item] = []
        var reserved = Set<String>()
        for fileID in ids {
            let source: String? = try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT path_text FROM files WHERE id=?", arguments: [fileID]) }
            guard let source else { throw Failure(text: "A selected file is no longer in the catalog.") }
            let sourceURL = URL(fileURLWithPath: source)
            let chapters = try await database.pool.read { db in try CatalogStore.chapters(db, fileID: fileID).filter { !$0.stale } }
            if recipe.kind == "chapters", chapters.isEmpty { throw Failure(text: "A selected file has no current chapter markers.") }
            if recipe.kind == "photo" { try Self.validateImage(sourceURL) }
            let sourceHash = try Self.hash(sourceURL)
            let stem = String(sourceURL.deletingPathExtension().lastPathComponent.prefix(50)) + (recipe.kind == "chapters" ? " - Chapters" : " - Export")
            let ext = recipe.format == "jpeg" ? "jpg" : recipe.format
            var index = 1
            var output: URL
            repeat {
                let suffix = index == 1 ? "" : " (\(index))"
                output = directory.appendingPathComponent(stem + suffix + "." + ext)
                index += 1
                try ReadOnlyLocations.requireSourceMutation(output)
            } while FileManager.default.fileExists(atPath: output.path) || reserved.contains(output.lastPathComponent.lowercased())
            reserved.insert(output.lastPathComponent.lowercased())
            items.append(Item(output: ToolOutput(fileID: fileID, sourcePath: source, outputPath: output.path), sourceHash: sourceHash, chapters: chapters))
        }
        let plan = Plan(recipe: recipe, items: items)
        let encoded = String(decoding: try JSONEncoder().encode(plan), as: UTF8.self)
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?,?,'[]','preview',?)", arguments: [id, encoded, Date().timeIntervalSince1970])
        }
        return ToolResponse(requestID: request.requestID, message: "Creates \(items.count) new file(s). Originals are preserved. Photo outputs strip metadata and normalize to 8-bit SDR.", operationID: id, outputs: items.map(\.output))
    }

    private func load(_ id: String, database: Database) async throws -> (Plan, [Receipt], String) {
        let values: (String, String, String)? = try await database.pool.read { db in
            try Row.fetchOne(db, sql: "SELECT plan_json,inverse_json,state FROM catalog_operations WHERE id=?", arguments: [id]).map { ($0["plan_json"], $0["inverse_json"], $0["state"]) }
        }
        guard let values, let plan = try? JSONDecoder().decode(Plan.self, from: Data(values.0.utf8)), plan.type == "export", plan.version == 1 else { throw Failure(text: "The export plan is unavailable.") }
        return (plan, try JSONDecoder().decode([Receipt].self, from: Data(values.1.utf8)), values.2)
    }

    private func execute(_ request: ToolRequest, database: Database) async throws -> ToolResponse {
        guard let id = request.operationID else { throw Failure(text: "Preview an export first.") }
        let loaded = try await load(id, database: database)
        var plan = loaded.0
        let previous = loaded.1
        let state = loaded.2
        guard state == "preview", previous.isEmpty else { throw Failure(text: "This plan was already executed. Preview a fresh plan.") }
        for item in plan.items {
            try ReadOnlyLocations.requireSourceMutation(URL(fileURLWithPath: item.output.outputPath))
            if plan.recipe.kind == "chapters" {
                let current = try await database.pool.read { db in try CatalogStore.chapters(db, fileID: item.output.fileID).filter { !$0.stale } }
                guard current == item.chapters else { throw Failure(text: "Chapter markers changed after preview. Preview again.") }
            }
            guard try Self.hash(URL(fileURLWithPath: item.output.sourcePath)) == item.sourceHash else { throw Failure(text: "A source changed after preview. Preview again.") }
            guard !FileManager.default.fileExists(atPath: item.output.outputPath) else { throw Failure(text: "An output name is now occupied. Preview again.") }
        }
        try await database.pool.write { db in try db.execute(sql: "UPDATE catalog_operations SET state='running' WHERE id=?", arguments: [id]) }
        var receipts: [Receipt] = []
        do {
            for item in plan.items {
                try Task.checkCancellation()
                let source = URL(fileURLWithPath: item.output.sourcePath)
                let destination = URL(fileURLWithPath: item.output.outputPath)
                let stage = destination.deletingLastPathComponent().appendingPathComponent(".FileIDExport-\(UUID().uuidString).part")
                try ReadOnlyLocations.requireWritable(stage)
                plan.stagePaths = (plan.stagePaths ?? []) + [stage.path]
                let checkpoint = String(decoding: try JSONEncoder().encode(plan), as: UTF8.self)
                try await database.pool.write { db in try db.execute(sql: "UPDATE catalog_operations SET plan_json=? WHERE id=?", arguments: [checkpoint, id]) }
                defer { try? FileManager.default.removeItem(at: stage) }
                if plan.recipe.kind == "photo" { try await VideoFrameWorker.exportPhoto(source: source, output: stage, recipe: plan.recipe) }
                else { try Self.exportChapters(item.chapters, format: plan.recipe.format).write(to: stage, options: .withoutOverwriting) }
                try Task.checkCancellation()
                guard try Self.hash(source) == item.sourceHash else { throw Failure(text: "A source changed during export. No output was published for that file.") }
                let hash = try Self.hash(stage)
                var receipt = Receipt(output: item.output, hash: hash)
                receipt.output.message = "Prepared output; review this path after an interrupted publication."
                receipts.append(receipt)
                try await persist(receipts, id: id, state: "running", database: database)
                try ReadOnlyLocations.requireWritable(destination)
                try FileManager.default.linkItem(at: stage, to: destination)
                receipts[receipts.count - 1].output.state = "completed"
                receipts[receipts.count - 1].output.message = "Original preserved."
                let outputSize = (try destination.resourceValues(forKeys: [.fileSizeKey])).fileSize ?? 0
                let modified = (try destination.resourceValues(forKeys: [.contentModificationDateKey])).contentModificationDate?.timeIntervalSince1970
                let recipeJSON = String(decoding: try JSONEncoder().encode(plan.recipe), as: UTF8.self)
                let published = receipts
                let outputKind = plan.recipe.kind == "photo" ? "image" : "doc"
                let derived: Int64 = try await database.pool.write { db in
                    try db.execute(sql: "INSERT INTO files(path_text,path_hash,size_bytes,scanned_at,modified_at,kind,extension) VALUES(?,?,?,0,?,?,?)", arguments: [destination.path, StablePathHash.hash(destination.path), outputSize, modified, outputKind, destination.pathExtension])
                    let derived = db.lastInsertedRowID
                    try db.execute(sql: "INSERT INTO catalog_assets(original_id,derived_id,role,recipe_json) VALUES(?,?,'export',?)", arguments: [item.output.fileID, derived, recipeJSON])
                    var recorded = published
                    recorded[recorded.count - 1].derivedID = derived
                    let inverse = String(decoding: try JSONEncoder().encode(recorded), as: UTF8.self)
                    try db.execute(sql: "UPDATE catalog_operations SET inverse_json=? WHERE id=?", arguments: [inverse, id])
                    return derived
                }
                receipts[receipts.count - 1].derivedID = derived
                try await persist(receipts, id: id, state: "running", database: database)
            }
            try await persist(receipts, id: id, state: "completed", database: database)
            return ToolResponse(requestID: request.requestID, message: "Export complete. Undo moves unchanged exports into the internal recovery folder.", operationID: id, outputs: receipts.map(\.output))
        } catch {
            try await persist(receipts, id: id, state: "failed", database: database)
            return ToolResponse(requestID: request.requestID, status: "error", message: error is CancellationError ? "Export cancelled. Completed outputs remain recoverable." : error.localizedDescription, operationID: id, outputs: receipts.map(\.output))
        }
    }

    private func persist(_ receipts: [Receipt], id: String, state: String, database: Database) async throws {
        let json = String(decoding: try JSONEncoder().encode(receipts), as: UTF8.self)
        try await database.pool.write { db in try db.execute(sql: "UPDATE catalog_operations SET inverse_json=?,state=? WHERE id=?", arguments: [json, state, id]) }
    }

    private func undo(_ request: ToolRequest, database: Database) async throws -> ToolResponse {
        guard let id = request.operationID else { throw Failure(text: "Select an export operation.") }
        let (_, stored, state) = try await load(id, database: database)
        guard ["completed", "failed"].contains(state) else { throw Failure(text: "This operation cannot be undone.") }
        var receipts = stored
        for receipt in receipts where receipt.output.state == "completed" {
            let output = URL(fileURLWithPath: receipt.output.outputPath)
            try ReadOnlyLocations.requireSourceMutation(output)
            if let recoveryPath = receipt.recoveryPath, !FileManager.default.fileExists(atPath: output.path), FileManager.default.fileExists(atPath: recoveryPath) { continue }
            guard try Self.hash(output) == receipt.hash else { throw Failure(text: "An export was edited. Undo leaves all remaining exports untouched.") }
        }
        let recovery = URL(fileURLWithPath: database.pool.path).deletingLastPathComponent()
        let directory = recovery.appendingPathComponent("ExportRecovery").appendingPathComponent(id)
        try ReadOnlyLocations.requireWritable(directory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        for index in receipts.indices where receipts[index].output.state == "completed" {
            let output = URL(fileURLWithPath: receipts[index].output.outputPath)
            try ReadOnlyLocations.requireSourceMutation(output)
            if let path = receipts[index].recoveryPath, !FileManager.default.fileExists(atPath: output.path), FileManager.default.fileExists(atPath: path), try Self.hash(URL(fileURLWithPath: path)) == receipts[index].hash {
                if let derived = receipts[index].derivedID { try await database.pool.write { db in try db.execute(sql: "DELETE FROM files WHERE id=?", arguments: [derived]) } }
                receipts[index].output.state = "undone"
                receipts[index].output.message = "Recoverable at \(path)"
                try await persist(receipts, id: id, state: state, database: database)
                continue
            }
            guard try Self.hash(output) == receipts[index].hash else { throw Failure(text: "An export was edited. Undo leaves it untouched.") }
            let target = directory.appendingPathComponent("\(index)-" + output.lastPathComponent)
            try ReadOnlyLocations.requireWritable(target)
            receipts[index].recoveryPath = target.path
            try await persist(receipts, id: id, state: state, database: database)
            try FileManager.default.moveItem(at: output, to: target)
            if let derived = receipts[index].derivedID {
                try await database.pool.write { db in try db.execute(sql: "DELETE FROM files WHERE id=?", arguments: [derived]) }
            }
            receipts[index].output.state = "undone"
            receipts[index].output.message = "Recoverable at \(target.path)"
            try await persist(receipts, id: id, state: state, database: database)
        }
        try await persist(receipts, id: id, state: "undone", database: database)
        return ToolResponse(requestID: request.requestID, message: "Exports moved to internal recovery storage. Originals are unchanged.", operationID: id, outputs: receipts.map(\.output))
    }

    static func hash(_ url: URL) throws -> String {
        let values = try url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])
        guard values.isRegularFile == true, values.isSymbolicLink != true else { throw Failure(text: "Choose a regular file, not a link or folder.") }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hash = SHA256()
        while let data = try handle.read(upToCount: 1024 * 1024), !data.isEmpty { try Task.checkCancellation(); hash.update(data: data) }
        return hash.finalize().map { String(format: "%02x", $0) }.joined()
    }

    static func validateImage(_ url: URL) throws {
        guard ["png", "jpg", "jpeg", "tif", "tiff", "heic"].contains(url.pathExtension.lowercased()),
              let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary), CGImageSourceGetCount(source) == 1 else { throw Failure(text: "Only supported single-image PNG, JPEG, TIFF, and HEIC inputs can be converted.") }
    }

    static func exportPhoto(source url: URL, output: URL, recipe: ToolRecipe) throws {
        try validateImage(url)
        guard let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [kCGImageSourceCreateThumbnailFromImageAlways: true, kCGImageSourceCreateThumbnailWithTransform: true, kCGImageSourceThumbnailMaxPixelSize: recipe.maxDimension] as CFDictionary) else { throw Failure(text: "The image could not be decoded.") }
        let space = image.colorSpace?.model == .rgb ? image.colorSpace! : CGColorSpace(name: CGColorSpace.sRGB)!
        guard let context = CGContext(data: nil, width: image.width, height: image.height, bitsPerComponent: 8, bytesPerRow: 0, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { throw Failure(text: "Not enough memory to convert this image.") }
        if recipe.format == "jpeg" { context.setFillColor(CGColor(gray: 1, alpha: 1)); context.fill(CGRect(x: 0, y: 0, width: image.width, height: image.height)) }
        context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
        let type = recipe.format == "jpeg" ? UTType.jpeg.identifier : recipe.format == "tiff" ? UTType.tiff.identifier : UTType.png.identifier
        guard let raster = context.makeImage(), let destination = CGImageDestinationCreateWithURL(output as CFURL, type as CFString, 1, nil) else { throw Failure(text: "The output codec is unavailable.") }
        CGImageDestinationAddImage(destination, raster, [kCGImageDestinationLossyCompressionQuality: 0.92, kCGImagePropertyOrientation: 1] as CFDictionary)
        guard CGImageDestinationFinalize(destination), let reopened = CGImageSourceCreateWithURL(output as CFURL, nil), CGImageSourceGetCount(reopened) == 1, let decoded = CGImageSourceCreateImageAtIndex(reopened, 0, nil), decoded.width == raster.width, decoded.height == raster.height else { throw Failure(text: "Output validation failed.") }
    }

    static func exportChapters(_ chapters: [CatalogChapter], format: String) throws -> Data {
        if format == "json" { let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys]; return try encoder.encode(chapters) }
        func stamp(_ seconds: Double) -> String {
            let milliseconds = Int64((min(seconds, 359999999) * 1000).rounded())
            return String(format: "%02lld:%02lld:%02lld.%03lld", milliseconds / 3600000, milliseconds / 60000 % 60, milliseconds / 1000 % 60, milliseconds % 1000)
        }
        var text = "WEBVTT\n\n"
        for (index, chapter) in chapters.enumerated() {
            let title = chapter.title.replacingOccurrences(of: "-->", with: "→").replacingOccurrences(of: "&", with: "&amp;").replacingOccurrences(of: "<", with: "&lt;").replacingOccurrences(of: ">", with: "&gt;").components(separatedBy: .newlines).joined(separator: " ")
            text += "\(index + 1)\n\(stamp(chapter.startSeconds)) --> \(stamp(max(chapter.endSeconds, chapter.startSeconds + 0.001)))\n\(title)\n\n"
        }
        return Data(text.utf8)
    }
}
