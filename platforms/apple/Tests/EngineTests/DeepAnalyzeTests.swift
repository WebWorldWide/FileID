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
import AVFoundation
import GRDB
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database
import FileIDShared

@Suite("Qwen3-VL weight adapter")
struct Qwen3VLWeightAdapterTests {
    @Test("already-normalized lm_head keys survive the pinned MLX sanitizer")
    func normalizedLMHeadKey() {
        #expect(
            Qwen3VLWeightAdapter.keyForPinnedRuntime("language_model.lm_head.weight")
                == "lm_head.weight"
        )
        #expect(
            Qwen3VLWeightAdapter.wrappedKey("language_model.lm_head.weight")
                == "model.language_model.lm_head.weight"
        )
    }

    @Test("unrelated model keys remain unchanged")
    func unrelatedKey() {
        let key = "language_model.model.layers.0.self_attn.q_proj.weight"
        #expect(Qwen3VLWeightAdapter.keyForPinnedRuntime(key) == key)
    }
}

@Suite("Deep Analyze pure-logic fixes (C3-DA)")
struct DeepAnalyzePureLogicTests {

    @Test("filename prompt forbids inferred dates")
    func filenamePromptForbidsInferredDates() {
        let prompt = DeepAnalyze.analysisSystemPrompt(faceNames: ["Adam"])
        #expect(prompt.contains("visibly legible"))
        #expect(prompt.contains("never infer or invent a year"))
        #expect(prompt.contains("never concatenate words"))
        #expect(prompt.contains("only when it is clearly legible"))
        #expect(prompt.contains("Never infer a person's identity or name"))
        #expect(prompt.contains("Known people in this photo: Adam"))
        #expect(!prompt.contains("Self.filenameDateRule"))
        #expect(DeepAnalyze.filenameRetryPrompt.contains("Use only facts stated"))
    }

