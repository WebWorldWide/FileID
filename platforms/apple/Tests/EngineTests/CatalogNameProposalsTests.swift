import Foundation
import Testing
import GRDB
import FileIDShared
@testable import FileIDEngine

@Suite("Smart name proposals", .serialized)
struct CatalogNameProposalsTests {
    @Test func confirmedTokensCollisionsAndValidation() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-names-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        for (name, byte) in [("IMG_1.jpg", UInt8(1)), ("IMG_2.jpg", 2), ("sam-baseball-hit.jpg", 3)] {
            try Data([byte]).write(to: root.appendingPathComponent(name))
        }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("test.sqlite"))
        let revision = "1:\(Double(10).bitPattern)"
        let blob = Data(Float(1).bitPattern.littleEndianBytes) + Data(count: 508)
        try await db.pool.write { d in
            for id in [1, 2] {
                try d.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,created_at,scanned_at,kind,extension,failed,vlm_proposed_name) VALUES(?,?,?,1,10.0,1688472000.0,0,'image','jpg',0,'Sam - Baseball Hit')",
                              arguments: [id, root.appendingPathComponent("IMG_\(id).jpg").path, id])
            }
            try d.execute(sql: "INSERT INTO persons(id,first_name,created_at) VALUES(1,'Sam',0),(2,'Alex',0)")
            for (face, file, person, model) in [(1, 1, 1, "m"), (2, 2, 2, "other"), (3, 1, 2, "m")] {
                try d.execute(sql: "INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision) VALUES(?,?,?,X'00','[0,0,1,1]',?,?,'p',?)",
                              arguments: [face, file, person, blob, model, revision])
            }
            try d.execute(sql: "UPDATE persons SET representative_face_id=id")
        }
        let naming = CatalogNaming(style: "short", maxLength: 40, includePeople: true)
        let response = await CatalogStore.handle(CatalogRequest(requestID: "n", action: "proposeNames", fileIDs: [1, 2, 1, 99], naming: naming), database: db)
        #expect(response.status == "ok")
        #expect(response.hits.map(\.text) == ["sam-baseball-hit_2.jpg", "alex-baseball-hit.jpg"])
        #expect(response.hits.first?.evidenceID == "proposedName")
        for bad in [CatalogRequest(requestID: "n", action: "proposeNames", fileIDs: [1], naming: CatalogNaming(style: "short", maxLength: 3)),
                    CatalogRequest(requestID: "n", action: "proposeNames", fileIDs: [], naming: naming),
                    CatalogRequest(requestID: "n", action: "proposeNames", fileIDs: [1])] {
            #expect(await CatalogStore.handle(bad, database: db).status == "error")
        }
    }
}

extension CatalogNameProposalsTests {
    @Test func companionsShareOneStemAndBumpTogether() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-companions-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("test.sqlite"))
        let files: [(Int64, String, String, String?)] = [
            (10, "DSC_1.ARW", "image", nil), (11, "DSC_1.JPG", "image", "beach sunset"), (12, "DSC_1.xmp", "other", nil),
            (13, "IMG_2.HEIC", "image", "dog park"), (14, "DSC_3.ARW", "image", "lone hill"),
        ]
        try await db.pool.write { d in
            for (id, name, kind, proposed) in files {
                let url = root.appendingPathComponent(name)
                try Data(name.utf8).write(to: url)
                try d.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,failed,vlm_proposed_name) VALUES(?,?,?,1,10.0,0,?,'x',0,?)",
                              arguments: [id, url.path, id, kind, proposed])
            }
        }
        try Data("m".utf8).write(to: root.appendingPathComponent("IMG_2.MOV"))
        try Data("x".utf8).write(to: root.appendingPathComponent("DSC_3.xmp"))
        try Data("t".utf8).write(to: root.appendingPathComponent("beach-sunset.ARW"))
        let naming = CatalogNaming(style: "short", maxLength: 40)
        let response = await CatalogStore.handle(CatalogRequest(requestID: "n", action: "proposeNames", fileIDs: [10, 13, 14, 12], naming: naming), database: db)
        #expect(response.status == "ok")
        #expect(response.hits.map(\.fileID) == [11, 10, 12, 13, 14])
        #expect(response.hits.map(\.text) == ["beach-sunset_2.JPG", "beach-sunset_2.ARW", "beach-sunset_2.xmp", "", "lone-hill.ARW"])
        #expect(response.hits.map { $0.evidenceID ?? "" } == ["proposedName|group:11", "proposedName|group:11", "proposedName|group:11",
                                                                "unscannedCompanion|group:13", "proposedName"])
    }
}

