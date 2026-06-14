// C3-DA regression suite — Deep Analyze fixes from audit-2026-06-10.
//
// Covers: parseFaceComparison negation (F-C3-022), 50 MP decode cap
// (F-C3-044), recursive HF tree listing (F-C3-043), escaped folder-scope
// LIKE (F-C3-027), COALESCE persist (F-C3-044), and the queued-cancel /
// exactly-one-terminal-complete run() invariant (F-C3-025, F-C3-028).
//
// The single-flight load coalescing (F-C3-023) and in-flight-download
// cancel (F-C3-024) need a live VLM download + MLX load and so are verified
// on-device, not here — see the structured result for the skip rationale.
import Testing
import Foundation
import GRDB
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database
import FileIDShared

@Suite("Deep Analyze pure-logic fixes (C3-DA)")
struct DeepAnalyzePureLogicTests {

    // F-C3-022 — a negated verdict must parse as DIFFERENT, never an
    // affirmative SAME at the defaulted 0.80 (> 0.75 auto-merge threshold).
    @Test("parseFaceComparison: negated 'same' is DIFFERENT, never auto-merges")
    func negatedVerdictNeverMerges() {
        // R-12: only LOOSE free-text negations parse as DIFFERENT here. A reply
        // with an explicit "VERDICT: SAME" line is authoritative and is covered
        // in `affirmativeVerdictPreserved` — the negated-same override must not
        // reach the explicit-verdict branch.
        let negatives = [
            "These are not the same person.",
            "not the same",
            "They are not the same.",
            "No — isn't the same person.",
            "These two aren't the same.",
            "They cannot be the same individual."
        ]
        for raw in negatives {
            let r = DeepAnalyze.parseFaceComparison(raw)
            #expect(r.sameClass == false, "negated verdict must be DIFFERENT: \(raw)")
            // The auto-merge gate is sameClass && confidence > 0.75; a false
            // sameClass already blocks it regardless of confidence.
            #expect(!(r.sameClass && r.confidence > 0.75),
                    "negated verdict must never clear the 0.75 auto-merge gate: \(raw)")
        }
    }

    @Test("parseFaceComparison: affirmative SAME still parses + keeps explicit confidence")
    func affirmativeVerdictPreserved() {
        let same = DeepAnalyze.parseFaceComparison("VERDICT: SAME\nCONFIDENCE: 0.92")
        #expect(same.sameClass == true)
        #expect(abs(same.confidence - 0.92) < 0.001)

        // Loose affirmative with no confidence number → clears the gate.
        let loose = DeepAnalyze.parseFaceComparison("Yes, these are the same person.")
        #expect(loose.sameClass == true)
        #expect(loose.confidence > 0.75)

        let diff = DeepAnalyze.parseFaceComparison("VERDICT: DIFFERENT\nCONFIDENCE: 0.9")
        #expect(diff.sameClass == false)

        // R-12: an explicit "VERDICT: SAME" line is authoritative — incidental
        // negation about lighting/angle must NOT be picked up by the
        // negated-same heuristic and flip the verdict to DIFFERENT.
        let incidental = DeepAnalyze.parseFaceComparison(
            "VERDICT: SAME\nCONFIDENCE: 0.92\nThese are not in the same lighting but clearly the same person.")
        #expect(incidental.sameClass == true,
                "explicit VERDICT: SAME must survive incidental negated phrasing")
        #expect(abs(incidental.confidence - 0.92) < 0.001)

        // An explicit DIFFERENT still wins even alongside a SAME line.
        let conflict = DeepAnalyze.parseFaceComparison("VERDICT: SAME\nVERDICT: DIFFERENT")
        #expect(conflict.sameClass == false)
    }

    // R-11 — the shared single-flight load must be cancelled only when its LAST
    // joined waiter bails, so cancelling a prewarm can't abort a run joined to
    // the same download (and vice-versa). The load lifecycle needs a live MLX
    // load (verified on-device), but the waiter ref-count that gates the
    // cancel decision is pure and unit-assertable.
    @Test("ModelLoadGate: shared load cancels only when the final waiter bails")
    func loadGateRefCountsWaiters() {
        // Two waiters (e.g. a prewarm + a run joined to the same download).
        let two = ModelLoadGate()
        two.enter(); two.enter()
        #expect(two.bail() == false, "first of two waiters bailing must NOT cancel the shared load")
        #expect(two.bail() == true,  "the last waiter bailing cancels the shared load")

        // A sole waiter cancels the shared load immediately.
        let one = ModelLoadGate()
        one.enter()
        #expect(one.bail() == true)

        // A bail with no registered waiter never cancels.
        let none = ModelLoadGate()
        #expect(none.bail() == false)
    }

    // F-C3-044 — refuse a decompression bomb above 50 MP (Windows parity).
    @Test("pixelsExceedDecodeCap: 50 MP cap")
    func decodeCap() {
        #expect(DeepAnalyze.pixelsExceedDecodeCap(width: 8000, height: 7000))   // 56 MP
        #expect(!DeepAnalyze.pixelsExceedDecodeCap(width: 5000, height: 5000))  // 25 MP
        #expect(!DeepAnalyze.pixelsExceedDecodeCap(width: 0, height: 0))        // unknown → pass
        // Exactly at the cap is allowed; one pixel over is refused.
        #expect(!DeepAnalyze.pixelsExceedDecodeCap(width: 50_000_000, height: 1))
        #expect(DeepAnalyze.pixelsExceedDecodeCap(width: 50_000_001, height: 1))
    }

    // F-C3-043 — the HF tree listing must be recursive, or a repo with any
    // subfolder installs incomplete yet writes the verified sentinel.
    @Test("treeListURL: listing is recursive")
    func recursiveTreeListing() throws {
        let url = try #require(VLMDownloader.treeListURL(
            repo: "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit", revision: "abc123"))
        #expect(url.query?.contains("recursive=true") == true,
                "tree listing must be recursive: \(url.absoluteString)")
    }

    // F-C3-027 — folder-scope LIKE must escape `_`/`%`.
    @Test("escapeLike: backslashes LIKE metacharacters")
    func escapeLikeMetacharacters() {
        #expect(DeepAnalyzeRunner.escapeLike("a_b") == #"a\_b"#)
        #expect(DeepAnalyzeRunner.escapeLike("50%/x") == #"50\%/x"#)
        // Backslash itself is escaped first so the pattern stays well-formed.
        #expect(DeepAnalyzeRunner.escapeLike(#"a\b"#) == #"a\\b"#)
    }
}