    @Test("malformed generated filenames are repaired or rejected")
    func malformedGeneratedFilenamesAreRejected() {
        #expect(!DeepAnalyze.isAcceptableProposedName("ramsonmakeup"))
        #expect(!DeepAnalyze.isAcceptableProposedName("family-photo-at-park"))
        #expect(DeepAnalyze.isAcceptableProposedName("boy-getting-face-paint"))
        #expect(DeepAnalyze.filenameOnlyCandidate(
            "FILENAME: boy-getting-face-paint\nNo extension."
        ) == "boy-getting-face-paint")
        #expect(DeepAnalyze.filenameOnlyCandidate("ramsonmakeup") == nil)
    }

    @Test("empty metadata-only results never establish model completion")
    func emptyMetadataResultIsFailure() {
        #expect(DeepAnalyzeRunner.isAnalysisFailure(
            DeepAnalyze.AnalysisResult(description: "", proposedName: nil, tags: [])
        ))
        #expect(!DeepAnalyzeRunner.isAnalysisFailure(
            DeepAnalyze.AnalysisResult(description: "", proposedName: "Rain", tags: [])
        ))
        #expect(!DeepAnalyzeRunner.isAnalysisFailure(
            DeepAnalyze.AnalysisResult(description: "", proposedName: nil, tags: ["BrushedSteel"])
        ))
    }

    @Test("unsupported identity claims and filename names are removed")
    func ungroundedIdentityClaimsAreRemoved() {
        let grounded = DeepAnalyze.removingUngroundedIdentityClaims(
            from: "Two boys, identified as Jacob and Mason, sit by a window.",
            faceNames: []
        )
        #expect(grounded.description == "Two boys sit by a window.")
        #expect(grounded.rejectedTokens == ["jacob", "mason"])
        #expect(DeepAnalyze.removingRejectedIdentityTokens(
            from: "jacob-mason-window-smile",
            rejectedTokens: grounded.rejectedTokens
        ) == nil)
    }

    @Test("known face names remain available to the model")
    func knownIdentityClaimsRemain() {
        let description = "Two boys, identified as Jacob and Mason, sit by a window."
        let grounded = DeepAnalyze.removingUngroundedIdentityClaims(
            from: description,
            faceNames: ["Jacob", "Mason"]
        )
        #expect(grounded.description == description)
        #expect(grounded.rejectedTokens.isEmpty)
    }

    @Test("proposed filenames retain only trusted metadata or OCR years")
    func proposedFilenameYearsAreGrounded() {
        #expect(DeepAnalyzeRunner.removingUntrustedYearTokens(
            from: "family-holiday-photo-2023", trustedYears: [2007]
        ) == "family-holiday-photo")
        #expect(DeepAnalyzeRunner.removingUntrustedYearTokens(
            from: "family-holiday-2007", trustedYears: [2007]
        ) == "family-holiday-2007")
        #expect(DeepAnalyzeRunner.removingUntrustedYearTokens(
            from: "2023_family-holiday_2007", trustedYears: [2007]
        ) == "family-holiday_2007")
        #expect(DeepAnalyzeRunner.removingUntrustedYearTokens(
            from: "2023", trustedYears: []
        ) == nil)
        #expect(DeepAnalyzeRunner.trustedYears(
            in: "Invoice 2024; reference 1234; copyright 1899"
        ) == [2024])
        #expect(!DeepAnalyze.hasMinimumGeneratedFilenameWords("family-holiday"))
        #expect(DeepAnalyze.hasMinimumGeneratedFilenameWords("adam-family-holiday"))
    }

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

    // macOS lockstep: parseVLMTags mirrors Windows parse_vlm_tags 1:1.
    @Test("parseVLMTags: splits, lowercases, strips punctuation")
    func vlmTagsSplitLowerStrip() {
        #expect(DeepAnalyze.parseVLMTags("Dog, beach.") == ["dog", "beach"])
    }

    @Test("parseVLMTags: strips list numbering and dedupes")
    func vlmTagsNumberingAndDedupe() {
        #expect(DeepAnalyze.parseVLMTags("1. dog\n2. dog\n3. ocean") == ["dog", "ocean"])
    }

    @Test("parseVLMTags: drops sentence fragments (>2 words), keeps short tags")
    func vlmTagsDropsFragments() {
        #expect(DeepAnalyze.parseVLMTags("a dog running on the beach at sunset, beach") == ["beach"])
    }

    @Test("parseVLMTags: filters generic stopwords and caps at 2")
    func vlmTagsStopwordsAndCap() {
        #expect(DeepAnalyze.parseVLMTags("photo, image, sushi platter, mountain lake, extra tag")
                == ["sushi platter", "mountain lake"])
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

    @Test("video keyframe target is one quarter of duration")
    func representativeVideoTime() {
        #expect(abs(DeepAnalyze.representativeVideoTime(durationSeconds: 120).seconds - 30) < 0.001)
        #expect(abs(DeepAnalyze.representativeVideoTime(durationSeconds: 3).seconds - 0.75) < 0.001)
        #expect(abs(DeepAnalyze.representativeVideoTime(durationSeconds: nil).seconds - 1) < 0.001)
        #expect(abs(DeepAnalyze.representativeVideoTime(durationSeconds: .infinity).seconds - 1) < 0.001)
    }

    // F-C3-043 — the HF tree listing must be recursive, or a repo with any
    // subfolder installs incomplete yet writes the verified sentinel.
    @Test("video duration loading has a wall-clock timeout")
    func videoDurationTimeout() async {
        let asset = AVURLAsset(url: URL(fileURLWithPath: "/definitely/missing/video.mp4"))
        let clock = ContinuousClock()
        let started = clock.now
        let duration = await DeepAnalyze.loadVideoDurationSeconds(asset, timeoutSeconds: 0)
        #expect(duration == nil)
        #expect(started.duration(to: clock.now) < .seconds(1))
    }

    @Test("treeListURL: listing is recursive")
    func recursiveTreeListing() throws {
        let url = try #require(VLMDownloader.treeListURL(
            repo: "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit", revision: "abc123"))
        #expect(url.query?.contains("recursive=true") == true,
                "tree listing must be recursive: \(url.absoluteString)")
    }

    @Test("verified VLM sentinel detects missing and same-size-corrupt files")
    func verifiedSentinelAttestsInstalledFiles() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileID-VLM-Sentinel-\(UUID().uuidString)")
        let modelDir = tmp.appendingPathComponent("model")
        let sentinel = modelDir.appendingPathComponent(".fileid-verified-rev")
        try FileManager.default.createDirectory(at: modelDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let file = modelDir.appendingPathComponent("config.json")
        try Data("alpha".utf8).write(to: file)
        let listed = [VLMRepoFile(path: "config.json", size: 5, sha256: nil)]
        try await VLMDownloader.writeVerifiedSentinel(
            sentinel,
            modelDir: modelDir,
            revision: "rev",
            files: listed
        )
        #expect(await VLMDownloader.verifiedSentinelIsValid(
            sentinel, modelDir: modelDir, revision: "rev"
        ))

        try Data("omega".utf8).write(to: file)
        let corruptIsValid = await VLMDownloader.verifiedSentinelIsValid(
            sentinel, modelDir: modelDir, revision: "rev"
        )
        #expect(!corruptIsValid)

        try FileManager.default.removeItem(at: file)
        let missingIsValid = await VLMDownloader.verifiedSentinelIsValid(
            sentinel, modelDir: modelDir, revision: "rev"
        )
        #expect(!missingIsValid)
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
    @Test("batch proposed names use source stems to avoid duplicates")
    func batchProposedNamesAreDistinct() {
        var reserved = Set<String>()
        let first = DeepAnalyzeRunner.reserveProposedName(
            "service-report",
            sourcePath: "/library/report-2026-08-01.pdf",
            reserved: &reserved
        )
        let second = DeepAnalyzeRunner.reserveProposedName(
            "service-report",
            sourcePath: "/library/report-2026-08-02.pdf",
            reserved: &reserved
        )

        #expect(first == "service-report")
        #expect(second == "service-report-report-2026-08-02")
    }

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

    @Test("skipExisting keys selected and library scopes on full completion")
    func skipExistingUsesFullCompletionModel() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }
        let legacy = try await insertFile(db, path: "/root/legacy.jpg")
        let complete = try await insertFile(db, path: "/root/complete.jpg")
        let otherModel = try await insertFile(db, path: "/root/other.jpg")
        try await db.pool.write { db in
            try db.execute(sql: """
                UPDATE files SET vlm_model = 'model-a' WHERE id = ?
                """, arguments: [legacy])
            try db.execute(sql: """
                UPDATE files
                SET vlm_model = 'model-a', vlm_full_model = 'model-a'
                WHERE id = ?
                """, arguments: [complete])
            try db.execute(sql: """
                UPDATE files
                SET vlm_model = 'model-b', vlm_full_model = 'model-b'
                WHERE id = ?
                """, arguments: [otherModel])
        }

        let selected = try await DeepAnalyzeRunner.resolveTargets(
            database: db,
            scope: .selected(fileIDs: [complete, legacy, otherModel], skipExisting: true),
            modelKey: "model-a")
        #expect(selected.map { $0.id } == [legacy, otherModel])

        let wholeLibrary = try await DeepAnalyzeRunner.resolveTargets(
            database: db,
            scope: .wholeLibrary(skipExisting: true, excludedFolders: []),
            modelKey: "model-a")
        #expect(Set(wholeLibrary.map { $0.id }) == Set([legacy, otherModel]))
    }

    // Mirrors the Rust engine's `exclusion_where_clause_respects_folder_boundaries_end_to_end`:
    // excluding a folder must not also exclude a sibling that merely shares
    // a text prefix, and must not touch an unrelated file.
    @Test("resolveTargets wholeLibrary scope: folder exclusion respects sibling-prefix boundaries")
    func wholeLibraryExclusionRespectsFolderBoundaries() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }

        let excludedDir = "/library/photos"
        let siblingDir  = "/library/photosbackup"
        let root        = "/library"

        let keptOut    = try await insertFile(db, path: "\(excludedDir)/kept_out.jpg")
        let keptIn     = try await insertFile(db, path: "\(siblingDir)/kept_in.jpg")
        let alsoKeptIn = try await insertFile(db, path: "\(root)/also_kept_in.jpg")

        let targets = try await DeepAnalyzeRunner.resolveTargets(
            database: db,
            scope: .wholeLibrary(skipExisting: false, excludedFolders: [excludedDir]),
            modelKey: "m")
        let ids = Set(targets.map { $0.id })

        #expect(!ids.contains(keptOut),
                "the excluded folder's own file must be dropped")
        #expect(ids.contains(keptIn),
                "a same-prefix sibling folder must survive")
        #expect(ids.contains(alsoKeptIn),
                "an unrelated file must survive")
    }

    @Test("exclusionWhereClause: empty for no exclusions, ignores relative paths")
    func exclusionWhereClauseEdgeCases() {
        let empty = DeepAnalyzeRunner.exclusionWhereClause([])
        #expect(empty.sql.isEmpty)
        #expect(empty.params.isEmpty)

        let relative = DeepAnalyzeRunner.exclusionWhereClause(["not/absolute"])
        #expect(relative.sql.isEmpty)
        #expect(relative.params.isEmpty)
    }

    @Test("persist full pass clears requested empty outputs and stale tags")
    func persistFullPassClearsRequestedEmptyOutputs() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }
        let id = try await insertFile(db, path: "/root/x.jpg")

        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: "old desc", proposedName: "old-name", tags: ["old-tag"],
            modelKey: "model-a")
        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: "", proposedName: nil, tags: [],
            modelKey: "model-b")
        let row = try await fetchVLM(db, id)
        let completion = try await fetchCompletion(db, id)
        let tagCount = try await vlmTagCount(db, id)

        #expect(row.desc == nil)
        #expect(row.name == nil)
        #expect(row.model == "model-b")
        #expect(completion.fullModel == "model-b")
        #expect(tagCount == 0)
    }

    @Test("persist partial clears requested empty output but preserves unrequested same-model data")
    func persistPartialRespectsRequestedComponents() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }
        let id = try await insertFile(db, path: "/root/partial.jpg")
        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: "old desc", proposedName: "keep-name", tags: ["old-tag"],
            modelKey: "model-a")

        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: "", proposedName: nil, tags: [],
            modelKey: "model-a",
            updatesDescription: true,
            updatesProposedName: false,
            completesFullPass: false)
        let row = try await fetchVLM(db, id)
        let completion = try await fetchCompletion(db, id)
        let tagCount = try await vlmTagCount(db, id)

        #expect(row.desc == nil)
        #expect(row.name == "keep-name")
        #expect(row.model == "model-a")
        #expect(completion.fullModel == "model-a")
        #expect(tagCount == 0)
    }

    @Test("persist tracks full completion without misattributing partial output")
    func persistFullCompletionSemantics() async throws {
        let (db, tmp) = try makeDB()
        defer { try? FileManager.default.removeItem(at: tmp) }
        let id = try await insertFile(db, path: "/root/completion.jpg")

        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: "full", proposedName: "full-name",
            modelKey: "model-a", completesFullPass: true)
        var state = try await fetchCompletion(db, id)
        let completedAt = try #require(state.analyzedAt)
        #expect(state.model == "model-a")
        #expect(state.fullModel == "model-a")

        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: nil, proposedName: nil, tags: ["same-model"],
            modelKey: "model-a",
            updatesDescription: false,
            updatesProposedName: false,
            completesFullPass: false)
        state = try await fetchCompletion(db, id)
        #expect(state.model == "model-a")
        #expect(state.fullModel == "model-a")
        #expect(state.analyzedAt == completedAt)

        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: "partial", proposedName: nil, tags: ["different-model"],
            modelKey: "model-b",
            updatesDescription: true,
            updatesProposedName: false,
            completesFullPass: false)
        state = try await fetchCompletion(db, id)
        #expect(state.model == nil)
        #expect(state.fullModel == nil)
        #expect(state.analyzedAt == nil)

        try await DeepAnalyzeRunner.persist(
            database: db, fileID: id,
            description: "full b", proposedName: "full-b",
            modelKey: "model-b", completesFullPass: true)
        state = try await fetchCompletion(db, id)
        #expect(state.model == "model-b")
        #expect(state.fullModel == "model-b")
        #expect(state.analyzedAt != nil)
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

    private func fetchCompletion(_ db: FileIDEngine.Database, _ id: Int64) async throws
        -> (model: String?, fullModel: String?, analyzedAt: Double?) {
        try await db.pool.read { db in
            guard let r = try Row.fetchOne(db, sql: """
                SELECT vlm_model, vlm_full_model, vlm_analyzed_at
                FROM files WHERE id = ?
                """, arguments: [id]) else {
                return (nil, nil, nil)
            }
            let model: String? = r["vlm_model"]
            let fullModel: String? = r["vlm_full_model"]
            let analyzedAt: Double? = r["vlm_analyzed_at"]
            return (model, fullModel, analyzedAt)
        }
    }

    private func vlmTagCount(_ db: FileIDEngine.Database, _ id: Int64) async throws -> Int {
        try await db.pool.read { db in
            try Int.fetchOne(
                db,
                sql: "SELECT COUNT(*) FROM tags WHERE file_id = ? AND source = 'vlm'",
                arguments: [id]) ?? -1
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
            scope: .wholeLibrary(skipExisting: false, excludedFolders: []),
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
