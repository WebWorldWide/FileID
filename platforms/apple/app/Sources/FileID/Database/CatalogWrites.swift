// The app's catalog edits: People names, merges and face moves, and
// suggested-name renames with their undo. The engine is the catalog's only
// writer, so each edit is an engine command and this awaits its
// bulkActionResult. ReadStore only reads.
import Foundation
import FileIDShared

@MainActor
enum CatalogWrites {
    static let notReady = "FileID is still getting ready. Try again in a moment."
    static let noAnswer = "FileID didn't answer in time. Check the files before trying again."

    /// Sends `command` and waits for its reply. `id` picks this request's
    /// reply out of others with the same action (a reply without per-item
    /// ids, such as "Database unavailable", is accepted too). A command that
    /// can't be sent or isn't answered comes back as a failed result.
    static func request(_ engine: EngineClient, _ command: IPCCommand.Payload, action: String,
                        id: Int64?, timeout: Double) async -> BulkActionResult {
        var seen = engine.bulkActionResultSignal
        guard engine.send(command) else {
            return BulkActionResult(action: action, succeeded: 0, failed: 0, messages: [BulkActionItem(ok: false, message: notReady)])
        }
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if engine.bulkActionResultSignal != seen {
                seen = engine.bulkActionResultSignal
                if let result = engine.bulkActionResult, result.action == action,
                   id == nil || result.messages.allSatisfy({ $0.fileID == nil }) || result.messages.contains(where: { $0.fileID == id }) {
                    return result
                }
            }
            try? await Task.sleep(for: .milliseconds(15))
        }
        return BulkActionResult(action: action, succeeded: 0, failed: 0, messages: [BulkActionItem(ok: false, message: noAnswer)])
    }

    private static func answered(_ result: BulkActionResult) -> Bool {
        !result.messages.contains { $0.fileID == nil && ($0.message == notReady || $0.message == noAnswer) }
    }

    // MARK: - People

    @discardableResult
    static func renamePerson(_ engine: EngineClient, store: ReadStore, id: Int64, title: String?, firstName: String?,
                             middleName: String?, lastName: String?, suffix: String?) async -> Bool {
        let result = await request(engine, .renamePerson(personID: id, title: title, firstName: firstName, middleName: middleName,
                                                         lastName: lastName, suffix: suffix),
                                   action: "renamePerson", id: id, timeout: 30)
        store.notifyChanged()
        return result.succeeded == 1
    }

    /// Marking someone unknown also clears their name, as on every platform.
    @discardableResult
    static func markUnknown(_ engine: EngineClient, store: ReadStore, ids: [Int64]) async -> Int {
        guard let first = ids.first else { return 0 }
        let result = await request(engine, .markPersonsAsUnknown(personIDs: ids), action: "markPersonsAsUnknown",
                                   id: first, timeout: 30 + Double(ids.count) / 50)
        store.notifyChanged()
        return result.succeeded
    }

    /// Merges each source into `target` (the engine keeps `target` and moves a
    /// source's typed name onto it when `target` has none). Returns the
    /// target's photo count, or nil when nothing merged.
    static func merge(_ engine: EngineClient, store: ReadStore, target: Int64, sources: [Int64],
                      progress: MergeProgress? = nil, shouldStop: MergeStop? = nil) async -> Int? {
        var seen = Set<Int64>()
        let sources = sources.filter { $0 != target && seen.insert($0).inserted }
        let merged = await mergeBatch(engine, store: store, plan: [(target: target, sources: sources)],
                                      progress: progress, shouldStop: shouldStop)
        guard merged > 0 else { return nil }
        return await Task.detached(priority: .userInitiated) { store.personFileCount(id: target) }.value ?? 0
    }

    typealias MergeProgress = @MainActor (_ current: Int, _ total: Int) -> Void
    typealias MergeStop = @MainActor () -> Bool

    /// Runs a `ReadStore.mergeBatchPlan`. Returns how many people were merged away.
    /// `progress` is told which merge is starting; `shouldStop` is asked
    /// between merges. Stopping is a flag rather than task cancellation
    /// because `request` polls with a sleep that cancellation would turn
    /// into a busy loop.
    static func mergeBatch(_ engine: EngineClient, store: ReadStore, plan: [(target: Int64, sources: [Int64])],
                           progress: MergeProgress? = nil, shouldStop: MergeStop? = nil) async -> Int {
        let total = plan.reduce(0) { sum, group in sum + group.sources.filter { $0 != group.target }.count }
        var merged = 0, started = 0
        outer: for group in plan {
            for source in group.sources where source != group.target {
                if shouldStop?() == true { break outer }
                started += 1
                progress?(started, total)
                let result = await request(engine, .mergeClusters(sourcePersonID: source, destinationPersonID: group.target),
                                           action: "mergeClusters", id: source, timeout: 120)
                if result.succeeded == 1 { merged += 1 }
                if !answered(result) { break outer }
            }
        }
        if !plan.isEmpty { store.notifyChanged() }
        return merged
    }

    /// Moves every face of `source` on `fileIDs` to `target`. Returns how many faces moved.
    static func moveFaces(_ engine: EngineClient, store: ReadStore, from source: Int64, to target: Int64, fileIDs: [Int64]) async -> Int {
        guard source != target, !fileIDs.isEmpty else { return 0 }
        let faces = await Task.detached(priority: .userInitiated) { store.faceIDs(personID: source, fileIDs: fileIDs) }.value
        var moved = 0
        for face in faces {
            let result = await request(engine, .reassignFace(faceID: face, destinationPersonID: target, createNewPerson: false),
                                       action: "reassignFace", id: face, timeout: 30)
            if result.succeeded == 1 { moved += 1 }
            if !answered(result) { break }
        }
        store.notifyChanged()
        return moved
    }

    // MARK: - Suggested-name renames

    struct RenameCandidate: Sendable {
        let fileID: Int64
        let path: String
        let proposed: String?
    }

    struct RenamePlan: Sendable {
        let fileID: Int64
        let oldPath: String
        let target: URL
    }

    private static let renameChunk = 250

    /// Picks each file's suggested name in its own folder, adding `_2`…`_99`
    /// when the name is taken on disk or by an earlier file in the batch.
    /// A file already carrying its name is left out; one with no free name
    /// counts in `unnamed`.
    nonisolated static func planRenames(_ files: [RenameCandidate]) -> (plans: [RenamePlan], unnamed: Int) {
        let fm = FileManager.default
        var claimed = Set<String>()
        var plans: [RenamePlan] = []
        var unnamed = 0
        for file in files {
            guard let proposed = file.proposed, !proposed.isEmpty else { unnamed += 1; continue }
            let oldURL = URL(fileURLWithPath: file.path)
            let dir = oldURL.deletingLastPathComponent()
            let ext = oldURL.pathExtension
            func candidate(_ bump: Int) -> URL {
                let stem = bump == 1 ? proposed : "\(proposed)_\(bump)"
                return dir.appendingPathComponent(ext.isEmpty ? stem : "\(stem).\(ext)")
            }
            var bump = 1
            var target = candidate(bump)
            // Claimed paths compare case-folded: two files in a batch must not
            // land on names a case-insensitive volume treats as one.
            while claimed.contains(target.path.lowercased())
                    || (fm.fileExists(atPath: target.path) && !sameFileOnDisk(target, oldURL)) {
                bump += 1
                if bump > 99 { break }
                target = candidate(bump)
            }
            guard bump <= 99 else { unnamed += 1; continue }
            claimed.insert(target.path.lowercased())
            if target.path != oldURL.path { plans.append(RenamePlan(fileID: file.fileID, oldPath: file.path, target: target)) }
        }
        return (plans, unnamed)
    }

    /// Same file by inode, so a case-only rename on a case-insensitive volume
    /// keeps the file's own slot instead of bumping to `_2`.
    nonisolated static func sameFileOnDisk(_ a: URL, _ b: URL) -> Bool {
        if a == b { return true }
        guard let ia = (try? a.resourceValues(forKeys: [.fileResourceIdentifierKey]))?.fileResourceIdentifier,
              let ib = (try? b.resourceValues(forKeys: [.fileResourceIdentifierKey]))?.fileResourceIdentifier
        else { return false }
        return ia.isEqual(ib)
    }

    /// Renames files to the suggested names the user saw. Paths are re-read
    /// first so a file that moved since the list loaded is renamed where it is.
    static func applyProposedNames(_ files: [FileRow], engine: EngineClient, store: ReadStore) async -> ReadStore.BulkRenameResult {
        guard !files.isEmpty else { return ReadStore.BulkRenameResult(renamed: [], failed: 0, firstError: nil) }
        let (planned, missing) = await Task.detached(priority: .userInitiated) { () -> ((plans: [RenamePlan], unnamed: Int), Int) in
            let current = Dictionary(store.files(forFileIDs: files.map(\.id)).map { ($0.id, $0.pathText) },
                                     uniquingKeysWith: { first, _ in first })
            let fresh = files.compactMap { file in
                current[file.id].map { RenameCandidate(fileID: file.id, path: $0, proposed: file.vlmProposedName) }
            }
            return (planRenames(fresh), files.count - fresh.count)
        }.value
        var renamed: [ReadStore.RenameOutcome] = []
        var failed = planned.unnamed + missing
        var firstError: String?
        let plans = planned.plans
        for start in stride(from: 0, to: plans.count, by: renameChunk) {
            let chunk = Array(plans[start..<min(start + renameChunk, plans.count)])
            let result = await request(engine, .renameFiles(renames: chunk.map { RenameEntry(fileID: $0.fileID, newName: $0.target.lastPathComponent) }),
                                       action: "renameFiles", id: chunk[0].fileID, timeout: 60 + Double(chunk.count) / 4)
            let byID = Dictionary(chunk.map { ($0.fileID, $0) }, uniquingKeysWith: { first, _ in first })
            let landed = result.messages.filter(\.ok).compactMap(\.fileID).compactMap { byID[$0] }
            renamed += await Task.detached(priority: .userInitiated) {
                landed.map { ReadStore.RenameOutcome(fileID: $0.fileID, oldPath: $0.oldPath, newPath: $0.target.path) }
            }.value
            failed += chunk.count - landed.count
            if firstError == nil { firstError = result.messages.first { !$0.ok }?.message }
            if !answered(result) {
                failed += plans.count - start - chunk.count
                break
            }
        }
        if !renamed.isEmpty { store.notifyChanged() }
        return ReadStore.BulkRenameResult(renamed: renamed, failed: failed, firstError: firstError)
    }

    struct NamedRename: Sendable, Equatable {
        let fileID: Int64
        let newName: String
        var group: Int64? = nil
    }

    /// Chunks of at most `size` renames that never split a companion group,
    /// so each group's members land in one renameFiles batch.
    nonisolated static func groupedChunks(_ renames: [NamedRename], size: Int) -> [[NamedRename]] {
        var chunks: [[NamedRename]] = []
        var start = 0
        while start < renames.count {
            var end = min(start + size, renames.count)
            while end < renames.count, end > start, let group = renames[end].group, renames[end - 1].group == group { end -= 1 }
            if end == start {
                end = start + 1
                while end < renames.count, renames[end].group != nil, renames[end].group == renames[start].group { end += 1 }
            }
            chunks.append(Array(renames[start..<end]))
            start = end
        }
        return chunks
    }

    /// Renames files to names the user confirmed, exactly as given. A name
    /// that is taken fails for that file instead of getting a `_N` bump.
    static func applyNames(_ renames: [NamedRename], engine: EngineClient, store: ReadStore) async -> ReadStore.BulkRenameResult {
        guard !renames.isEmpty else { return ReadStore.BulkRenameResult(renamed: [], failed: 0, firstError: nil) }
        let current = await Task.detached(priority: .userInitiated) {
            Dictionary(store.files(forFileIDs: renames.map(\.fileID)).map { ($0.id, $0.pathText) }, uniquingKeysWith: { first, _ in first })
        }.value
        let missingGroups = Set(renames.filter { current[$0.fileID] == nil }.compactMap(\.group))
        let known = renames.filter { current[$0.fileID] != nil && !($0.group.map(missingGroups.contains) ?? false) }
        var renamed: [ReadStore.RenameOutcome] = []
        var failed = renames.count - known.count
        var firstError: String?
        var sent = 0
        for chunk in groupedChunks(known, size: renameChunk) {
            sent += chunk.count
            let result = await request(engine, .renameFiles(renames: chunk.map { RenameEntry(fileID: $0.fileID, newName: $0.newName) }),
                                       action: "renameFiles", id: chunk[0].fileID, timeout: 60 + Double(chunk.count) / 4)
            let byID = Dictionary(chunk.map { ($0.fileID, $0) }, uniquingKeysWith: { first, _ in first })
            let landed = result.messages.filter(\.ok).compactMap(\.fileID).compactMap { byID[$0] }
            renamed += await Task.detached(priority: .userInitiated) {
                landed.compactMap { entry in
                    current[entry.fileID].map { old in
                        ReadStore.RenameOutcome(fileID: entry.fileID, oldPath: old,
                                                newPath: URL(fileURLWithPath: old).deletingLastPathComponent().appendingPathComponent(entry.newName).path)
                    }
                }
            }.value
            failed += chunk.count - landed.count
            if firstError == nil { firstError = result.messages.first { !$0.ok }?.message }
            if !answered(result) {
                failed += known.count - sent
                break
            }
        }
        if !renamed.isEmpty { store.notifyChanged() }
        return ReadStore.BulkRenameResult(renamed: renamed, failed: failed, firstError: firstError)
    }

    /// One file's suggested-name rename. Returns its path afterwards (the old
    /// one when it already had the name), or nil when the rename failed.
    static func applyProposedName(_ file: FileRow, engine: EngineClient, store: ReadStore) async -> URL? {
        let result = await applyProposedNames([file], engine: engine, store: store)
        if let landed = result.renamed.first { return URL(fileURLWithPath: landed.newPath) }
        return result.failed == 0 ? file.url : nil
    }

    /// Why a recorded rename can't be put back, or nil when it can: the file
    /// moved or changed since, or its old name is taken by another file.
    nonisolated static func undoBlocker(_ outcome: ReadStore.RenameOutcome, catalogPath: String?) -> String? {
        let fm = FileManager.default
        let newURL = URL(fileURLWithPath: outcome.newPath), oldURL = URL(fileURLWithPath: outcome.oldPath)
        guard catalogPath == outcome.newPath, fm.fileExists(atPath: newURL.path),
              newURL.deletingLastPathComponent().path == oldURL.deletingLastPathComponent().path else { return "moved" }
        if let size = outcome.fileSize, let date = outcome.modifiedAt {
            let attrs = try? fm.attributesOfItem(atPath: newURL.path)
            guard let curSize = attrs?[.size] as? Int64, let curDate = attrs?[.modificationDate] as? Date,
                  curSize == size, abs(curDate.timeIntervalSince(date)) < 1 else { return "changed" }
        }
        let caseOnly = oldURL.lastPathComponent.caseInsensitiveCompare(newURL.lastPathComponent) == .orderedSame
        if fm.fileExists(atPath: oldURL.path), !(caseOnly && sameFileOnDisk(oldURL, newURL)) { return "occupied" }
        return nil
    }

    /// Puts a rename batch back. Entries whose file moved or changed since,
    /// or whose old name is taken, are skipped.
    static func undoRenames(_ outcomes: [ReadStore.RenameOutcome], engine: EngineClient, store: ReadStore) async -> (undone: Int, skipped: Int, failed: Int) {
        guard !outcomes.isEmpty else { return (0, 0, 0) }
        let ready = await Task.detached(priority: .userInitiated) { () -> [ReadStore.RenameOutcome] in
            let current = Dictionary(store.files(forFileIDs: outcomes.map(\.fileID)).map { ($0.id, $0.pathText) },
                                     uniquingKeysWith: { first, _ in first })
            return outcomes.reversed().filter { undoBlocker($0, catalogPath: current[$0.fileID]) == nil }
        }.value
        var undone = 0, failed = 0, putBack = Set<Int64>()
        for start in stride(from: 0, to: ready.count, by: renameChunk) {
            let chunk = Array(ready[start..<min(start + renameChunk, ready.count)])
            let result = await request(engine, .renameFiles(renames: chunk.map { RenameEntry(fileID: $0.fileID, newName: URL(fileURLWithPath: $0.oldPath).lastPathComponent) }),
                                       action: "renameFiles", id: chunk[0].fileID, timeout: 60 + Double(chunk.count) / 4)
            let ids = Set(chunk.map(\.fileID))
            let back = Set(result.messages.filter(\.ok).compactMap(\.fileID)).intersection(ids)
            putBack.formUnion(back)
            undone += back.count
            failed += chunk.count - back.count
            if !answered(result) {
                failed += ready.count - start - chunk.count
                break
            }
        }
        store.notifyChanged()
        await forgetLearned(ready, putBack: putBack, engine: engine)
        return (undone, outcomes.count - ready.count, failed)
    }

    /// A rename the user reversed shouldn't teach a name style.
    private static func forgetLearned(_ outcomes: [ReadStore.RenameOutcome], putBack: Set<Int64>, engine: EngineClient) async {
        let split = NameStyle.undone(NameStyle.storedLearned(), outcomes: outcomes, putBack: putBack)
        guard !split.forget.isEmpty else { return }
        let request = NameStyle.unlearnRequest(id: "names-unlearn-" + UUID().uuidString, edits: split.forget)
        guard let response = await TakeRequests.send(engine, request, timeout: 15), response.status == "ok" else { return }
        if let data = try? JSONEncoder().encode(split.keep) { UserDefaults.standard.set(data, forKey: NameStyle.learnedEditsKey) }
    }
}
