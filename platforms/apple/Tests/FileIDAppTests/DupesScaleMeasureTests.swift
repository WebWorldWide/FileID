import Foundation
import Testing
@testable import FileID

@Suite("Duplicates on a real catalog", .serialized)
struct DupesScaleMeasureTests {
    @Test("exact duplicates timing on the catalog named by FILEID_W5_CATALOG")
    func exactTiming() async throws {
        guard let path = ProcessInfo.processInfo.environment["FILEID_W5_CATALOG"] else { return }
        let store = ReadStore(dbURL: URL(fileURLWithPath: path))
        store.openIfPossible()
        defer { store.close() }
        let t0 = Date()
        let load = store.exactDuplicateCandidates()
        print("W5 candidates: \(load.files.count) of \(load.candidateCount) partial=\(load.partial) in \(Date().timeIntervalSince(t0)) s")
        let t1 = Date()
        let snap = await store.exactDuplicateSnapshotAsync()
        print("W5 snapshot: groups=\(snap.groups.count) partial=\(snap.partial) skipped=\(snap.skipped) capped=\(snap.capped) in \(Date().timeIntervalSince(t1)) s")
        let t2 = Date()
        let stored = store.exactStoredGroups()
        print("W5 stored groups: shown=\(stored.groups.count) of \(stored.totalGroups) extra=\(stored.totalExtraCopies) bytes=\(stored.totalBytes) in \(Date().timeIntervalSince(t2)) s")
        let t3 = Date()
        let final = await ExactDuplicateVerifier.verifyStored(stored.groups) { update in
            print("W5 verify progress \(update.done)/\(update.total) groups=\(update.groups.count) at \(Date().timeIntervalSince(t3)) s")
        }
        print("W5 verified: groups=\(final.groups.count) unverified=\(final.unverified.count) skipped=\(final.skipped) in \(Date().timeIntervalSince(t3)) s")
    }
}
