// Cleanup: two modes. "Exact" groups live full-file SHA-256 matches;
// "Similar" groups visually near-identical images by dHash Hamming distance
// (resizes / re-encodes / crops / light edits). Per-tile selection; the user
// can override the keeper per group or trash across all groups at once. Similar
// mode never pre-selects — those copies are NOT byte-identical.
import SwiftUI
import AppKit
import FileIDShared

struct CleanupView: View {
    let engine: EngineClient
    let store: ReadStore

    /// "exact" (live full-file SHA-256) | "similar" (perceptual dHash).
    @State private var mode: String = "exact"
    private var isSimilar: Bool { mode == "similar" }

    @State private var groups: [DuplicateGroup] = []
    @State private var lastSeenBatchIndex: Int = -1
    @State private var status: String?
    @State private var statusWarning = false
    @State private var exactPreviewPartial = false
    @State private var exactCandidateCount = 0
    @State private var exactSkipped = 0
    /// Single-flight guard: the per-group/header "Delete" buttons have no
    /// confirmation, so a rapid double-tap would otherwise trash the same files
    /// twice (the 2nd fails "already in Trash" → a confusing "1 failed"). (audit P2)
    @State private var deleting = false
    @State private var confirmDelete: Bool = false

    /// Initialized lazily on first reload to non-keepers per group.
    @State private var selection: Set<Int64> = []
    @State private var skippedGroups: Set<Int64> = []
    @State private var reloadTask: Task<Void, Never>?
    @State private var reloadPending = false

    /// Mirrors LibraryView: true while the engine is discovering, tagging, or
    /// post-processing. Gates the exact-mode live SHA-256 re-verify off the ~1 Hz
    /// scan batch ticks (see the batch handler). (audit — reload storm)
    private var scanActive: Bool {
        guard let p = engine.lastProgress else { return false }
        return p.phase == .discovering || p.phase == .tagging || p.phase == .postScan
    }

    private var visibleGroups: [DuplicateGroup] {
        groups.filter { !skippedGroups.contains($0.id) }
    }

    private var totalSelected: Int {
        visibleGroups.reduce(0) { acc, g in
            acc + g.files.reduce(0) { $0 + (selection.contains($1.id) ? 1 : 0) }
        }
    }