@Suite("Deep Analyze DB + run() fixes (C3-DA)", .serialized)
struct DeepAnalyzeRunnerTests {

    private func makeDB() throws -> (Database, URL) {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDDeepTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        return (db, tmp)
    }

    private func insertFile(_ db: Database, path: String) async throws -> Int64 {
        try await db.pool.write { db in
            try db.execute(sql: """
                INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension)
                VALUES (?, 0, 0, ?, 'image', 'jpg')
                """, arguments: [path, Date().timeIntervalSince1970])
            return db.lastInsertedRowID
        }
    }

    // F-C3-027 — a folder whose name contains `_` must not over-match a
    // sibling subtree where any character sits in the `_` position.
    @Test("resolveTargets folder scope: '_' does not over-match siblings")
    func folderScopeEscapesUnderscore() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }

        let inFolder = "/root/a_b/photo.jpg"
        let sibling  = "/root/aXb/photo.jpg"   // 'X' would satisfy an unescaped '_'
        let deeper   = "/root/a_b/sub/deep.jpg"
        _ = try await insertFile(db, path: inFolder)
        _ = try await insertFile(db, path: sibling)
        _ = try await insertFile(db, path: deeper)

        let targets = try await DeepAnalyzeRunner.resolveTargets(
            database: db, scope: .folder(prefix: "/root/a_b"), modelKey: "m")
        let paths = Set(targets.map { $0.path })
        #expect(paths.contains(inFolder))
        #expect(paths.contains(deeper))
        #expect(!paths.contains(sibling),
                "unescaped LIKE '_' must not pull /root/aXb into the /root/a_b pass")
    }

    @Test("resolveTargets folder scope: '%' is treated literally")
    func folderScopeEscapesPercent() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }

        let inFolder = "/root/50%off/a.jpg"
        let sibling  = "/root/50anythingoff/b.jpg"  // '%' would match 'anything'
        _ = try await insertFile(db, path: inFolder)
        _ = try await insertFile(db, path: sibling)

        let targets = try await DeepAnalyzeRunner.resolveTargets(
            database: db, scope: .folder(prefix: "/root/50%off"), modelKey: "m")
        let paths = Set(targets.map { $0.path })
        #expect(paths.contains(inFolder))
        #expect(!paths.contains(sibling),
                "unescaped LIKE '%' must not pull /root/50anythingoff into the pass")
    }

    // F-C3-044 — a NULL caption / proposed name must preserve the prior
    // value (COALESCE), not clobber it; the model column always updates.
    @Test("persist: NULL caption/name preserves prior value (COALESCE)")
    func persistCoalesces() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }
        let id = try await insertFile(db, path: "/root/x.jpg")

        // Seed a prior good result.
        try await db.pool.write { db in
            try db.execute(sql: """
                UPDATE files SET vlm_description = 'old desc',
                                 vlm_proposed_name = 'old-name',
                                 vlm_model = 'm0' WHERE id = ?
                """, arguments: [id])
        }

        // New caption, but the model proposed no name → name must survive.
        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: "new desc", proposedName: nil, modelKey: "m1")
        var row = try await fetchVLM(db, id)
        #expect(row.desc == "new desc")
        #expect(row.name == "old-name", "NULL proposed_name must not clobber prior value")
        #expect(row.model == "m1")

        // Both NULL (e.g. inference produced nothing this pass) → both survive.
        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: nil, proposedName: nil, modelKey: "m2")
        row = try await fetchVLM(db, id)
        #expect(row.desc == "new desc")
        #expect(row.name == "old-name")
        #expect(row.model == "m2")

        // Real values overwrite.
        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: "newer", proposedName: "new-name", modelKey: "m3")
        row = try await fetchVLM(db, id)
        #expect(row.desc == "newer")
        #expect(row.name == "new-name")
        #expect(row.model == "m3")
    }

    private func fetchVLM(_ db: FileIDEngine.Database, _ id: Int64) async throws
        -> (desc: String?, name: String?, model: String?) {
        try await db.pool.read { db in
            guard let r = try Row.fetchOne(db, sql:
                "SELECT vlm_description, vlm_proposed_name, vlm_model FROM files WHERE id = ?",
                arguments: [id]) else { return (nil, nil, nil) }
            let desc: String? = r["vlm_description"]
            let name: String? = r["vlm_proposed_name"]
            let model: String? = r["vlm_model"]
            return (desc, name, model)
        }
    }

    // F-C3-025 + F-C3-028 — a cancel issued while the job was queued must
    // abort the run before any model load, and that exit must still emit
    // exactly one terminal deepAnalyzeComplete (cancelled = true).
    @Test("run(): a cancel issued while queued aborts before load and emits one terminal complete")
    func queuedCancelHonoredWithTerminalComplete() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }

        let cap = WireCapture()
        let sink = cap.sink

        // Cancel BEFORE run dispatches — mirrors a cancel pressed while the
        // job sat in the JobQueue.
        await DeepAnalyze.shared.requestCancel()
        defer { Task { await DeepAnalyze.shared.clearCancel() } }

        await DeepAnalyzeRunner.run(
            database: db, sink: sink,
            scope: .wholeLibrary(skipExisting: false),
            modelKind: .qwen3VL4B)
        await cap.finish()

        let completeNeedle = Data("\"deepAnalyzeComplete\"".utf8)
        let cancelledNeedle = Data("\"cancelled\":true".utf8)
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            if cap.bytes().range(of: completeNeedle) != nil { break }
            try await Task.sleep(nanoseconds: 50_000_000)
        }

        let out = cap.bytes()
        #expect(Self.count(of: completeNeedle, in: out) == 1,
                "exactly one terminal deepAnalyzeComplete must be emitted")
        #expect(out.range(of: cancelledNeedle) != nil,
                "the queued cancel must be honored (cancelled = true), not erased")
        // The cancelled job must not have loaded a model or processed files.
        let loaded = Data("\"deepAnalyzeFileDone\"".utf8)
        #expect(out.range(of: loaded) == nil, "no files processed for a cancelled job")
    }

    private static func count(of needle: Data, in data: Data) -> Int {
        var c = 0
        var range = data.startIndex..<data.endIndex
        while let r = data.range(of: needle, in: range) {
            c += 1
            range = r.upperBound..<data.endIndex
        }
        return c
    }
}

