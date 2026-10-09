import Foundation
import Testing
import GRDB
import ImageIO
import UniformTypeIdentifiers
import FileIDShared
@testable import FileIDEngine

@Suite("Safe media exports")
struct MediaToolsTests {
    func fixture() async throws -> (URL, FileIDEngine.Database, URL) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let source = root.appendingPathComponent("Portrait.tiff")
        let context = try #require(CGContext(data: nil, width: 16, height: 8, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        context.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1)); context.fill(CGRect(x: 0, y: 0, width: 16, height: 8))
        let image = try #require(context.makeImage())
        let destination = try #require(CGImageDestinationCreateWithURL(source as CFURL, UTType.tiff.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, [kCGImagePropertyOrientation: 6, kCGImagePropertyGPSDictionary: [kCGImagePropertyGPSLatitude: 45.0, kCGImagePropertyGPSLatitudeRef: "N"], kCGImagePropertyExifDictionary: [kCGImagePropertyExifUserComment: "Private metadata"]] as CFDictionary)
        #expect(CGImageDestinationFinalize(destination))
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,?,1,100,0,'image','tiff')", arguments: [source.path]) }
        return (root, database, source)
    }
    @Test func enlargementIsOptInAndPreservesOrientationOriginalAndUndo() async throws {
        let (root, db, source) = try await fixture()
        defer { try? FileManager.default.removeItem(at: root) }
        let original = try MediaTools.hash(source)
        let tools = MediaTools()
        for flag: Bool? in [nil, false, true] {
            let recipe = ToolRecipe(kind: "photo", format: "png", maxDimension: 64, allowUpscale: flag)
            let preview = await tools.handle(ToolRequest(requestID: "p", action: "preview", fileIDs: [1], destination: root.path, recipe: recipe), database: db)
            #expect(preview.status == "ok")
            let result = await tools.handle(ToolRequest(requestID: "e", action: "execute", destination: root.path, operationID: preview.operationID), database: db)
            #expect(result.status == "ok", "\(result.message)")
            let output = URL(fileURLWithPath: try #require(result.outputs.first).outputPath)
            let raster = try #require(CGImageSourceCreateWithURL(output as CFURL, nil))
            let image = try #require(CGImageSourceCreateImageAtIndex(raster, 0, nil))
            #expect(image.width == (flag == true ? 32 : 8))
            #expect(image.height == (flag == true ? 64 : 16))
            #expect(try MediaTools.hash(source) == original)
            let undone = await tools.handle(ToolRequest(requestID: "u", action: "undo", destination: root.path, operationID: preview.operationID), database: db)
            #expect(undone.status == "ok")
            #expect(!FileManager.default.fileExists(atPath: output.path))
            let history = await tools.handle(ToolRequest(requestID: "h", action: "history"), database: db)
            #expect(history.operationID == preview.operationID)
            #expect(history.outputs.first?.state == "undone")
            #expect(history.outputs.first?.message.contains("Recoverable at") == true)
        }
    }

    @Test func enlargementKeepsAspectBoundsAndRejectsOtherTools() throws {
        let recipe = ToolRecipe(kind: "photo", format: "jpeg", maxDimension: 64, allowUpscale: true)
        let wide = try MediaTools.photoRasterSize(width: 1024, height: 1, recipe: recipe)
        #expect(wide.width == 64 && wide.height == 1)
        let tall = try MediaTools.photoRasterSize(width: 1, height: 32, recipe: recipe)
        #expect(tall.width == 2 && tall.height == 64)
        #expect(!MediaTools.supports(ToolRecipe(kind: "photo", format: "png", maxDimension: 8193, allowUpscale: true)))
        #expect(!MediaTools.supports(ToolRecipe(kind: "video", format: "mp4", maxDimension: 1920, allowUpscale: true)))
        #expect(!MediaTools.supports(ToolRecipe(kind: "chapters", format: "vtt", allowUpscale: true)))
        #expect(!MediaTools.supports(ToolRecipe(kind: "photo", format: "png", videoAspectRatio: "9:16")))
        #expect(!MediaTools.supports(ToolRecipe(kind: "video", format: "mp4", maxDimension: 1920, videoAspectRatio: "10:3")))
        #expect(MediaTools.supports(ToolRecipe(kind: "video", format: "mp4", maxDimension: 1920, videoAspectRatio: "9:16")))
        #expect(throws: MediaTools.Failure.self) {
            try MediaTools.photoRasterSize(width: 0, height: 16, recipe: recipe)
        }
    }

    @Test func conversionAppliesOrientationStripsMetadataAndSurvivesUndoReopen() async throws {
        let (root, db, source) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let original = try MediaTools.hash(source)
        let tools = MediaTools()
        let preview = await tools.handle(ToolRequest(requestID: "p", action: "preview", fileIDs: [1], destination: root.path, recipe: ToolRecipe(kind: "photo", format: "png", maxDimension: 16)), database: db)
        #expect(preview.status == "ok")
        let id = try #require(preview.operationID)
        let exported = await tools.handle(ToolRequest(requestID: "e", action: "execute", destination: root.path, operationID: id), database: db)
        #expect(exported.status == "ok", "\(exported.message)")
        let output = URL(fileURLWithPath: try #require(exported.outputs.first).outputPath)
        let rasterSource = try #require(CGImageSourceCreateWithURL(output as CFURL, nil))
        let image = try #require(CGImageSourceCreateImageAtIndex(rasterSource, 0, nil))
        #expect(image.width == 8 && image.height == 16)
        let properties = try #require(CGImageSourceCopyPropertiesAtIndex(rasterSource, 0, nil) as? [String: Any])
        #expect(properties[kCGImagePropertyGPSDictionary as String] == nil)
        let exif = properties[kCGImagePropertyExifDictionary as String] as? [String: Any]
        #expect(exif?[kCGImagePropertyExifUserComment as String] == nil)
        #expect(try MediaTools.hash(source) == original)
        #expect(try await db.pool.read { db in try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM catalog_assets") } == 1)
        try db.pool.close()
        let reopened = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let history = await tools.handle(ToolRequest(requestID: "h", action: "history"), database: reopened)
        #expect(history.operationID == id)
        let undone = await tools.handle(ToolRequest(requestID: "u", action: "undo", destination: root.path, operationID: id), database: reopened)
        #expect(undone.status == "ok")
        #expect(undone.outputs.first?.state == "undone")
        #expect(!FileManager.default.fileExists(atPath: output.path))
        #expect(try MediaTools.hash(source) == original)
        #expect(try await reopened.pool.read { db in try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM catalog_assets") } == 0)
    }
    @Test func changedSourceAndOccupiedOutputAreRejectedWithoutOverwriting() async throws {
        let (root, db, source) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let tools = MediaTools()
        let request = ToolRequest(requestID: "p", action: "preview", fileIDs: [1], destination: root.path, recipe: ToolRecipe(kind: "photo", format: "jpeg"))
        let preview = await tools.handle(request, database: db)
        let path = URL(fileURLWithPath: try #require(preview.outputs.first).outputPath)
        try Data("Existing output".utf8).write(to: path)
        let occupied = await tools.handle(ToolRequest(requestID: "e", action: "execute", destination: root.path, operationID: preview.operationID), database: db)
        #expect(occupied.status == "error")
        #expect(try String(contentsOf: path, encoding: .utf8) == "Existing output")
        try FileManager.default.removeItem(at: path)
        try Data("Replaced source".utf8).write(to: source)
        let changed = await tools.handle(ToolRequest(requestID: "changed", action: "execute", destination: root.path, operationID: preview.operationID), database: db)
        #expect(changed.status == "error")
        #expect(!FileManager.default.fileExists(atPath: path.path))
    }
    @Test func chapterExportsAreEvidenceSnapshotsAndProtectedPathsFailBeforeWrite() async throws {
        let (root, db, _) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let chapter = CatalogChapter(id: "c", fileID: 1, startSeconds: 1.234, endSeconds: 1.234, title: "Gift <opening> -->\nGrandma", summary: "", sourceRevision: "", modelVersion: "user", confidence: 1, userEdited: true, stale: false)
        #expect(await CatalogStore.handle(CatalogRequest(requestID: "c", action: "saveChapter", chapter: chapter), database: db).status == "ok")
        let tools = MediaTools()
        let preview = await tools.handle(ToolRequest(requestID: "p", action: "preview", fileIDs: [1], destination: root.path, recipe: ToolRecipe(kind: "chapters", format: "vtt")), database: db)
        let exported = await tools.handle(ToolRequest(requestID: "e", action: "execute", destination: root.path, operationID: preview.operationID), database: db)
        #expect(exported.status == "ok")
        let text = try String(contentsOfFile: try #require(exported.outputs.first).outputPath, encoding: .utf8)
        #expect(text.contains("00:00:01.234 --> 00:00:01.235"))
        #expect(text.contains("Gift &lt;opening&gt; → Grandma"))
        let protected = await tools.handle(ToolRequest(requestID: "protected", action: "preview", fileIDs: [1], destination: "/Volumes/Adlon", recipe: ToolRecipe(kind: "chapters", format: "json")), database: db)
        #expect(protected.status == "error")
        let editedPreview = await tools.handle(ToolRequest(requestID: "p2", action: "preview", fileIDs: [1], destination: root.path, recipe: ToolRecipe(kind: "chapters", format: "json")), database: db)
        var edited = chapter; edited.title = "Changed marker"
        _ = await CatalogStore.handle(CatalogRequest(requestID: "edit", action: "saveChapter", chapter: edited), database: db)
        #expect(await tools.handle(ToolRequest(requestID: "e2", action: "execute", destination: root.path, operationID: editedPreview.operationID), database: db).status == "error")
    }
    @Test func undoProtectsEditedExportsAndInterruptedOperationsRemainInspectable() async throws {
        let (root, db, _) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let tools = MediaTools()
        let preview = await tools.handle(ToolRequest(requestID: "p", action: "preview", fileIDs: [1], destination: root.path, recipe: ToolRecipe(kind: "photo", format: "png")), database: db)
        let exported = await tools.handle(ToolRequest(requestID: "e", action: "execute", destination: root.path, operationID: preview.operationID), database: db)
        let output = URL(fileURLWithPath: try #require(exported.outputs.first).outputPath)
        try Data("Edited export".utf8).write(to: output)
        #expect(await tools.handle(ToolRequest(requestID: "u", action: "undo", destination: root.path, operationID: preview.operationID), database: db).status == "error")
        #expect(try String(contentsOf: output, encoding: .utf8) == "Edited export")
        let id = try #require(preview.operationID)
        try await db.pool.write { db in try db.execute(sql: "UPDATE catalog_operations SET state='running' WHERE id=?", arguments: [id]) }
        await tools.recover(database: db)
        #expect(try await db.pool.read { db in try String.fetchOne(db, sql: "SELECT state FROM catalog_operations WHERE id=?", arguments: [id]) } == "failed")
    }
    @Test func recoveryCleansOnlyRegisteredStagingAndPreservesSourcesAndAliases() async throws {
        let (root, db, source) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let stage = root.appendingPathComponent(".FileIDExport-\(UUID().uuidString).part")
        let unrelated = root.appendingPathComponent(".FileIDExport-user.part")
        let alias = root.appendingPathComponent(".FileIDExport-\(UUID().uuidString).part")
        try Data("Partial output".utf8).write(to: stage)
        try Data("Unrelated".utf8).write(to: unrelated)
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: source)
        let original = try MediaTools.hash(source)
        let item = MediaTools.Item(output: ToolOutput(fileID: 1, sourcePath: source.path, outputPath: root.appendingPathComponent("Export.png").path), sourceHash: original, chapters: [])
        let plan = MediaTools.Plan(recipe: ToolRecipe(kind: "photo", format: "png"), items: [item], stagePaths: [stage.path, unrelated.path, alias.path])
        let json = String(decoding: try JSONEncoder().encode(plan), as: UTF8.self)
        try await db.pool.write { db in try db.execute(sql: "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES('interrupted',?,'[]','running',0)", arguments: [json]) }
        await MediaTools().recover(database: db)
        let deadline = ContinuousClock.now.advanced(by: .seconds(6))
        while FileManager.default.fileExists(atPath: stage.path), ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(50)) }
        #expect(!FileManager.default.fileExists(atPath: stage.path))
        #expect(FileManager.default.fileExists(atPath: unrelated.path))
        #expect(FileManager.default.fileExists(atPath: alias.path))
        #expect(try MediaTools.hash(source) == original)
    }

}