extension CatalogNameProposalsTests {
    @Test func learnedStyleIsRecordedAppliedAndReset() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-names-learn-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("test.sqlite"))
        try await db.pool.write { d in
            for (id, stored) in [(1, "beach sunset"), (2, "dog park"), (3, "red barn field"), (4, "kids playing soccer")] {
                let url = root.appendingPathComponent("Renamed_\(id).jpg")
                try Data([1]).write(to: url)
                try d.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,failed,vlm_proposed_name) VALUES(?,?,?,1,10.0,0,'image','jpg',0,?)",
                              arguments: [id, url.path, id, stored])
            }
        }
        let style = CatalogNaming(style: "short", maxLength: 40)
        func learn(_ edits: [CatalogNameEdit]?) async -> CatalogResponse {
            await CatalogStore.handle(CatalogRequest(requestID: "l", action: "learnNames", naming: style, nameEdits: edits), database: db)
        }
        var response = await learn([CatalogNameEdit(fileID: 1, proposed: "beach-sunset.jpg", applied: "Beach_Sunset.jpg"),
                                    CatalogNameEdit(fileID: 2, proposed: "dog-park.jpg", applied: "Dog_Park.jpg"),
                                    CatalogNameEdit(fileID: 99, proposed: "x.jpg", applied: "y.jpg")])
        #expect(response.status == "ok")
        #expect(response.hits.map(\.kind) == ["nameSamples"])
        #expect(response.hits.map(\.text) == ["2"])
        response = await learn([CatalogNameEdit(fileID: 3, proposed: "x.jpg", applied: "Red_Barn_Field.jpg")])
        #expect(response.hits.map(\.kind) == ["namePreference", "namePreference", "nameSamples"])
        #expect(response.hits.map { $0.evidenceID ?? "" } == ["separator", "case", ""])
        #expect(response.hits.map(\.text) == ["_", "title", "3"])
        let base = try await db.pool.read { d in
            try String.fetchOne(d, sql: "SELECT json_extract(before_json,'$.base') FROM catalog_corrections WHERE kind='nameStyle' AND file_id=3")
        }
        #expect(base == "red-barn-field")
        func propose(learn: Bool) async -> CatalogResponse {
            let naming = CatalogNaming(style: "short", maxLength: 40, learn: learn)
            return await CatalogStore.handle(CatalogRequest(requestID: "p", action: "proposeNames", fileIDs: [4], naming: naming), database: db)
        }
        var proposal = await propose(learn: false)
        #expect(proposal.hits.map(\.text) == ["kids-playing-soccer.jpg"])
        #expect(proposal.hits.first?.evidenceID == "proposedName")
        proposal = await propose(learn: true)
        #expect(proposal.hits.map(\.text) == ["Kids_Playing_Soccer.jpg"])
        #expect(proposal.hits.first?.evidenceID == "proposedName|learned:separator,case")
        #expect(await learn([CatalogNameEdit(fileID: 1, proposed: " ", applied: "a.jpg")]).status == "error")
        #expect(await learn(nil).status == "error")
        let reset = await CatalogStore.handle(CatalogRequest(requestID: "r", action: "resetNamePreferences"), database: db)
        #expect(reset.hits.map(\.text) == ["0"])
        proposal = await propose(learn: true)
        #expect(proposal.hits.map(\.text) == ["kids-playing-soccer.jpg"])
    }

    @Test func undoneRenamesForgetTheirNewestMatchingSample() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-names-unlearn-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("test.sqlite"))
        try await db.pool.write { d in
            for id in 1...2 {
                try d.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,failed,vlm_proposed_name) VALUES(?,?,?,1,10.0,0,'image','jpg',0,'beach sunset')",
                              arguments: [id, "/n/Renamed_\(id).jpg", id])
            }
        }
        func send(_ action: String, _ edits: [CatalogNameEdit]?) async -> CatalogResponse {
            await CatalogStore.handle(CatalogRequest(requestID: "l", action: action, naming: CatalogNaming(style: "short", maxLength: 40), nameEdits: edits), database: db)
        }
        for applied in ["Beach_Sunset.jpg", "Beach_Sunset.jpg", "Beach Sunset.jpg"] {
            _ = await send("learnNames", [CatalogNameEdit(fileID: 1, proposed: "beach-sunset.jpg", applied: applied)])
        }
        _ = await send("learnNames", [CatalogNameEdit(fileID: 2, proposed: "beach-sunset.jpg", applied: "Beach_Sunset.jpg")])
        let newest = try await db.pool.read { d in
            try Int64.fetchOne(d, sql: "SELECT MAX(rowid) FROM catalog_corrections WHERE file_id=1 AND json_extract(after_json,'$.applied')='Beach_Sunset.jpg'")
        }
        let response = await send("unlearnNames", [CatalogNameEdit(fileID: 1, proposed: "other.jpg", applied: "Beach_Sunset.jpg"),
                                                   CatalogNameEdit(fileID: 1, proposed: "x.jpg", applied: "Never_Applied.jpg"),
                                                   CatalogNameEdit(fileID: 99, proposed: "x.jpg", applied: "Beach_Sunset.jpg")])
        #expect(response.status == "ok")
        #expect(response.hits.last?.kind == "nameSamples")
        #expect(response.hits.last?.text == "3")
        let left = try await db.pool.read { d in
            try Row.fetchAll(d, sql: "SELECT rowid,file_id,json_extract(after_json,'$.applied') FROM catalog_corrections WHERE kind='nameStyle' ORDER BY rowid")
                .map { (rowid: $0[0] as Int64, file: $0[1] as Int64, applied: $0[2] as String) }
        }
        #expect(!left.contains { $0.rowid == newest })
        #expect(left.map(\.file) == [1, 1, 2])
        #expect(left.map(\.applied) == ["Beach_Sunset.jpg", "Beach Sunset.jpg", "Beach_Sunset.jpg"])
        #expect(await send("unlearnNames", []).status == "error")
        #expect(await send("unlearnNames", nil).status == "error")
    }
}

private extension UInt32 {
    var littleEndianBytes: [UInt8] { withUnsafeBytes(of: littleEndian) { Array($0) } }
}
