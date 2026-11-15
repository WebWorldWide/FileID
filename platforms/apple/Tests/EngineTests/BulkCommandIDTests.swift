// Schema 1.28.0: every bulk result answering a command carries that command's id.
import Testing
import Foundation
@testable import FileIDEngine
import FileIDShared

@Suite("Bulk result command id", .serialized)
struct BulkCommandIDTests {

    private func results(_ cap: WireCapture) async throws -> [BulkActionResult] {
        await cap.finish()
        try await Task.sleep(nanoseconds: 30_000_000)
        return try cap.bytes().split(separator: 10).compactMap { line in
            if case .bulkActionResult(let result) = try IPCCoder.decoder.decode(IPCEvent.self, from: Data(line)).payload { return result }
            return nil
        }
    }

    @Test("a refusal for an unavailable database carries the command id")
    func rejectionCarriesID() async throws {
        let cap = WireCapture()
        let cmd = IPCCommand(id: "11111111-2222-3333-4444-555555555555",
                             payload: .mergeClusters(sourcePersonID: 1, destinationPersonID: 2))
        await FileIDEngineMain.dispatch(cmd, coordinator: ScanCoordinator(), sink: cap.sink, database: nil)
        let all = try await results(cap)
        #expect(all.count == 1)
        #expect(all.first?.action == "mergeClusters")
        #expect(all.first?.commandID == cmd.id)
    }

    @Test("a result from the database carries the command id")
    func successPathCarriesID() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try Database(at: root.appendingPathComponent("catalog.sqlite"))
        let cap = WireCapture()
        let cmd = IPCCommand(id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                             payload: .applyTags(fileIDs: [], tags: [], mode: "replace"))
        await FileIDEngineMain.dispatch(cmd, coordinator: ScanCoordinator(), sink: cap.sink, database: db)
        let all = try await results(cap)
        #expect(all.count == 1)
        #expect(all.first?.commandID == cmd.id)
    }

    @Test("an encoded result carries commandID and a result without it still decodes")
    func codingRoundTrip() throws {
        var result = BulkActionResult(action: "mergeClusters", succeeded: 1, failed: 0, messages: [])
        let plain = String(decoding: try IPCCoder.encodeLine(IPCEvent(payload: .bulkActionResult(result))), as: UTF8.self)
        #expect(!plain.contains("commandID"))
        result.commandID = "abc"
        let line = try IPCCoder.encodeLine(IPCEvent(payload: .bulkActionResult(result)))
        #expect(String(decoding: line, as: UTF8.self).contains("\"commandID\":\"abc\""))
        guard case .bulkActionResult(let decoded) = try IPCCoder.decoder.decode(IPCEvent.self, from: line).payload else {
            Issue.record("not a bulk result"); return
        }
        #expect(decoded.commandID == "abc")
        guard case .bulkActionResult(let old) = try IPCCoder.decoder.decode(IPCEvent.self, from: Data(plain.utf8)).payload else {
            Issue.record("not a bulk result"); return
        }
        #expect(old.commandID == nil)
    }
}
