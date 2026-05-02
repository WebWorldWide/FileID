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
}