    private var totalSelectedMB: Double {
        visibleGroups.reduce(0.0) { acc, g in
            acc + g.files.reduce(0.0) { $0 + (selection.contains($1.id) ? $1.sizeMB : 0) }
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().opacity(0.4)
            if visibleGroups.isEmpty {
                empty
            } else {
                list
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .onAppear {
            store.openIfPossible()
            reload()
        }
        .onChange(of: engine.lastBatch?.batchIndex ?? -1) { _, new in
            if new != lastSeenBatchIndex {
                lastSeenBatchIndex = new
                store.notifyChanged(includeDuplicateMetrics: false)
                // Exact mode's reload() runs a live full-file SHA-256 pass over up
                // to ExactDuplicateVerifier.candidateCap candidates; the digest
                // cache lives only 30 s, so re-verifying on every ~1 Hz scan batch
                // keeps the whole candidate set hot and contends with the engine's
                // scan reads on the same drive for the entire scan. Defer the exact
                // re-verify to scan completion (the lastTerminalEventAt handler
                // below). Similar mode's perceptual index is cheap and drive-light,
                // so it keeps refreshing live. (audit — reload storm)
                if isSimilar || !scanActive { reload() }
            }
        }
        // Exact mode skips its per-batch reload during a live scan, so the final
        // verified set — and the last batch the scan-active skip drops — is computed
        // once here when the scan reaches a terminal phase. Mirrors LibraryView's
        // terminal reload; single-flight reload() coalesces if one is in flight.
        .onChange(of: engine.lastTerminalEventAt) { _, _ in
            store.notifyChanged()
            reload()
        }
        .onChange(of: confirmDelete) { _, presented in
            // Dialog dismissed (confirm, cancel, or click-away): apply any scan
            // batches we deferred while it was open, re-deriving keepers.
            if !presented { reload() }
        }
        .onChange(of: mode) { _, _ in
            // Switching modes starts from a clean slate — nothing carries over,
            // and Similar mode must begin with NOTHING pre-selected for deletion.
            selection.removeAll()
            skippedGroups.removeAll()
            status = nil
            statusWarning = false
            exactPreviewPartial = false
            exactCandidateCount = 0
            exactSkipped = 0
            groups = []
            reload()
        }
    }

    // MARK: - Header

    @ViewBuilder
    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Cleanup").font(.largeTitle.bold())
                Text(headerSubtitle)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            ThemedSegmentedControl(
                selection: $mode,
                options: [(tag: "exact", label: "Exact"), (tag: "similar", label: "Similar")]
            )
            .help("Exact: byte-for-byte identical copies. Similar: visually near-identical images (resizes, re-encodes, crops, light edits) found by perceptual hash — review each before deleting, they are NOT byte-identical.")
            Spacer()
            if !visibleGroups.isEmpty {
                HStack(spacing: 6) {
                    // Bulk "select every non-keeper" is hidden in Similar mode: those
                    // copies are NOT byte-identical, so one-click mass selection would
                    // be unsafe — the user must pick copies deliberately per group.
                    if !isSimilar {
                        Button("Select all non-keepers") { selectAllNonKeepers() }
                            .buttonStyle(.bordered)
                            .help("Select every duplicate except the keeper in each group.")
                    }
                    Button("Clear selection") { selection.removeAll() }
                        .buttonStyle(.bordered)
                        .disabled(totalSelected == 0)
                }
                Button {
                    confirmDeleteSelected()
                } label: {
                    Label(
                        "Delete \(totalSelected) selected (\(String(format: "%.1f MB", totalSelectedMB)))",
                        systemImage: "trash"
                    )
                    .fontWeight(.semibold)
                    .foregroundStyle(.red)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .background(Color.red.opacity(0.12))
                    .overlay(
                        RoundedRectangle(cornerRadius: 8)
                            .stroke(Color.red.opacity(0.5), lineWidth: 1)
                    )
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                }
                .buttonStyle(.plain)
                .disabled(totalSelected == 0)
                .help("Move every selected copy to Trash. The keeper of each group is preserved unless you explicitly checked it.")
                .confirmationDialog(
                    "Move \(totalSelected) file\(totalSelected == 1 ? "" : "s") to Trash?",
                    isPresented: $confirmDelete,
                    titleVisibility: .visible
                ) {
                    Button("Move to Trash", role: .destructive) {
                        Task { await trashSelected(across: visibleGroups) }
                    }
                    Button("Cancel", role: .cancel) { }
                } message: {
                    Text(confirmDeleteMessage)
                }
            }
        }
        .padding(20)
    }

    private var headerSubtitle: String {
        let g = visibleGroups.count
        let skipped = skippedGroups.count
        let base: String
        if isSimilar {
            base = g == 0
                ? "Visually similar images — resizes, re-encodes, crops, and light edits that byte-exact matching misses"
                : "\(g) similar group\(g == 1 ? "" : "s") · review each before deleting — these are NOT byte-identical"
        } else {
            let bytes = visibleGroups.reduce(Int64(0)) { $0 + $1.reclaimableBytes }
            let mb = String(format: "%.1f", Double(bytes) / 1_048_576)
            base = "\(g) verified duplicate group\(g == 1 ? "" : "s") · \(mb) MB reclaimable if you keep 1 per group"
        }
        var result = skipped > 0 ? "\(base) · \(skipped) skipped" : base
        if !isSimilar && exactPreviewPartial {
            result += " · verified preview is partial (\(exactCandidateCount) candidates"
            if exactSkipped > 0 { result += ", \(exactSkipped) unreadable or changed" }
            result += ")"
        }
        return result
    }

    // MARK: - Empty / list

