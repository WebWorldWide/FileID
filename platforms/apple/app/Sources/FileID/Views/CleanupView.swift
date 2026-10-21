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
    /// twice (the 2nd fails "already in Trash" → a confusing "1 failed").
    @State private var deleting = false
    @State private var confirmDelete: Bool = false

    /// Initialized lazily on first reload to non-keepers per group.
    @State private var selection: Set<Int64> = []
    @State private var skippedGroups: Set<Int64> = []
    @State private var reloadTask: Task<Void, Never>?
    @State private var reloadPending = false

    /// Mirrors LibraryView: true while the engine is discovering, tagging, or
    /// post-processing. Gates the exact-mode live SHA-256 re-verify off the ~1 Hz
    /// scan batch ticks (see the batch handler). (reload storm)
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

    private var totalSelectedSize: String {
        let bytes = visibleGroups.reduce(Int64(0)) { acc, g in
            acc + g.files.reduce(Int64(0)) { $0 + (selection.contains($1.id) ? $1.sizeBytes : 0) }
        }
        return ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
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
                store.notifyChanged()
                // Exact mode's reload() runs a live full-file SHA-256 pass over up
                // to ExactDuplicateVerifier.candidateCap candidates; the digest
                // cache lives only 30 s, so re-verifying on every ~1 Hz scan batch
                // keeps the whole candidate set hot and contends with the engine's
                // scan reads on the same drive for the entire scan. Defer the exact
                // re-verify to scan completion (the lastTerminalEventAt handler
                // below). Similar mode's perceptual index is cheap and drive-light,
                // so it keeps refreshing live. (reload storm)
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
            MetaText(headerSubtitle, color: Theme.text2)
                .lineLimit(2)
                .layoutPriority(-1)
            SegmentedBar(selection: $mode, options: [("exact", "Exact copies"), ("similar", "Look-alikes")])
            .help("Exact copies are the same file twice. Look-alikes are photos that look nearly the same (resized, cropped or lightly edited) but are not identical, so check each one.")
            Spacer()
            if !visibleGroups.isEmpty {
                HStack(spacing: 6) {
                    // Bulk "select every non-keeper" is hidden in Similar mode: those
                    // copies are NOT byte-identical, so one-click mass selection would
                    // be unsafe — the user must pick copies deliberately per group.
                    if !isSimilar {
                        Button("Select all extra copies") { selectAllNonKeepers() }
                            .buttonStyle(.dsSecondary)
                            .fixedSize()
                            .help("Choose every copy except the one marked KEEP in each group")
                    }
                    Button("Clear selection") { selection.removeAll() }
                        .buttonStyle(.dsSecondary)
                        .fixedSize()
                        .disabled(totalSelected == 0)
                        .help(totalSelected == 0 ? "Nothing is chosen" : "Unchoose every file")
                }
                Button {
                    confirmDeleteSelected()
                } label: {
                    Label(
                        totalSelected == 0 ? "Move to Trash" : "Move \(totalSelected) to Trash · \(totalSelectedSize)",
                        systemImage: "trash"
                    )
                    .fontWeight(.semibold)
                    .foregroundStyle(Theme.danger)
                    .fixedSize()
                    .padding(.horizontal, 14)
                    .frame(minHeight: Theme.Metrics.hitTarget)
                    .background(Theme.danger.opacity(0.12), ignoresSafeAreaEdges: [])
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                            .stroke(Theme.danger.opacity(0.5), lineWidth: 1)
                    )
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
                }
                .buttonStyle(.plain)
                .disabled(totalSelected == 0)
                .help(totalSelected == 0 ? "Choose the copies to remove first: click their pictures, or use Select all extra copies." : "Move the chosen copies to Trash. You can put them back from Trash.")
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
        .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, 14)
    }

    private var headerSubtitle: String {
        let g = visibleGroups.count
        let skipped = skippedGroups.count
        let base: String
        if isSimilar {
            base = g == 0
                ? "Photos that look nearly the same but aren't identical"
                : "\(g) group\(g == 1 ? "" : "s") of look-alikes · check each before removing"
        } else {
            let bytes = visibleGroups.reduce(Int64(0)) { $0 + $1.reclaimableBytes }
            base = g == 0
                ? "No extra copies found"
                : "\(g.formatted()) file\(g == 1 ? "" : "s") with extra copies · \(ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)) can be freed"
        }
        var result = skipped > 0 ? "\(base) · \(skipped) skipped" : base
        if !isSimilar && exactPreviewPartial {
            result += " · more may exist"
            if exactSkipped > 0 { result += " · \(exactSkipped) couldn't be checked" }
        }
        return result
    }

    // MARK: - Empty / list

    @ViewBuilder
    private var empty: some View {
        if store.totalFiles == 0 {
            EmptyStateView(
                icon: "trash.slash",
                title: "No files yet",
                message: "Scan a folder first. Extra copies of the same file then show up here in groups, and you choose which copy to keep."
            )
        } else if !skippedGroups.isEmpty {
            VStack(spacing: 14) {
                EmptyStateView(
                    icon: "checkmark.seal.fill",
                    title: "Every group is hidden",
                    message: "You skipped all the groups. Show them again to keep going."
                )
                Button("Show skipped groups again") { skippedGroups.removeAll() }
                    .buttonStyle(.dsSecondary)
            }
        } else if !isSimilar && exactPreviewPartial {
            EmptyStateView(
                icon: "exclamationmark.magnifyingglass",
                title: "No extra copies found so far",
                message: "FileID checked part of this library (\(exactCandidateCount) files of the same size) and found no exact copies. Rescan later to check the rest."
            )
        } else if isSimilar {
            EmptyStateView(
                icon: "checkmark.seal.fill",
                title: "No look-alikes",
                message: "None of the \(store.totalImages.formatted()) photos look nearly the same as another. Exact copies are under Exact copies."
            )
        } else {
            EmptyStateView(
                icon: "checkmark.seal.fill",
                title: "No extra copies",
                message: "None of the \(store.totalFiles.formatted()) files is an exact copy of another. Try Look-alikes for photos that are nearly the same."
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
                            .foregroundStyle(Theme.warning)
                        Text("**Look-alikes are not identical.** They may be resized, cropped or edited versions. Nothing is chosen for you: check each group, then click the copies you want to remove.")
                            .font(Theme.Font.body(12))
                            .foregroundStyle(Theme.text3)
                    }
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.warning.opacity(0.10)))
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.warning.opacity(0.35), lineWidth: 1))
                    .padding(.bottom, 4)
                } else {
                    HStack(spacing: 8) {
                        Image(systemName: exactPreviewPartial
                              ? "exclamationmark.triangle.fill" : "info.circle")
                            .foregroundStyle(exactPreviewPartial ? Theme.warning : Theme.delight)
                        Text(exactPreviewPartial
                             ? "**Some files weren't checked yet.** Every group below is an exact copy, checked byte by byte, but more copies may exist. Rescan later to check the rest."
                             : "Each group is the same file saved more than once, checked byte by byte. FileID marks one copy **KEEP**; click any other copy to choose it, then press Move to Trash. You can put files back from Trash.")
                            .font(Theme.Font.body(12))
                            .foregroundStyle(Theme.text3)
                    }
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(
                        (exactPreviewPartial ? Theme.warning : Theme.delight).opacity(0.08)))
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(
                        (exactPreviewPartial ? Theme.warning : Theme.delight).opacity(0.3),
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
                            .foregroundStyle(statusWarning ? Theme.warning : Theme.delight)
                        Text(s)
                            .font(Theme.Font.body(13.5))
                        Spacer()
                        Button("Open Trash") {
                            NSWorkspace.shared.open(
                                URL(fileURLWithPath: NSHomeDirectory())
                                    .appendingPathComponent(".Trash")
                            )
                        }
                        .buttonStyle(.dsSecondary)
                        Button("Dismiss") {
                            status = nil
                            statusWarning = false
                        }
                            .buttonStyle(.borderless)
                    }
                    .padding(12)
                    .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(
                        (statusWarning ? Theme.warning : Theme.delight).opacity(0.10)))
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(
                        (statusWarning ? Theme.warning : Theme.delight).opacity(0.4),
                        lineWidth: 1))
                    .help("To undo, open Trash in Finder, right-click a file and choose Put Back.")
                }
            }
            .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, Theme.Metrics.pageInsetY)
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
        let mb = totalSelectedSize
        return "Moves the selected copies to Trash. Frees about \(mb). You can restore them from Trash if you change your mind."
    }

    private func confirmDeleteSelected() {
        confirmDelete = true
    }

    private func trashSelectedInGroup(_ g: DuplicateGroup) async {
        await trashSelected(across: [g])
    }

    private func trashSelected(across groupsToScan: [DuplicateGroup]) async {
        guard !deleting else { return }   // re-entrancy guard
        deleting = true
        statusWarning = false
        defer { deleting = false }
        // Build the work list first on the main actor so SwiftUI selection
        // reads stay off the concurrent trash path.
        var work: [DuplicateTrashItem] = []
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
                work.append(DuplicateTrashItem(
                    id: file.id, url: file.url, size: file.sizeBytes,
                    exactKeeper: exactKeeper))
                keeperTagsByVictim[file.id] = kept.map(\.url)
            }
        }

        // Trash up to 8 files concurrently. Foundation's trashItem isn't
        // thread-hostile (Finder serializes journaling underneath), but
        // doing them sequentially on a 10K dedup pass = 10–50 s freeze.
        let results = await ExactDuplicateVerifier.trash(work)

        var trashedIDs: [Int64] = []
        var freedBytes: Int64 = 0
        verificationRejected += results.filter(\.verificationFailed).count
        let trashFailures = results.filter { !$0.success && !$0.verificationFailed }.count
        for result in results where result.success {
            trashedIDs.append(result.id)
            freedBytes += result.size
        }
        let freed = ByteCountFormatter.string(fromByteCount: freedBytes, countStyle: .file)
        var keeperURLSet = Set<URL>()
        for id in trashedIDs {
            for url in keeperTagsByVictim[id] ?? [] { keeperURLSet.insert(url) }
        }
        let keeperURLsToTag = Array(keeperURLSet)

        // auto-tag keepers (Settings toggle, default on). Useful so
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
            tagSummary = " · tagged \(taggedAdded) kept file\(taggedAdded == 1 ? "" : "s") with \"\(AppSettings.cleanupAutoTagName)\""
        }
        var summary = "Moved \(trashedIDs.count) file\(trashedIDs.count == 1 ? "" : "s")"
            + " to Trash · freed \(freed)"
            + tagSummary
        if verificationRejected > 0 {
            summary += " · \(verificationRejected) skipped because the file changed since the scan"
        }
        if trashFailures > 0 {
            summary += " · \(trashFailures) couldn't be moved to Trash"
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
        // final assignment is always the freshest (latest-wins). (05 delta fix)
        // Freeze the duplicate re-query + selection re-derivation while a
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
        Panel {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 10) {
                    SectionLabel(group.isSimilar ? "Look-alikes · \(group.totalFileCount) photos" : "Same file · \(group.totalFileCount) copies")
                    if group.isSimilar {
                        BadgePill(label: "perceptual match", color: Theme.warning)
                            .help("These look nearly the same but are not identical. Check each before removing.")
                    }
                    Text("\(ByteCountFormatter.string(fromByteCount: group.totalBytes, countStyle: .file)) total · \(ByteCountFormatter.string(fromByteCount: group.reclaimableBytes, countStyle: .file)) freed if you keep one")
                        .font(Theme.Font.body(13)).foregroundStyle(Theme.text2)
                    if group.isTruncated { MetaText("showing \(group.files.count)", size: 11) }
                    Spacer()
                    if selectedInGroup > 0 {
                        MetaText("\(selectedInGroup) selected · \(ByteCountFormatter.string(fromByteCount: selectedBytes, countStyle: .file))", color: Theme.danger, size: 11)
                    }
                }
                .padding(.horizontal, 14).padding(.top, 10)

                HStack(spacing: 6) {
                    Menu {
                        Button(group.isTruncated ? "All shown except KEEP" : "All except KEEP") {
                            onSelectAllExceptKeeper()
                        }
                        Button(group.isTruncated ? "All shown" : "All") { onSelectAll() }
                        Button("None") { onSelectNone() }
                        Button("Invert") { onInvert() }
                    } label: {
                        Label("Choose…", systemImage: "checkmark.circle")
                            .font(Theme.Font.body(12))
                    }
                    .menuStyle(.borderlessButton)
                    .frame(width: 100)

                    Button { onDeleteGroup() } label: { Label(selectedInGroup == 0 ? "Move to Trash" : "Move \(selectedInGroup) to Trash", systemImage: "trash") }
                    .buttonStyle(selectedInGroup > 0 ? AnyButtonStyle(.dsDanger) : AnyButtonStyle(DSSecondaryButtonStyle(height: 26)))
                    .disabled(selectedInGroup == 0)
                    .help(selectedInGroup == 0 ? "Click the copies you want to remove first" : "Move the chosen copies in this group to Trash")

                    Button { onSkip() } label: { Label("Skip group", systemImage: "eye.slash") }
                    .buttonStyle(DSSecondaryButtonStyle(height: 26))
                    .help("Hide this group if these aren't really copies. Nothing is deleted.")

                    Spacer()
                }
                .padding(.horizontal, 14)

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
                    .padding(.horizontal, 14)
                }
                .padding(.bottom, 12)
            }
        }
    }
}

