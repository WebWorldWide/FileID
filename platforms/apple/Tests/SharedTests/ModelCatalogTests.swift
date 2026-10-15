import Foundation
import Testing
@testable import FileIDShared

@Suite("Offline model catalog")
struct ModelCatalogTests {
    private var canonical: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("shared/Sources/FileIDShared/Resources/model-catalog.json")
    }
    @Test func bundledCatalogPinsLicensesAndWireIDs() throws {
        let entries = try MLXModelCatalog.load(data: Data(contentsOf: canonical))
        #expect(entries == MLXModelCatalog.entries)
        #expect(entries.count >= 200)
        #expect(entries.contains { $0.role == .text })
        #expect(entries.contains { $0.role == .vision })
        for entry in entries {
            let kind = try #require(AIModelKind(rawValue: entry.id))
            #expect(kind.sourceRepo == entry.repo)
            #expect(kind.supportsVision == (entry.role == .vision))
            #expect(try JSONDecoder().decode(AIModelKind.self, from: JSONEncoder().encode(kind)) == kind)
            let pin = try #require(ModelManifest.vlmPin(forRepo: entry.repo))
            #expect(pin.revision.count == 40)
            #expect(pin.revision != "main")
        }
        #expect(AIModelKind(rawValue: "hf:attacker/arbitrary") == nil)
        #expect(ModelManifest.vlmPin(forRepo: "attacker/arbitrary") == nil)
        #expect(AIModelKind.migrated(rawValue: "qwen2_vl_7b") == .qwen2VL7B)
        #expect(AIModelKind.visionModels.allSatisfy { $0.supportsVision })
        #expect(AIModelKind.favorites.count == 8)
    }
    @Test(arguments: ["license", "revision", "weights", "duplicate", "lineage", "architecture", "quantization"])
    func tamperedCatalogFailsClosed(fault: String) throws {
        let data = try Data(contentsOf: canonical)
        var document = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
        var entries = try #require(document["entries"] as? [[String: Any]])
        switch fault {
        case "license": entries[0]["license"] = "cc-by-nc-4.0"
        case "revision": entries[0]["revision"] = "main"
        case "weights": entries[0]["weightBytes"] = 1
        case "duplicate": entries.append(entries[0])
        case "architecture": entries[0]["architecture"] = "unsupported_remote_model"
        case "quantization": entries[0]["quantization"] = ["bits":7,"groupSize":64,"mode":"affine"]
        default: entries[0]["bases"] = [["repo":"upstream/model", "revision":String(repeating:"a",count:40), "license":"llama3"]]
        }
        document["entries"] = entries
        let invalid = try JSONSerialization.data(withJSONObject: document)
        #expect(throws: (any Error).self) { try MLXModelCatalog.load(data: invalid) }
    }
    @Test func chatModelSelectionRemainsOptionalOnTheWire() throws {
        let old = Data(#"{"requestID":"r","conversationID":"c","action":"send","text":"gift"}"#.utf8)
        #expect(try JSONDecoder().decode(ChatRequest.self, from: old).modelKind == nil)
        let kind = try #require(AIModelKind.allCases.first { !$0.supportsVision })
        let request = ChatRequest(requestID: "r", conversationID: "c", action: "send", text: "gift", useModel: true, modelKind: kind.rawValue)
        #expect(try JSONDecoder().decode(ChatRequest.self, from: JSONEncoder().encode(request)) == request)
    }
}