    @ViewBuilder
    private var empty: some View {
        if store.totalFiles == 0 {
            EmptyStateView(
                icon: "trash.slash",
                title: "Nothing to clean up yet",
                message: "Pick a folder in the sidebar and click Start Scan. Once files are indexed, duplicate copies show up here grouped together — pick which copy to keep."
            )
        } else if !skippedGroups.isEmpty {
            VStack(spacing: 14) {
                EmptyStateView(
                    icon: "checkmark.seal.fill",
                    title: "All duplicate groups skipped",
                    message: "You've hidden every group from this view. Want to revisit them?"
                )
                Button("Show skipped groups again") { skippedGroups.removeAll() }
                    .buttonStyle(.bordered)
            }
        } else if !isSimilar && exactPreviewPartial {
            EmptyStateView(
                icon: "exclamationmark.magnifyingglass",
                title: "No duplicates in the verified preview",
                message: "FileID bounded this full-byte verification pass to protect memory and disk throughput. The library has \(exactCandidateCount) same-size candidates; additional duplicates may exist outside this preview."
            )
        } else if isSimilar {
            EmptyStateView(
                icon: "checkmark.seal.fill",
                title: "No visually similar images found",
                message: "All \(store.totalImages) images compared — none are near-identical within the similarity threshold. Byte-for-byte duplicates appear under \"Exact\"."
            )
        } else {
            EmptyStateView(
                icon: "checkmark.seal.fill",
                title: "No duplicates found",
                message: "All \(store.totalFiles) indexed files considered — none in this verified preview are byte-for-byte identical."
            )
        }
    }

