// F-C3-031 + F-C3-039 regression for the engine's writer connection.
//
// 031: a cancelled scan still has to record its final scan_sessions status.
// GRDB 7's async pool.write throws CancellationError on a cancelled task, so
// the terminal UPDATE was swallowed and the row stayed 'running' → mislabeled
// 'crashed' on next launch. Database.writeUncancellable must land the write
// regardless of the caller's cancellation.
//
// 039: the writer must issue PRAGMA cache_spill = 0 (Windows parity) so dirty
// pages can't spill to a temp file mid-transaction under memory pressure.
import Testing
import Foundation
import GRDB
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

@Suite("Database writer guards (F-C3-031/039)")
struct DatabaseWriterGuardTests {

    private func makeDB() throws -> (Database, URL) {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDWriterGuard-\(UUID().uuidString)")
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        return (db, tmp)
    }

    @Test("writeUncancellable lands a terminal write issued from a cancelled task")
    func uncancellableWriteSurvivesCancellation() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }

        let sessionID = UUID().uuidString
        try await db.pool.write { d in
            try d.execute(sql: """
                INSERT INTO scan_sessions (id, root_path, started_at, status)
                VALUES (?, '/tmp/library', ?, 'running')
                """, arguments: [sessionID, Date().timeIntervalSince1970])
        }

        // A task that waits until it is cancelled, then proves that a plain
        // pool.write is swallowed while the shielded write still commits.
        let probe = Task { () -> (plainThrew: Bool, shieldedOK: Bool) in
            while !Task.isCancelled { try? await Task.sleep(nanoseconds: 1_000_000) }

            var plainThrew = false
            do {
                try await db.pool.write { d in
                    try d.execute(sql: "UPDATE scan_sessions SET status = 'plainwrite' WHERE id = ?",
                                  arguments: [sessionID])
                }
            } catch is CancellationError {
                plainThrew = true
            } catch {
                plainThrew = false
            }

            var shieldedOK = false
            do {
                try await db.writeUncancellable { d in
                    try d.execute(sql: "UPDATE scan_sessions SET status = 'cancelled', completed_at = ? WHERE id = ?",
                                  arguments: [Date().timeIntervalSince1970, sessionID])
                }
                shieldedOK = true
            } catch {
                shieldedOK = false
            }
            return (plainThrew, shieldedOK)
        }
        probe.cancel()
        let result = await probe.value

        #expect(result.plainThrew,
                "GRDB 7's pool.write must throw on a cancelled task — the bug this guards")
        #expect(result.shieldedOK, "writeUncancellable must not throw under cancellation")

        let status = try await db.pool.read { d in
            try String.fetchOne(d, sql: "SELECT status FROM scan_sessions WHERE id = ?",
                                arguments: [sessionID])
        }
        #expect(status == "cancelled",
                "the shielded terminal write must persist (not be left 'running')")
    }

    @Test("prepareDatabase applies the WAL + durability pragmas on pooled connections")
    func prepareDatabasePragmas() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }
        // These run in GRDB's prepareDatabase hook for EVERY pooled connection,
        // so a fresh reader must report them — proving the hook fired.
        //
        // `cache_spill = 0` is also issued there, but it CANNOT be asserted via
        // `PRAGMA cache_spill`: that query returns the cache_size-derived page
        // threshold (e.g. 15857 for a 64 MB cache), never the disabled-boolean
        // 0 — so the prior `value == 0` assertion could never pass on any host.
        // The disable is verified by the SQL in Database.swift, not round-tripped.
        let (journal, sync, tempStore) = try await db.pool.read { d in
            (try String.fetchOne(d, sql: "PRAGMA journal_mode"),
             try Int.fetchOne(d, sql: "PRAGMA synchronous"),
             try Int.fetchOne(d, sql: "PRAGMA temp_store"))
        }
        #expect(journal == "wal")
        #expect(sync == 1)       // NORMAL
        #expect(tempStore == 2)  // MEMORY
    }
}
