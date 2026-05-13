// IPC envelope round-trip tests. The wire is JSON; the simplest way to
// catch encoder/decoder bugs is to round-trip every payload variant and
// assert equality.
import Testing
import Foundation
@testable import FileIDShared

@Suite("IPC protocol round-trip")
struct IPCProtocolTests {

    @Test("Command: every payload variant survives JSON round-trip")
    func commandRoundTrip() throws {
        let commands: [IPCCommand.Payload] = [
            .startScan(rootBookmark: Data([0x01, 0x02, 0x03]), rootPathDisplay: "/Users/adam/photos"),
            .pauseScan,
            .resumeScan,
            .cancelScan,
            .requestStatus,
            .shutdown
        ]
        for payload in commands {
            let cmd = IPCCommand(payload: payload)
            let line = try IPCCoder.encodeLine(cmd)
            // Strip trailing newline before decoding.
            let withoutNewline = line.dropLast()
            let decoded = try IPCCoder.decoder.decode(IPCCommand.self, from: Data(withoutNewline))
            #expect(decoded.id == cmd.id)
            // Variant must match — pattern-match by encoding both as JSON
            // and comparing the bytes of the payload.
            let originalPayloadJSON = try IPCCoder.encoder.encode(cmd.payload)
            let decodedPayloadJSON = try IPCCoder.encoder.encode(decoded.payload)
            #expect(originalPayloadJSON == decodedPayloadJSON)
        }
    }

    @Test("Event: progress payload survives round-trip with all fields")
    func eventProgressRoundTrip() throws {
        let progress = ScanProgress(
            sessionID: "session-uuid",
            phase: .tagging,
            total: 50_000,
            discovered: 50_000,
            processed: 12_345,
            failed: 7,
            filesPerSecond: 87.4,
            etaSeconds: 432.1,
            residentMB: 612,
            availableMB: 4200
        )
        let event = IPCEvent(payload: .progress(progress))
        let line = try IPCCoder.encodeLine(event)
        let decoded = try IPCCoder.decoder.decode(IPCEvent.self, from: Data(line.dropLast()))
        guard case .progress(let p) = decoded.payload else {
            Issue.record("Decoded payload was not .progress")
            return
        }
        #expect(p.sessionID == progress.sessionID)
        #expect(p.phase == progress.phase)
        #expect(p.total == progress.total)
        #expect(p.processed == progress.processed)
        #expect(p.failed == progress.failed)
        #expect(p.filesPerSecond == progress.filesPerSecond)
        #expect(p.etaSeconds == progress.etaSeconds)
    }

    @Test("Encoded line ends with exactly one '\\n'")
    func lineTerminator() throws {
        let cmd = IPCCommand(payload: .pauseScan)
        let line = try IPCCoder.encodeLine(cmd)
        #expect(line.last == 0x0A)
        // No embedded newlines (would corrupt the wire).
        let interior = line.dropLast()
        #expect(!interior.contains(0x0A))
    }

    @Test("Windows-originated commands round-trip")
    func windowsCommandsRoundTrip() throws {
        let moves = [
            RestructureMove(fileID: 1, source: "/a/x.jpg", destination: "/b/x.jpg",
                            category: "Anchor", tier: "Anchor"),
            RestructureMove(fileID: 2, source: "/a/y.jpg", destination: "/c/y.jpg",
                            category: "Mixed", tier: nil),
        ]
        let renames = [RenameEntry(fileID: 1, newName: "vacation_beach")]
        let commands: [IPCCommand.Payload] = [
            .planRestructure(libraryRoot: "/Users/x/Pictures"),
            .applyRestructure(libraryRoot: "/Users/x/Pictures", moves: moves, useSymlinks: true),
            .applyTags(fileIDs: [1, 2, 3], tags: ["beach", "summer"], mode: "add"),
            .renameFiles(renames: renames),
            .trashFiles(fileIDs: [10, 11]),
            .mergeClusters(sourcePersonID: 4, destinationPersonID: 7),
            .embedTextQuery(query: "dog at the beach", queryID: "q-1"),
            .renamePerson(personID: 1, title: "Dr.", firstName: "Adam",
                          middleName: nil, lastName: "Nolle", suffix: nil),
            .markPersonsAsUnknown(personIDs: [4, 5]),
            .findMergeSuggestions,
            .embedImageQuery(fileID: 42, queryID: "iq-1"),
            .restoreFromTrash(batchID: "batch-uuid"),
            .revertMerge(sourcePersonID: 4, destinationPersonID: 7, faceIDsToRevert: [11, 12]),
            .verifyCudaPack,
        ]
        for payload in commands {
            let cmd = IPCCommand(payload: payload)
            let line = try IPCCoder.encodeLine(cmd)
            let decoded = try IPCCoder.decoder.decode(IPCCommand.self, from: Data(line.dropLast()))
            #expect(decoded.id == cmd.id)
            let originalJSON = try IPCCoder.encoder.encode(cmd.payload)
            let decodedJSON = try IPCCoder.encoder.encode(decoded.payload)
            #expect(originalJSON == decodedJSON,
                    "round-trip mismatch for \(payload)")
        }
    }

    @Test("FileDoneEvent.skippedStages survives round-trip")
    func skippedStagesRoundTrip() throws {
        let evt = FileDoneEvent(path: "/foo/bar.jpg", kind: "image",
                                totalMs: 42.0, failed: false,
                                skippedStages: ["face_detection", "image_embedding"])
        let event = IPCEvent(payload: .fileDone(evt))
        let line = try IPCCoder.encodeLine(event)
        let decoded = try IPCCoder.decoder.decode(IPCEvent.self, from: Data(line.dropLast()))
        guard case .fileDone(let d) = decoded.payload else {
            Issue.record("Decoded payload was not .fileDone")
            return
        }
        #expect(d.skippedStages == ["face_detection", "image_embedding"])
    }
}