    @ViewBuilder
    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
                // First-timer explainer above the groups. Inline (not a
                // tooltip) so the keeper concept is impossible to miss.
                if isSimilar {
                    // CRITICAL safety UX: similar groups are NOT byte-identical.
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                        Text("**Visually similar — review before deleting (not identical).** These images match by perceptual hash (resizes, re-encodes, crops, light edits), not byte-for-byte. Nothing is pre-selected: open each, confirm it's a true duplicate, then choose which copies to Trash.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: 6).fill(Color.orange.opacity(0.10)))
                    .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.orange.opacity(0.35), lineWidth: 1))
                    .padding(.bottom, 4)
                } else {
                    HStack(spacing: 8) {
                        Image(systemName: exactPreviewPartial
                              ? "exclamationmark.triangle.fill" : "info.circle")
                            .foregroundStyle(exactPreviewPartial ? .orange : .green)
                        Text(exactPreviewPartial
                             ? "**Verified preview is partial.** Every group shown passed a live full-file SHA-256 check, but candidate/read limits may leave additional duplicates undisplayed. Selected copies are re-read again immediately before Trash."
                             : "Each group was verified with a live full-file SHA-256. The **KEEPER** is the copy we recommend you keep. Selected copies are re-read again immediately before Trash; you can restore them if you change your mind.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: 6).fill(
                        (exactPreviewPartial ? Color.orange : Color.green).opacity(0.08)))
                    .overlay(RoundedRectangle(cornerRadius: 6).stroke(
                        (exactPreviewPartial ? Color.orange : Color.green).opacity(0.3),
                        lineWidth: 1))
                    .padding(.bottom, 4)
                }
                ForEach(visibleGroups) { group in
                    GroupCard(
                        group: group,
                        selection: $selection,
                        onSelectAll: { setGroup(group, allSelected: true) },
                        onSelectNone: { setGroup(group, allSelected: false) },
                        onSelectAllExceptKeeper: { setGroupNonKeepers(group) },
                        onInvert: { invertGroup(group) },
                        onSkip: { skippedGroups.insert(group.id) },
                        onDeleteGroup: { Task { await trashSelectedInGroup(group) } }
                    )
                }
                if let s = status {
                    HStack(spacing: 10) {
                        Image(systemName: statusWarning
                              ? "exclamationmark.triangle.fill" : "trash.fill")
                            .foregroundStyle(statusWarning ? .orange : .green)
                        Text(s)
                            .font(.callout)
                        Spacer()
                        Button("Open Trash") {
                            NSWorkspace.shared.open(
                                URL(fileURLWithPath: NSHomeDirectory())
                                    .appendingPathComponent(".Trash")
                            )
                        }
                        .buttonStyle(.bordered)
                        Button("Dismiss") {
                            status = nil
                            statusWarning = false
                        }
                            .buttonStyle(.borderless)
                    }
                    .padding(12)
                    .background(RoundedRectangle(cornerRadius: 8).fill(
                        (statusWarning ? Color.orange : Color.green).opacity(0.10)))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(
                        (statusWarning ? Color.orange : Color.green).opacity(0.4),
                        lineWidth: 1))
                    .help("Files moved to Trash can be restored in Finder: open Trash, right-click a file, and choose Put Back.")
                }
            }
            .padding(20)
        }
    }

    // MARK: - Selection helpers

    private func setGroup(_ g: DuplicateGroup, allSelected: Bool) {
        for f in g.files {
            if allSelected { selection.insert(f.id) } else { selection.remove(f.id) }
        }
    }

    private func setGroupNonKeepers(_ g: DuplicateGroup) {
        // Files are sorted keeper-first.
        for (i, f) in g.files.enumerated() {
            if i == 0 { selection.remove(f.id) } else { selection.insert(f.id) }
        }
    }

    private func invertGroup(_ g: DuplicateGroup) {
        for f in g.files {
            if selection.contains(f.id) { selection.remove(f.id) } else { selection.insert(f.id) }
        }
    }

    private func selectAllNonKeepers() {
        selection.removeAll()
        for g in visibleGroups { setGroupNonKeepers(g) }
    }

    // MARK: - Trash actions

    private var confirmDeleteMessage: String {
        let mb = String(format: "%.1f MB", totalSelectedMB)
        return "Moves the selected copies to Trash. Frees about \(mb). You can restore them from Trash if you change your mind."
    }

    private func confirmDeleteSelected() {
        confirmDelete = true
    }

    private func trashSelectedInGroup(_ g: DuplicateGroup) async {
        await trashSelected(across: [g])
    }

    private func trashSelected(across groupsToScan: [DuplicateGroup]) async {
        guard !deleting else { return }   // re-entrancy guard (audit P2)
        deleting = true
        statusWarning = false
        defer { deleting = false }
        // Build the parallel work list first: every (id, url, size) we
        // intend to trash. Doing this on the main actor up front keeps
        // SwiftUI selection/state reads off the concurrent path.
        struct TrashItem: Sendable {
            let id: Int64
            let url: URL
            let size: Int64
            let exactKeeper: URL?
        }
        var work: [TrashItem] = []
        var keeperTagsByVictim: [Int64: [URL]] = [:]
        var verificationRejected = 0
        for group in groupsToScan {
            let trashed = group.files.filter { selection.contains($0.id) }
            let kept = group.files.filter { !selection.contains($0.id) }
            let exactKeeper = group.isSimilar ? nil : kept.first?.url
            if !group.isSimilar && exactKeeper == nil {
                verificationRejected += trashed.count
                continue
            }
            for file in trashed {
                work.append(TrashItem(
                    id: file.id, url: file.url, size: file.sizeBytes,
                    exactKeeper: exactKeeper))
                keeperTagsByVictim[file.id] = kept.map(\.url)
            }
        }

        // Trash up to 8 files concurrently. Foundation's trashItem isn't
        // thread-hostile (Finder serializes journaling underneath), but
        // doing them sequentially on a 10K dedup pass = 10–50 s freeze.
        struct TrashResult: Sendable {
            let id: Int64
            let size: Int64
            let success: Bool
            let verificationFailed: Bool
        }
        let results: [TrashResult] = await withTaskGroup(of: TrashResult.self) { group in
            var inFlight = 0
            var i = 0
            var collected: [TrashResult] = []
            collected.reserveCapacity(work.count)
            let maxConcurrency = 8
            while i < work.count {
                if inFlight >= maxConcurrency {
                    if let r = await group.next() { collected.append(r); inFlight -= 1 }
                }
                let item = work[i]
                group.addTask {
                    if let keeper = item.exactKeeper {
                        guard let verifiedVictim = await ExactDuplicateVerifier.matchesImmediately(
                            keeper: keeper, victim: item.url, expectedSize: item.size),
                              ExactFileDigest.pathStillMatches(verifiedVictim) else {
                            return TrashResult(
                                id: item.id, size: item.size, success: false,
                                verificationFailed: true)
                        }
                    }
                    do {
                        try FileManager.default.trashItem(at: item.url, resultingItemURL: nil)
                        return TrashResult(
                            id: item.id, size: item.size, success: true,
                            verificationFailed: false)
                    } catch {
                        // NSError descriptions embed the full path — log
                        // domain+code only, beside the redacted copy.
                        let ns = error as NSError
                        NSLog("FileID v2 cleanup: could not trash %@: %@ (%ld)", redactPathForLog(item.url.path), ns.domain, ns.code)
                        return TrashResult(
                            id: item.id, size: item.size, success: false,
                            verificationFailed: false)
                    }
                }
                inFlight += 1
                i += 1
            }
            for await r in group { collected.append(r) }
            return collected
        }

        var trashedIDs: [Int64] = []
        var freedBytes: Int64 = 0
        verificationRejected += results.filter(\.verificationFailed).count
        let trashFailures = results.filter { !$0.success && !$0.verificationFailed }.count
        for result in results where result.success {
            trashedIDs.append(result.id)
            freedBytes += result.size
        }
        let mb = Double(freedBytes) / 1_048_576
        var keeperURLSet = Set<URL>()
        for id in trashedIDs {
            for url in keeperTagsByVictim[id] ?? [] { keeperURLSet.insert(url) }
        }
        let keeperURLsToTag = Array(keeperURLSet)

        // P5 — auto-tag keepers (Settings toggle, default on). Useful so
        // the user can find "files I deduped this session" in Finder.
        let autoTagOn = UserDefaults.standard.object(forKey: AppSettings.cleanupAutoTagKey) == nil
            ? AppSettings.cleanupAutoTagDefault
            : UserDefaults.standard.bool(forKey: AppSettings.cleanupAutoTagKey)
        let shouldTag = autoTagOn && !keeperURLsToTag.isEmpty

        // Run the chunked DELETE (+ person reconciliation) and the per-keeper
        // xattr writes OFF the MainActor — on a large dedup pass these are
        // thousands of synchronous DB/FS ops and would re-freeze the UI the
        // off-main trashing above just avoided. Resume on the MainActor only
        // to mutate @State. (mirrors BulkTagSheet.apply)
        let taggedAdded: Int = await Task.detached(priority: .userInitiated) {
            [store, trashedIDs, keeperURLsToTag, shouldTag] in
            _ = store.deleteFiles(ids: trashedIDs)
            guard shouldTag else { return 0 }
            return TagWriter.addTagsBulk([AppSettings.cleanupAutoTagName],
                                         to: keeperURLsToTag).added
        }.value

        for id in trashedIDs { selection.remove(id) }
        if shouldTag { store.notifyChanged() }

        // Plain-language status. "DB rows pruned" is internal noise —
        // users care about file count + reclaimed space. Tag summary
        // appended only when the auto-tag toggle did something.
        var tagSummary = ""
        if taggedAdded > 0 {
            tagSummary = " · tagged \(taggedAdded) keeper\(taggedAdded == 1 ? "" : "s") with \"\(AppSettings.cleanupAutoTagName)\""
        }
        var summary = "Trashed \(trashedIDs.count) file\(trashedIDs.count == 1 ? "" : "s")"
            + " · freed \(String(format: "%.1f", mb)) MB"
            + tagSummary
        if verificationRejected > 0 {
            summary += " · \(verificationRejected) skipped because full-byte verification changed or failed"
        }
        if trashFailures > 0 {
            summary += " · \(trashFailures) Trash operation\(trashFailures == 1 ? "" : "s") failed"
        }
        statusWarning = verificationRejected > 0 || trashFailures > 0
        status = summary
        reload()
    }

    private func reload() {
        // Exact mode reads bounded same-size candidates and full-hashes them on
        // a dedicated queue; Similar mode performs its perceptual index off-main.
        //
        // COALESCE instead of cancel-and-restart: hashing and database reads are
        // not interrupted mid-file, so spawning a fresh task each scan tick would
        // pile up superseded work. If a
        // reload is already running, mark it dirty and let it re-run once when it
        // finishes — at most one in-flight + one pending. A result known-stale
        // (a newer reload was requested mid-query) is skipped, not assigned, so the
        // final assignment is always the freshest (latest-wins). (audit R3-05 delta fix)
        // R7: freeze the duplicate re-query + selection re-derivation while a
        // destructive confirmation is on screen, so the count the user is
        // confirming can't drift mid-scan. Re-runs on dialog dismissal
        // (onChange of confirmDelete), which re-applies the keeper protection.
        if confirmDelete { return }
        if reloadTask != nil { reloadPending = true; return }
        reloadTask = Task { @MainActor in
            repeat {
                reloadPending = false
                // Read the mode fresh each iteration so a mid-flight mode switch
                // (which marks reloadPending) re-queries the now-selected mode.
                let newGroups: [DuplicateGroup]
                let newExactPartial: Bool
                let newExactCandidateCount: Int
                let newExactSkipped: Int
                if isSimilar {
                    newGroups = await store.similarImageGroupsAsync()
                    newExactPartial = false
                    newExactCandidateCount = 0
                    newExactSkipped = 0
                } else {
                    let snapshot = await store.exactDuplicateSnapshotAsync()
                    newGroups = snapshot.groups
                    newExactPartial = snapshot.partial
                    newExactCandidateCount = snapshot.candidateCount
                    newExactSkipped = snapshot.skipped
                }
                // A newer reload landed while this query ran — its result is stale;
                // loop and re-query rather than assigning it.
                if reloadPending { continue }
                // Prior keeper of each group (by phash id) before we overwrite `groups`,
                // so we can tell which copies *became* the keeper across this reload.
                let priorKeepers = Set(groups.compactMap { $0.files.first?.id })
                groups = newGroups
                exactPreviewPartial = newExactPartial
                exactCandidateCount = newExactCandidateCount
                exactSkipped = newExactSkipped
                let visibleIDs = Set(newGroups.flatMap { $0.files.map(\.id) })
                selection.formIntersection(visibleIDs)
                // Selection is stored by file id, but a mid-scan re-rank can change
                // which copy is the keeper (index 0). Only drop a copy that *became*
                // the keeper this reload — a copy that was already the keeper and is
                // still selected was explicitly checked by the user ("trash the keeper
                // too", see header/CopyTile help) and must not be silently reverted on
                // every throttled batch during a live scan.
                for g in newGroups {
                    guard let keeper = g.files.first else { continue }
                    if !priorKeepers.contains(keeper.id) {
                        selection.remove(keeper.id)
                    }
                }
            } while reloadPending
            reloadTask = nil
        }
    }
}