private struct CopyTile: View {
    let file: FileRow
    let isKeeper: Bool
    let isSelected: Bool
    let onToggle: () -> Void
    @State private var hovering = false

    private var borderColor: Color {
        if isSelected { return Theme.danger }
        if isKeeper   { return Theme.delight }
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
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous)
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
                Text(ByteCountFormatter.string(fromByteCount: file.sizeBytes, countStyle: .file))
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(Theme.text3)
                Spacer()
                if let date = file.displayDate {
                    Text(date.formatted(date: .numeric, time: .omitted))
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(Theme.text3)
                }
            }
            .frame(width: 132)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityDescription)
        .accessibilityAddTraits([.isButton, isSelected ? .isSelected : []])
        .accessibilityHint(isSelected
            ? "Chosen. It goes to Trash when you press Move to Trash. Click to unchoose."
            : isKeeper
                ? "FileID suggests keeping this copy. Click to choose it for Trash instead."
                : "Click to choose this copy for Trash.")
    }

    private var accessibilityDescription: String {
        let mb = ByteCountFormatter.string(fromByteCount: file.sizeBytes, countStyle: .file)
        let role = isKeeper ? "Keeper. " : ""
        return "\(role)\(file.url.lastPathComponent), \(mb)"
    }

    private var thumbContent: some View {
        AsyncThumbnail(url: file.url, size: 264) { Image(systemName: "photo").foregroundStyle(Theme.text3) }
    }

    @ViewBuilder
    private var badgeOverlay: some View {
        ZStack(alignment: .topLeading) {
            if isKeeper {
                BadgePill(label: "KEEP", color: Theme.delight, onImage: true)
                    .padding(6)
                    .help("FileID suggests keeping this copy, usually the largest or sharpest one. The others are copies of it.")
            }
            // Top-right checkbox.
            Button(action: onToggle) {
                Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 22))
                    .foregroundStyle(isSelected ? Theme.danger : Color.white.opacity(0.85))
                    .background(Circle().fill(.black.opacity(0.4)))
            }
            .buttonStyle(.plain)
            .padding(6)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
        }
    }
}