// MARK: - Group card

private struct GroupCard: View {
    let group: DuplicateGroup
    @Binding var selection: Set<Int64>
    let onSelectAll: () -> Void
    let onSelectNone: () -> Void
    let onSelectAllExceptKeeper: () -> Void
    let onInvert: () -> Void
    let onSkip: () -> Void
    let onDeleteGroup: () -> Void

    private var selectedInGroup: Int {
        group.files.reduce(0) { $0 + (selection.contains($1.id) ? 1 : 0) }
    }

    private var selectedBytes: Int64 {
        group.files.reduce(0) { $0 + (selection.contains($1.id) ? $1.sizeBytes : 0) }
    }

    var body: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    BadgePill(label: "\(group.totalFileCount) \(group.isSimilar ? "images" : "copies")")
                    if group.isSimilar {
                        BadgePill(label: "Visually similar", color: .orange)
                            .help("Matched by perceptual hash (dHash), NOT byte-for-byte. Resizes, re-encodes, crops, and light edits land here — review each before deleting.")
                    }
                    Text(String(format: "%.1f MB total · %.1f MB if you keep 1",
                                Double(group.totalBytes) / 1_048_576,
                                Double(group.reclaimableBytes) / 1_048_576))
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                    if group.isTruncated {
                        Text("showing \(group.files.count)")
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    if selectedInGroup > 0 {
                        Text(String(format: "%d selected · %.1f MB",
                                    selectedInGroup,
                                    Double(selectedBytes) / 1_048_576))
                            .font(.caption.monospaced())
                            .foregroundStyle(.red)
                    }
                }

                HStack(spacing: 6) {
                    Menu {
                        Button(group.isTruncated ? "All shown except keeper" : "All except keeper") {
                            onSelectAllExceptKeeper()
                        }
                        Button(group.isTruncated ? "All shown" : "All") { onSelectAll() }
                        Button("None") { onSelectNone() }
                        Button("Invert") { onInvert() }
                    } label: {
                        Label("Select…", systemImage: "checkmark.circle")
                            .font(.caption)
                    }
                    .menuStyle(.borderlessButton)
                    .frame(width: 100)

                    Button {
                        onDeleteGroup()
                    } label: {
                        Label("Delete \(selectedInGroup) from this group",
                              systemImage: "trash")
                            .font(.caption)
                            .foregroundStyle(selectedInGroup > 0 ? .red : .secondary)
                    }
                    .buttonStyle(.bordered)
                    .disabled(selectedInGroup == 0)

                    Button {
                        onSkip()
                    } label: {
                        Label("Skip group", systemImage: "eye.slash")
                            .font(.caption)
                    }
                    .buttonStyle(.bordered)
                    .help("Hide this group — useful for false positives.")

                    Spacer()
                }

                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 12) {
                        ForEach(Array(group.files.enumerated()), id: \.element.id) { idx, file in
                            CopyTile(
                                file: file,
                                isKeeper: idx == 0,
                                isSelected: selection.contains(file.id),
                                onToggle: {
                                    if selection.contains(file.id) {
                                        selection.remove(file.id)
                                    } else {
                                        selection.insert(file.id)
                                    }
                                }
                            )
                        }
                    }
                }
            }
        }
    }
}

private struct CopyTile: View {
    let file: FileRow
    let isKeeper: Bool
    let isSelected: Bool
    let onToggle: () -> Void
    @State private var thumb: NSImage?
    @State private var hovering = false

    private var borderColor: Color {
        if isSelected { return .red }
        if isKeeper   { return .green }
        return Color.white.opacity(0.10)
    }

    private var borderWidth: CGFloat {
        isSelected || isKeeper ? 2 : 1
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Color.white.opacity(0.04)
                .frame(width: 132, height: 132)
                .overlay(thumbContent)
                .clipShape(RoundedRectangle(cornerRadius: 10))
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(borderColor, lineWidth: borderWidth)
                )
                .overlay(badgeOverlay)
                .scaleEffect(hovering ? 1.02 : 1.0)
                .animation(.easeInOut(duration: 0.12), value: hovering)
                .onHover { hovering = $0 }
                .contentShape(Rectangle())
                .onTapGesture { onToggle() }
                .contextMenu {
                    Button("Reveal in Finder") {
                        NSWorkspace.shared.activateFileViewerSelecting([file.url])
                    }
                    Button("Quick Look") {
                        NSWorkspace.shared.open(file.url)
                    }
                }
            Text(file.url.lastPathComponent)
                .font(.system(size: 10, weight: .medium))
                .lineLimit(1).truncationMode(.middle)
                .frame(width: 132, alignment: .center)
            HStack(spacing: 4) {
                Text(String(format: "%.1f MB", file.sizeMB))
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Spacer()
                if let date = file.displayDate {
                    Text(date.formatted(date: .numeric, time: .omitted))
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            .frame(width: 132)
        }
        .task(id: file.id) { thumb = await ThumbnailService.shared.thumbnail(for: file.url, size: 264) }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityDescription)
        .accessibilityAddTraits([.isButton, isSelected ? .isSelected : []])
        .accessibilityHint(isSelected
            ? "Selected. Will be moved to Trash on Delete. Tap to deselect."
            : isKeeper
                ? "Recommended copy to keep. Tap to override and select for deletion instead."
                : "Tap to select for moving to Trash.")
    }

    private var accessibilityDescription: String {
        let mb = String(format: "%.1f megabytes", file.sizeMB)
        let role = isKeeper ? "Keeper. " : ""
        return "\(role)\(file.url.lastPathComponent), \(mb)"
    }

    @ViewBuilder
    private var thumbContent: some View {
        if let thumb {
            Image(nsImage: thumb).resizable().scaledToFill()
        } else {
            Image(systemName: "photo").foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private var badgeOverlay: some View {
        ZStack(alignment: .topLeading) {
            if isKeeper {
                BadgePill(label: "KEEPER", color: .green)
                    .padding(6)
                    .help("This is the copy we recommend you keep — usually the largest / highest-resolution one. The other copies in this group are duplicates of it. You can override by clicking another tile to make it the keeper instead.")
            }
            // Top-right checkbox.
            Button(action: onToggle) {
                Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 22))
                    .foregroundStyle(isSelected ? Color.red : Color.white.opacity(0.85))
                    .background(Circle().fill(.black.opacity(0.4)))
            }
            .buttonStyle(.plain)
            .padding(6)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
        }
    }
}
