// Cleanup: Exact groups are full-file SHA-256 matches; Similar groups are dHash look-alikes, which are NOT byte-identical, so Similar never pre-selects.
import SwiftUI
import AppKit
import FileIDShared
import FileIDModelInfo

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
    /// The last Trash batch. Kept across launches: the engine's journal keeps it until
    /// it is put back, so "Put back last move" still works after a restart or crash.
    @AppStorage("cleanup.lastTrashBatch") private var undoBatchStored = ""
    private var undoBatch: String? { undoBatchStored.isEmpty ? nil : undoBatchStored }
    @State private var exactPreviewPartial = false
    @State private var exactCandidateCount = 0
    @State private var exactSkipped = 0
    @State private var exactCapped = false
    @State private var exactUnshownClasses = 0
    @State private var exactUnshownBytes: Int64 = 0
    @State private var exactTotals: ExactDuplicateVerifier.Totals?
    @State private var exactBase: ExactDuplicateVerifier.Totals?
    @State private var exactStored: [DuplicateGroup] = []
    @State private var exactSettled: [DuplicateGroup] = []
    @State private var exactBatch: [DuplicateGroup] = []
    @State private var exactQueued: [DuplicateGroup] = []
    @State private var exactSettledUnverified: Set<Int64> = []
    @State private var exactSettledSkipped = 0
    @State private var exactLoadingMore = false
    @State private var exactVerify: (done: Int, total: Int)?
    @State private var exactUnverified: Set<Int64> = []
    @State private var verifyTask: Task<Void, Never>?
    /// Single-flight guard: a rapid double-tap on the confirmation would
    /// otherwise trash the same files twice (the 2nd fails "already in Trash").
    @State private var deleting = false
    @State private var confirmDelete: Bool = false
    /// The one group a per-group "Move to Trash" button asked about; nil means the header button (every visible group).
    @State private var confirmGroup: DuplicateGroup?

    /// Initialized lazily on first reload to non-keepers per group.
    @State private var selection: Set<Int64> = []
    @State private var skippedGroups: Set<Int64> = CleanupSession.skipped["exact"] ?? []
    @State private var reloadTask: Task<Void, Never>?
    @State private var exactChecking = true
    @State private var lastScanTimeReload = Date.distantPast
    @State private var reloadPending = false
    @Environment(\.pageActive) private var pageActive
    /// Files changed, or a comparison was cut short, while another page was shown.
    @State private var staleWhileHidden = false

    /// Mirrors LibraryView: true while the engine is discovering, tagging, or post-processing. Gates the exact-mode live SHA-256 re-verify off the ~1 Hz scan batch ticks.
    private var scanActive: Bool {
        guard let p = engine.lastProgress else { return false }
        return p.phase == .discovering || p.phase == .tagging || p.phase == .postScan
    }

    private var visibleGroups: [DuplicateGroup] {
        skippedGroups.isEmpty ? groups : groups.filter { !skippedGroups.contains($0.id) }
    }

    /// Counted once per render: the header reads these several times, and counting walks every file of every group.
    private struct SelectionTotals {
        var count = 0
        var bytes: Int64 = 0
        var size: String { ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file) }
    }

    private func selectionTotals(in visible: [DuplicateGroup]) -> SelectionTotals {
        var totals = SelectionTotals()
        guard !selection.isEmpty else { return totals }
        for group in visible {
            for file in group.files where selection.contains(file.id) {
                totals.count += 1
                totals.bytes += file.sizeBytes
            }
        }
        return totals
    }

    private var totalSelectedSize: String { selectionTotals(in: visibleGroups).size }

    var body: some View {
        let visible = visibleGroups
        VStack(spacing: 0) {
            header(visible: visible, totals: selectionTotals(in: visible))
            if let note = Self.unreadableNote(store.unreadableFiles, timedOut: store.unreadableTimedOutFiles) {
                MetaText(note, color: Theme.text2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.bottom, 10)
            }
            Hairline()
            if visible.isEmpty {
                if status != nil {
                    statusBanner
                        .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, Theme.Metrics.pageInsetY)
                }
                empty
            } else {
                list(visible: visible)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .onAppear {
            store.openIfPossible()
            reload()
        }
        .onChange(of: engine.lastBatch?.batchIndex ?? -1) { _, _ in
            if pageActive { scanBatchArrived() } else { staleWhileHidden = true }
        }
        .onDisappear { reloadTask?.cancel(); verifyTask?.cancel(); exactVerify = nil }
        .onReceive(NotificationCenter.default.publisher(for: .selfShotAction)) { note in
            guard let action = note.object as? String, action.hasPrefix("more:"), let pages = Int(action.dropFirst(5)) else { return }
            Task { @MainActor in
                for _ in 0..<pages { await loadMoreExactPage() }
            }
        }
        // Exact mode skips its per-batch reload during a live scan, so the final
        // verified set — and the last batch the scan-active skip drops — is computed
        // once here when the scan reaches a terminal phase. Mirrors LibraryView's
        // terminal reload; single-flight reload() coalesces if one is in flight.
        .onChange(of: engine.lastTerminalEventAt) { _, _ in
            guard pageActive else { staleWhileHidden = true; return }
            store.notifyChanged()
            reload()
        }
        // Leaving stops an unfinished comparison; coming back re-runs it only if it was cut short or files changed.
        .onPageVisibility(shown: {
            guard staleWhileHidden else { return }
            staleWhileHidden = false
            lastSeenBatchIndex = engine.lastBatch?.batchIndex ?? -1
            store.notifyChanged()
            reload()
        }, hidden: {
            verifyTask?.cancel()
            if exactVerify != nil { exactVerify = nil; staleWhileHidden = true }
            if reloadTask != nil {
                reloadTask?.cancel()
                reloadPending = false
                staleWhileHidden = true
            }
        })
        .onChange(of: confirmDelete) { _, presented in
            // Dialog dismissed (confirm, cancel, or click-away): apply any scan
            // batches we deferred while it was open, re-deriving keepers.
            if !presented { reload() }
        }
        .onChange(of: skippedGroups) { _, new in CleanupSession.skipped[mode] = new }
        .onChange(of: mode) { _, _ in
            // Switching modes starts from a clean slate — nothing carries over,
            // and Similar mode must begin with NOTHING pre-selected for deletion.
            selection.removeAll()
            skippedGroups = CleanupSession.skipped[mode] ?? []
            status = nil
            statusWarning = false
            exactPreviewPartial = false
            exactCandidateCount = 0
            exactSkipped = 0
            exactCapped = false
            exactUnshownClasses = 0; exactUnshownBytes = 0
            verifyTask?.cancel()
            exactTotals = nil; exactBase = nil; exactVerify = nil; exactUnverified = []
            exactStored = []; exactSettled = []; exactBatch = []; exactQueued = []
            groups = []
            reload()
        }
    }

    // MARK: - Header

    @ViewBuilder
    private func header(visible: [DuplicateGroup], totals: SelectionTotals) -> some View {
        let totalSelected = totals.count
        let subtitle = MetaText(headerSubtitle(visible: visible), color: Theme.text2)
        let controls = Group {
            SegmentedBar(selection: $mode, options: [("exact", "Exact copies"), ("similar", "Look-alikes")])
            .help("Exact copies are the same file twice. Look-alikes are photos that look nearly the same (resized, cropped or lightly edited) but are not identical, so check each one.")
            if let batch = undoBatch, status == nil {
                Button { Task { await undoTrash(batch) } } label: { Label("Put back last move", systemImage: "arrow.uturn.backward") }
                    .buttonStyle(.dsSecondary)
                    .fixedSize()
                    .disabled(deleting)
                    .help("Puts the copies FileID last moved to Trash back where they were")
            }
            if !visible.isEmpty {
                HStack(spacing: 6) {
                    // Bulk "select every non-keeper" is hidden in Similar mode: those
                    // copies are NOT byte-identical, so one-click mass selection would
                    // be unsafe — the user must pick copies deliberately per group.
                    if !isSimilar {
                        Button("Select all extra copies") { selectAllNonKeepers() }
                            .buttonStyle(.dsSecondary)
                            .fixedSize()
                            .disabled(engine.libraryReadOnlyReason != nil)
                            .accessibilityHint(engine.libraryReadOnlyReason ?? "")
                            .help(engine.libraryReadOnlyReason ?? "Choose every copy except the one marked KEEP in each group")
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
                        totalSelected == 0 ? "Move to Trash" : "Move \(totalSelected) to Trash · \(totals.size)",
                        systemImage: "trash"
                    )
                    .fixedSize()
                }
                .buttonStyle(DSDangerButtonStyle(height: Theme.Metrics.control))
                .disabled(totalSelected == 0 || trashReadOnlyReason != nil)
                .accessibilityHint(trashReadOnlyReason ?? "")
                .help(trashReadOnlyReason ?? (totalSelected == 0 ? "Choose the copies to remove first: click their pictures, or use Select all extra copies." : "Move the chosen copies to Trash. You can put them back afterwards."))
                .confirmationDialog(
                    "Move \(confirmCount) file\(confirmCount == 1 ? "" : "s") to Trash?",
                    isPresented: $confirmDelete,
                    titleVisibility: .visible
                ) {
                    Button("Move to Trash", role: .destructive) {
                        let targets = confirmGroup.map { [$0] } ?? visibleGroups
                        confirmGroup = nil
                        Task { await trashSelected(across: targets) }
                    }
                    Button("Cancel", role: .cancel) { confirmGroup = nil }
                } message: {
                    Text(confirmDeleteMessage)
                }
            }
        }
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .center, spacing: 12) { subtitle.fixedSize(); Spacer(); controls }
            VStack(alignment: .leading, spacing: 8) {
                subtitle.fixedSize(horizontal: false, vertical: true)
                FlowLayout(spacing: 8) { controls }
            }
        }
        .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, 14)
    }

    enum ExactEmptyPhase: Equatable { case checking, partial, none }

    /// An empty list says "no copies" only after a check has finished; while one runs it says so.
    static func exactEmptyPhase(checking: Bool, partial: Bool) -> ExactEmptyPhase {
        if checking { return .checking }
        return partial ? .partial : .none
    }

    static func exactSummary(groups: Int, extraCopies: Int, bytes: Int64) -> String {
        if groups == 0 { return "No extra copies found" }
        let freed = ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
        return "\(extraCopies.formatted()) extra cop\(extraCopies == 1 ? "y" : "ies") in \(groups.formatted()) group\(groups == 1 ? "" : "s") · \(freed) can be freed"
    }

    /// Files that failed to read are in no group; Library's Unreadable chip counts the same rows. "Yet" only for the ones a later scan can still read.
    static func unreadableNote(_ count: Int, timedOut: Int = 0) -> String? {
        guard count > 0 else { return nil }
        let noun = count == 1 ? "1 file" : "\(count.formatted()) files"
        let them = count == 1 ? "it’s" : "they’re"
        let timedOut = min(max(timedOut, 0), count)
        if timedOut == 0 { return "\(noun) couldn’t be read, so \(them) not in these results" }
        if timedOut == count { return "\(noun) couldn’t be read yet, so \(them) not in these results" }
        return "\(noun) couldn’t be read, so \(them) not in these results. \(timedOut.formatted()) of them ran out of time and may read next time"
    }

    private func headerSubtitle(visible: [DuplicateGroup]) -> String {
        if store.totalFiles == 0 { return "Nothing scanned yet" }
        let g = visible.count
        let skipped = skippedGroups.count
        let base: String
        if isSimilar {
            base = g == 0
                ? "Photos that look nearly the same but aren’t identical"
                : "\(g) group\(g == 1 ? "" : "s") of look-alikes · check each before removing"
        } else {
            let bytes = visible.reduce(Int64(0)) { $0 + $1.reclaimableBytes }
            let extraCopies = visible.reduce(0) { $0 + max(0, $1.totalFileCount - 1) }
            if let totals = exactTotals {
                let hidden = groups.filter { skippedGroups.contains($0.id) }
                base = Self.exactSummary(
                    groups: max(0, totals.groups - hidden.count),
                    extraCopies: max(0, totals.extra - hidden.reduce(0) { $0 + max(0, $1.totalFileCount - 1) }),
                    bytes: max(0, totals.bytes - hidden.reduce(Int64(0)) { $0 + $1.reclaimableBytes }))
            } else {
                base = Self.exactSummary(groups: g, extraCopies: extraCopies, bytes: bytes)
            }
        }
        var result = skipped > 0 ? "\(base) · \(skipped) skipped" : base
        if !isSimilar, let verify = exactVerify {
            result += " · checking byte by byte (\(verify.done.formatted()) of \(verify.total.formatted()) groups)"
        }
        if !isSimilar && exactPreviewPartial {
            if exactCapped {
                result += exactTotals != nil
                    ? " · showing the biggest \(g.formatted()) of \((exactTotals?.groups ?? g).formatted()) groups"
                    : exactUnshownClasses > 0
                    ? " · \(exactUnshownClasses.formatted()) more sets not listed (up to \(ByteCountFormatter.string(fromByteCount: exactUnshownBytes, countStyle: .file)))"
                    : " · more may exist"
            }
            if exactSkipped > 0 { result += " · \(exactSkipped.formatted()) couldn’t be checked" }
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
            EmptyStateView(
                icon: "checkmark.seal.fill",
                title: "Every group is hidden",
                message: "You skipped all the groups. Show them again to keep going.",
                primaryAction: (label: "Show skipped groups again", run: { skippedGroups.removeAll() })
            )
        } else if !isSimilar && Self.exactEmptyPhase(checking: exactChecking, partial: exactPreviewPartial) == .checking {
            EmptyStateView(
                icon: "magnifyingglass",
                title: "Looking for extra copies",
                message: "FileID compares the files byte by byte, so this can take a few minutes on a big library or an external drive.",
                progressLabel: "Checking files"
            )
        } else if !isSimilar && Self.exactEmptyPhase(checking: exactChecking, partial: exactPreviewPartial) == .partial {
            EmptyStateView(
                icon: "exclamationmark.magnifyingglass",
                title: "No extra copies found so far",
                message: exactCapped
                    ? "FileID compared the most likely files and found no exact copies. It checks a limited number at a time, so a very large library may still hold some."
                    : "No exact copies among the files FileID could read. \(exactSkipped.formatted()) file\(exactSkipped == 1 ? "" : "s") couldn’t be opened, for example because a drive is disconnected."
            )
        } else if isSimilar {
            EmptyStateView(
                icon: "checkmark.seal.fill",
                title: "No look-alikes",
                message: "None of the \(Plural.noun(store.totalImages, "photo", "photos")) look nearly the same as another. Exact copies are under Exact copies."
            )
        } else {
            EmptyStateView(
                icon: "checkmark.seal.fill",
                title: "No extra copies",
                message: "None of the \(Plural.noun(store.totalFiles, "file", "files")) is an exact copy of another. Try Look-alikes for photos that are nearly the same."
            )
        }
    }

    /// The result of the last Move to Trash, with its Undo. Shown above the empty page too: trashing the last group leaves no list to carry it.
    @ViewBuilder
    private var statusBanner: some View {
        if let s = status {
            HStack(spacing: 10) {
                Image(systemName: statusWarning
                      ? "exclamationmark.triangle.fill" : "trash.fill")
                    .foregroundStyle(statusWarning ? Theme.warning : Theme.delight)
                Text(s)
                    .font(Theme.Font.body(13.5))
                Spacer()
                if let batch = undoBatch {
                    Button("Undo") { Task { await undoTrash(batch) } }
                        .buttonStyle(.dsSecondary)
                        .disabled(deleting)
                }
                Button("Open Trash") {
                    NSWorkspace.shared.open(UserHome.trashURL)
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
            .help(undoBatch == nil ? "To put a file back, open Trash in Finder, right-click it and choose Put Back." : "Undo puts the moved copies back where they were.")
        }
    }

    @ViewBuilder
    private func list(visible: [DuplicateGroup]) -> some View {
        ScrollView(.vertical) {
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
                    .frame(maxWidth: .infinity, alignment: .leading)
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
                             ? partialNotice(shown: visible.count)
                             : "Each group is the same file saved more than once, checked byte by byte. FileID marks one copy **KEEP**; click any other copy to choose it, then press Move to Trash. You can undo it right after.")
                            .font(Theme.Font.body(12))
                            .foregroundStyle(Theme.text3)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(
                        (exactPreviewPartial ? Theme.warning : Theme.delight).opacity(0.08)))
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(
                        (exactPreviewPartial ? Theme.warning : Theme.delight).opacity(0.3),
                        lineWidth: 1))
                    .padding(.bottom, 4)
                }
                ForEach(visible) { group in
                    GroupCard(
                        group: group,
                        selection: $selection,
                        onSelectAll: { setGroup(group, allSelected: true) },
                        onSelectNone: { setGroup(group, allSelected: false) },
                        onSelectAllExceptKeeper: { setGroupNonKeepers(group) },
                        onInvert: { invertGroup(group) },
                        onSkip: { skippedGroups.insert(group.id) },
                        onDeleteGroup: { confirmGroup = group; confirmDelete = true },
                        gate: engine.readOnlyGate
                    )
                }
                if !isSimilar, exactCapped, let totals = exactTotals {
                    showMoreFooter(totals: totals)
                }
                statusBanner
            }
            .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, Theme.Metrics.pageInsetY)
        }
        .hoverPausedWhileScrolling()
    }

    @ViewBuilder
    private func showMoreFooter(totals: ExactDuplicateVerifier.Totals) -> some View {
        let remaining = max(0, totals.groups - groups.count)
        let next = min(ExactDuplicateVerifier.groupCap, remaining)
        let freed = max(0, totals.bytes - groups.reduce(Int64(0)) { $0 + $1.reclaimableBytes })
        if remaining > 0 {
            HStack(spacing: 12) {
                Button(exactLoadingMore ? "Loading…" : "Show \(next.formatted()) more groups") { loadMoreExact() }
                    .buttonStyle(.dsSecondary)
                    .disabled(exactLoadingMore)
                MetaText("\(groups.count.formatted()) of \(totals.groups.formatted()) groups shown · up to \(ByteCountFormatter.string(fromByteCount: freed, countStyle: .file)) more can be freed", size: 12)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
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
        // Copies on a read-only drive can't be moved, so they are never offered for the Trash.
        let gate = engine.readOnlyGate
        if !gate.entries.isEmpty {
            let locked = Set(visibleGroups.flatMap(\.files).filter { gate.drive(for: $0.url) != nil }.map(\.id))
            selection.subtract(locked)
        }
    }

    /// The reason the Trash button is off when every chosen copy sits on a read-only drive.
    private var trashReadOnlyReason: String? {
        guard !selection.isEmpty else { return nil }
        let gate = engine.readOnlyGate
        guard !gate.entries.isEmpty else { return nil }
        let chosen = visibleGroups.flatMap(\.files).filter { selection.contains($0.id) }.map(\.url)
        return gate.reasonIfAll(chosen)
    }

    // MARK: - Trash actions

    private var confirmDeleteMessage: String {
        let bytes = confirmGroup.map { group in
            group.files.reduce(Int64(0)) { $0 + (selection.contains($1.id) ? $1.sizeBytes : 0) }
        }
        let size = bytes.map { ByteCountFormatter.string(fromByteCount: $0, countStyle: .file) } ?? totalSelectedSize
        var message = "Moves the selected \(isSimilar ? "photos" : "copies") to Trash. Frees about \(size). You can undo it right after, or put them back from the Trash later."
        if autoTagOn {
            message += " Kept files get the Finder tag “\(AppSettings.cleanupAutoTagName)”. You can turn that off in Settings."
        }
        return message
    }

    private var autoTagOn: Bool {
        UserDefaults.standard.object(forKey: AppSettings.cleanupAutoTagKey) == nil
            ? AppSettings.cleanupAutoTagDefault
            : UserDefaults.standard.bool(forKey: AppSettings.cleanupAutoTagKey)
    }

    private func partialNotice(shown: Int) -> LocalizedStringKey {
        let unread = exactSkipped > 0
            ? " \(exactSkipped.formatted()) file\(exactSkipped == 1 ? "" : "s") couldn’t be opened, so \(exactSkipped == 1 ? "it wasn’t" : "they weren’t") checked."
            : ""
        if exactVerify != nil || !exactUnverified.isEmpty {
            let pending = exactVerify != nil
                ? "**Checking the copies byte by byte.** These groups already match by their saved fingerprints; any that don’t hold up are removed when the check reaches them."
                : "**\(exactUnverified.count.formatted()) large group\(exactUnverified.count == 1 ? "" : "s") match by saved fingerprint only.** FileID reads every copy byte by byte before anything moves to Trash. The other groups are checked byte by byte."
            return LocalizedStringKey(pending + (exactCapped ? " The rest are one tap away below." : "") + unread)
        }
        if exactCapped {
            return "**Showing the biggest \(shown.formatted()) of \((exactTotals?.groups ?? shown).formatted()) groups.** Each one is an exact copy, checked byte by byte. Show more below to see the rest.\(unread)"
        }
        return "**Some files couldn’t be checked.** Every group below is an exact copy, checked byte by byte.\(unread)"
    }

    private func confirmDeleteSelected() {
        confirmGroup = nil
        confirmDelete = true
    }

    private var confirmCount: Int {
        guard let group = confirmGroup else { return selectionTotals(in: visibleGroups).count }
        return group.files.filter { selection.contains($0.id) }.count
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
        var keepOneSkipped = 0
        for group in groupsToScan {
            let trashed = group.files.filter { selection.contains($0.id) }
            let kept = group.files.filter { !selection.contains($0.id) }
            // Every group keeps at least one file, look-alikes included: the
            // tab removes extra copies, never the last version of a photo.
            if kept.isEmpty && !trashed.isEmpty {
                keepOneSkipped += trashed.count
                continue
            }
            let exactKeeper = group.isSimilar ? nil : kept.first?.url
            for file in trashed {
                work.append(DuplicateTrashItem(
                    id: file.id, url: file.url, size: file.sizeBytes,
                    exactKeeper: exactKeeper))
                keeperTagsByVictim[file.id] = kept.map(\.url)
            }
        }

        let outcome = await ExactDuplicateVerifier.trashThroughEngine(work, engine: engine)
        if outcome.engineUnavailable || outcome.timedOut {
            statusWarning = true
            status = outcome.engineUnavailable
                ? "FileID is still getting ready. Try again in a moment."
                : "Moving to Trash didn’t finish. Check the Trash before trying again."
            reload()
            return
        }
        let trashedIDs = outcome.trashed.map(\.id)
        let freedBytes = outcome.trashed.reduce(Int64(0)) { $0 + $1.size }
        verificationRejected += outcome.verificationFailed
        let trashFailures = outcome.failed
        let protectedCount = outcome.protectedLocation
        let freed = ByteCountFormatter.string(fromByteCount: freedBytes, countStyle: .file)
        var keeperURLSet = Set<URL>()
        for id in trashedIDs {
            for url in keeperTagsByVictim[id] ?? [] { keeperURLSet.insert(url) }
        }
        let keeperURLsToTag = Array(keeperURLSet)

        // auto-tag keepers (Settings toggle, default on). Useful so
        // the user can find "files I deduped this session" in Finder.
        let shouldTag = autoTagOn && !keeperURLsToTag.isEmpty

        // Run the chunked DELETE and per-keeper xattr writes off the MainActor: on a large pass they are thousands of synchronous DB/FS ops that would re-freeze the UI.
        // Resume on the MainActor only to mutate @State.
        let taggedAdded: Int = await Task.detached(priority: .userInitiated) {
            [keeperURLsToTag, shouldTag] in
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
            tagSummary = " · tagged \(Plural.noun(taggedAdded, "kept file", "kept files")) with “\(AppSettings.cleanupAutoTagName)”"
        }
        var summary = "Moved \(trashedIDs.count) file\(trashedIDs.count == 1 ? "" : "s")"
            + " to Trash · freed \(freed)"
            + tagSummary
        if keepOneSkipped > 0 {
            summary += " · \(keepOneSkipped) skipped: every file in a group was chosen, and each group keeps at least one"
        }
        if verificationRejected > 0 {
            summary += " · \(verificationRejected) skipped because the file changed since the scan"
        }
        if protectedCount > 0 {
            summary += " · " + ExactDuplicateVerifier.protectedSummary(outcome)
        }
        if trashFailures > 0 {
            summary += " · \(trashFailures.formatted()) couldn’t be moved to Trash"
        }
        statusWarning = verificationRejected > 0 || trashFailures > 0 || keepOneSkipped > 0 || protectedCount > 0
        status = summary
        store.notifyChanged()
        reload()
        if !trashedIDs.isEmpty, let batch = outcome.batchID { undoBatchStored = batch }
    }

    private func undoTrash(_ batch: String) async {
        guard !deleting else { return }
        deleting = true
        defer { deleting = false }
        let undone = await ExactDuplicateVerifier.undoTrash(batchID: batch, engine: engine)
        if undone.restored > 0 || undone.message?.contains("unknown or already restored") == true { undoBatchStored = "" }
        statusWarning = undone.restored == 0
        status = undone.restored > 0
            ? "Put back \(undone.restored) file\(undone.restored == 1 ? "" : "s") where \(undone.restored == 1 ? "it was" : "they were")."
            : (undone.message ?? "Nothing was put back. Check the Trash.")
        store.notifyChanged()
        reload()
    }

    private func scanBatchArrived() {
        let new = engine.lastBatch?.batchIndex ?? -1
        guard new != lastSeenBatchIndex else { return }
        lastSeenBatchIndex = new
        store.notifyChanged()
        // Exact re-verify runs once at scan completion: it hashes up to ExactDuplicateVerifier.candidateCap files, and its 30 s digest cache would be cold on every ~1 Hz batch while contending with the scan's reads. Similar mode is cheap, so it refreshes live.
        if !scanActive {
            reload()
        } else if isSimilar, Date().timeIntervalSince(lastScanTimeReload) > 15 {
            // Look-alike grouping compares every pair of photos; once per
            // batch it kept a core busy for the whole scan.
            lastScanTimeReload = Date()
            reload()
        }
    }

    private func reload() {
        // Coalesce instead of cancel-and-restart: hashing and DB reads are not interrupted mid-file. At most one run in flight plus one pending; a result superseded mid-query is dropped so the latest assignment wins.
        // Freeze the duplicate re-query + selection re-derivation while a
        // destructive confirmation is on screen, so the count the user is
        // confirming can't drift mid-scan. Re-runs on dialog dismissal
        // (onChange of confirmDelete), which re-applies the keeper protection.
        if confirmDelete { return }
        if reloadTask != nil { reloadPending = true; return }
        reloadTask = Task { @MainActor in
            exactChecking = true
            defer { exactChecking = false }
            repeat {
                reloadPending = false
                // Read the mode fresh each iteration so a mid-flight mode switch
                // (which marks reloadPending) re-queries the now-selected mode.
                let newGroups: [DuplicateGroup]
                let newExactPartial: Bool
                let newExactCandidateCount: Int
                let newExactSkipped: Int
                var newExactCapped = false
                var newExactTotals: ExactDuplicateVerifier.Totals?
                var newUnshown: (Int, Int64) = (0, 0)
                var stopped = false
                if isSimilar {
                    let engine = engine
                    let threshold = ReadStore.defaultNearDupHamming
                    newGroups = await store.similarImageGroupsAsync(maxHamming: threshold) { offset, limit in
                        await engine.similarClusters(offset: offset, limit: limit, threshold: threshold)
                    }
                    newExactPartial = false
                    newExactCandidateCount = 0
                    newExactSkipped = 0
                } else {
                    let load = await store.exactStoredGroupsAsync(limit: max(ExactDuplicateVerifier.groupCap, exactStored.count))
                    newGroups = load.groups
                    newExactCapped = load.totalGroups > load.groups.count
                    newExactPartial = newExactCapped
                    newExactCandidateCount = load.totalExtraCopies + load.totalGroups
                    newExactSkipped = 0
                    newUnshown = (max(0, load.totalGroups - load.groups.count), max(0, load.totalBytes - load.shownBytes))
                    newExactTotals = ExactDuplicateVerifier.Totals(groups: load.totalGroups, extra: load.totalExtraCopies, bytes: load.totalBytes)
                }
                // Cancelled when the tab went away: drop the unfinished result. A
                // reload asked for since then (the tab came back) starts a fresh task.
                if stopped || Task.isCancelled {
                    reloadTask = nil
                    if reloadPending {
                        reloadPending = false
                        reload()
                    }
                    return
                }
                // A newer reload landed while this query ran — its result is stale;
                // loop and re-query rather than assigning it.
                if reloadPending { continue }
                // Prior keeper of each group (by phash id) before we overwrite `groups`,
                // so we can tell which copies *became* the keeper across this reload.
                let priorKeepers = Set(groups.compactMap { $0.files.first?.id })
                groups = newGroups
                #if DEBUG
                if !isSimilar { print("cleanup: exact groups listed \(newGroups.count) of \(newExactTotals?.groups ?? 0) at \(ProcessInfo.processInfo.systemUptime)") }
                #endif
                exactPreviewPartial = newExactPartial
                exactCandidateCount = newExactCandidateCount
                exactSkipped = newExactSkipped
                exactCapped = newExactCapped
                (exactUnshownClasses, exactUnshownBytes) = newUnshown
                exactTotals = newExactTotals
                exactBase = newExactTotals
                exactStored = isSimilar ? [] : newGroups
                exactUnverified = []
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
            if !isSimilar { startExactVerification(of: groups) }
        }
    }

    /// The stored-hash groups are already on screen; this re-reads them byte by byte off the main thread and swaps in what held up. Groups added later by "Show more" queue behind the batch in flight.
    private func startExactVerification(of stored: [DuplicateGroup]) {
        verifyTask?.cancel()
        exactSettled = []
        exactBatch = []
        exactQueued = stored
        exactSettledUnverified = []
        exactSettledSkipped = 0
        guard !stored.isEmpty else { exactVerify = nil; return }
        exactVerify = (0, stored.count)
        exactPreviewPartial = true
        runNextExactBatch(baseSkipped: exactSkipped)
    }

    private func runNextExactBatch(baseSkipped: Int) {
        let batch = exactQueued
        guard !batch.isEmpty else { exactVerify = nil; refreshExactPartial(); return }
        exactQueued = []
        exactBatch = batch
        let settledCount = exactStored.count - batch.count
        verifyTask = Task { @MainActor in
            let final = await ExactDuplicateVerifier.verifyStored(batch) { update in
                Task { @MainActor in
                    guard !Task.isCancelled, exactVerify != nil else { return }
                    adopt(update, baseSkipped: baseSkipped, settledCount: settledCount)
                }
            }
            guard !Task.isCancelled, !final.cancelled else { return }
            adopt(final, baseSkipped: baseSkipped, settledCount: settledCount)
            exactSettled += final.groups
            exactBatch = []
            exactSettledUnverified.formUnion(final.unverified)
            exactSettledSkipped += final.skipped
            if exactQueued.isEmpty {
                exactVerify = nil
                refreshExactPartial()
            } else {
                exactVerify = (exactStored.count - exactQueued.count, exactStored.count)
                runNextExactBatch(baseSkipped: baseSkipped)
            }
        }
    }

    private func loadMoreExact() {
        Task { @MainActor in await loadMoreExactPage() }
    }

    private func loadMoreExactPage() async {
        guard !exactLoadingMore, !isSimilar else { return }
        exactLoadingMore = true
        let offset = exactStored.count
        do {
            defer { exactLoadingMore = false }
            let load = await store.exactStoredGroupsAsync(offset: offset, limit: ExactDuplicateVerifier.groupCap)
            guard !isSimilar, exactStored.count == offset else { return }
            let known = Set(exactStored.map(\.id))
            let fresh = load.groups.filter { !known.contains($0.id) }
            guard !fresh.isEmpty else { return }
            let wasIdle = exactQueued.isEmpty && exactBatch.isEmpty
            exactStored += fresh
            exactQueued += fresh
            groups = exactSettled + exactBatch + exactQueued
            refreshExactTotals()
            if wasIdle {
                exactVerify = (offset, exactStored.count)
                exactPreviewPartial = true
                runNextExactBatch(baseSkipped: exactSkipped - exactSettledSkipped)
            } else if exactVerify != nil {
                exactVerify = (exactVerify?.done ?? 0, exactStored.count)
            }
        }
    }

    private func refreshExactTotals() {
        guard let base = exactBase else { return }
        exactCapped = base.groups > exactStored.count
        exactTotals = ExactDuplicateVerifier.adjustedTotals(base: base, stored: exactStored, current: groups)
    }

    private func refreshExactPartial() {
        exactPreviewPartial = exactCapped || !exactUnverified.isEmpty || exactSkipped > 0
    }

    private func adopt(_ update: ExactDuplicateVerifier.VerifyUpdate, baseSkipped: Int, settledCount: Int) {
        let priorKeepers = Set(groups.compactMap { $0.files.first?.id })
        exactBatch = update.groups
        groups = exactSettled + exactBatch + exactQueued
        exactUnverified = exactSettledUnverified.union(update.unverified)
        exactSkipped = baseSkipped + exactSettledSkipped + update.skipped
        if !update.finished { exactVerify = (settledCount + update.done, exactStored.count) }
        refreshExactTotals()
        exactPreviewPartial = exactCapped || !exactUnverified.isEmpty || exactSkipped > 0 || !update.finished || !exactQueued.isEmpty
        selection.formIntersection(Set(groups.flatMap { $0.files.map(\.id) }))
        for group in groups {
            if let keeper = group.files.first, !priorKeepers.contains(keeper.id) { selection.remove(keeper.id) }
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
    var gate: ReadOnlyGate = .none
    @State private var hoveredFile: Int64?
    @Environment(\.hoverPaused) private var hoverPaused

    private var readOnlyReason: String? {
        gate.reasonIfAll(group.files.filter { selection.contains($0.id) }.map(\.url))
    }

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
                    SectionLabel(group.isSimilar ? "Look-alikes · \(Plural.noun(group.totalFileCount, "photo", "photos"))" : "Same file · \(Plural.noun(group.totalFileCount, "copy", "copies"))")
                    if group.isSimilar {
                        BadgePill(label: "Looks the same", color: Theme.warning)
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
                        DSMenuLabel(title: "Choose…", systemImage: "checkmark.circle", height: Theme.Metrics.hitTarget)
                    }
                    .menuStyle(.button)
                    .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.s))
                    .menuIndicator(.hidden)
                    .fixedSize()

                    Button { onDeleteGroup() } label: { Label(selectedInGroup == 0 ? "Move to Trash" : "Move \(selectedInGroup) to Trash", systemImage: "trash") }
                    .buttonStyle(selectedInGroup > 0 ? AnyButtonStyle(.dsDanger) : AnyButtonStyle(DSSecondaryButtonStyle()))
                    .disabled(selectedInGroup == 0 || readOnlyReason != nil)
                    .accessibilityHint(readOnlyReason ?? "")
                    .help(readOnlyReason ?? (selectedInGroup == 0 ? "Click the copies you want to remove first" : "Move the chosen copies in this group to Trash"))

                    Button { onSkip() } label: { Label("Skip group", systemImage: "eye.slash") }
                    .buttonStyle(DSSecondaryButtonStyle(height: 26))
                    .help("Hide this group if these aren’t really copies. Nothing is deleted.")

                    Spacer()
                }
                .padding(.horizontal, 14)

                ScrollView(.horizontal, showsIndicators: false) {
                    LazyHStack(spacing: CopyStrip.spacing) {
                        ForEach(Array(group.files.enumerated()), id: \.element.id) { idx, file in
                            CopyTile(
                                file: file,
                                isKeeper: idx == 0,
                                isSimilar: group.isSimilar,
                                isSelected: selection.contains(file.id),
                                hovering: hoveredFile == file.id,
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
                    .onContinuousHover { phase in
                        var next: Int64?
                        if !hoverPaused, case .active(let point) = phase,
                           let index = CopyStrip.thumbnailIndex(at: point, count: group.files.count) {
                            next = group.files[index].id
                        }
                        if next != hoveredFile { hoveredFile = next }
                    }
                    .onChange(of: hoverPaused) { _, paused in if paused { hoveredFile = nil } }
                    .padding(.horizontal, 14)
                }
                .padding(.bottom, 12)
            }
        }
    }
}

/// Groups the user skipped, kept for the session so they stay hidden across tab switches.
@MainActor
enum CleanupSession {
    static var skipped: [String: Set<Int64>] = [:]
}

/// The copies row in a duplicate group. One hover tracker for the row picks the
/// thumbnail under the pointer, instead of one tracker per copy.
enum CopyStrip {
    static let tile: CGFloat = 132
    static let spacing: CGFloat = 12

    static func thumbnailIndex(at point: CGPoint, count: Int) -> Int? {
        guard point.x >= 0, point.y >= 0, point.y < tile else { return nil }
        let pitch = tile + spacing
        let index = Int(point.x / pitch)
        guard index < count, point.x - CGFloat(index) * pitch < tile else { return nil }
        return index
    }
}

private struct CopyTile: View {
    let file: FileRow
    let isKeeper: Bool
    let isSimilar: Bool
    let isSelected: Bool
    let hovering: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var fullFolder: String {
        (file.url.deletingLastPathComponent().path as NSString).abbreviatingWithTildeInPath
    }
    private var folder: String { PathText.tail(file.url.deletingLastPathComponent().path) }
    let onToggle: () -> Void

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
                .frame(width: CopyStrip.tile, height: CopyStrip.tile)
                .overlay(thumbContent.accessibilityHidden(true))
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous)
                        .stroke(borderColor, lineWidth: borderWidth)
                )
                .overlay(badgeOverlay)
                .scaleEffect(hovering && !reduceMotion ? 1.02 : 1.0)
                .animation(Theme.quick, value: hovering)
                .contentShape(Rectangle())
                .onTapGesture { onToggle() }
                .contextMenu {
                    Button("Reveal in Finder") {
                        NSWorkspace.shared.activateFileViewerSelecting([file.url])
                    }
                    Button("Open") {
                        NSWorkspace.shared.open(file.url)
                    }
                }
            Text(file.url.lastPathComponent)
                .font(Theme.Font.body(12, .medium))
                .foregroundStyle(Theme.text1)
                .lineLimit(1).truncationMode(.middle)
                .frame(width: CopyStrip.tile, alignment: .leading)
                .padding(.top, 2)
            PathLabel(path: folder, font: Theme.Font.body(11.5), color: Theme.text2)
                .frame(width: CopyStrip.tile, alignment: .leading)
                .help(fullFolder)
            let sizeText = Text(ByteCountFormatter.string(fromByteCount: file.sizeBytes, countStyle: .file))
            let dateText = file.displayDate.map { Text($0.formatted(date: .numeric, time: .omitted)) }
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 4) {
                    sizeText.fixedSize()
                    Spacer(minLength: 4)
                    if let dateText { dateText.fixedSize() }
                }
                VStack(alignment: .leading, spacing: 0) {
                    sizeText
                    if let dateText { dateText }
                }
            }
            .font(Theme.Font.body(11.5))
            .monospacedDigit()
            .foregroundStyle(Theme.text3)
            .frame(width: CopyStrip.tile)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityDescription)
        .accessibilityAddTraits([.isButton, isSelected ? .isSelected : []])
        .accessibilityAction(.default, onToggle)
        .accessibilityHint(isSelected
            ? "Chosen. It goes to Trash when you press Move to Trash. Click to unchoose."
            : isKeeper
                ? "FileID suggests keeping this copy. Click to choose it for Trash instead."
                : "Click to choose this copy for Trash.")
    }

    private var accessibilityDescription: String {
        let mb = ByteCountFormatter.string(fromByteCount: file.sizeBytes, countStyle: .file)
        let role = isKeeper ? "Keeper. " : ""
        return "\(role)\(file.url.lastPathComponent), in \(folder), \(mb)"
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
                    .help(isSimilar
                          ? "FileID suggests keeping this photo: the best-looking one, then the largest file, then the oldest. Check the others before removing them."
                          : "FileID suggests keeping this copy: the oldest one, then the one in the shortest folder path. Every copy here is identical, so any of them is safe to keep.")
            }
            Button(action: onToggle) {
                Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 22))
                    .foregroundStyle(isSelected ? Theme.danger : Color.white.opacity(0.85))
                    .background(Circle().fill(.black.opacity(0.5)))
            }
            .buttonStyle(DSBareButtonStyle(radius: 11))
            .help(isSelected ? "Don’t move this copy to Trash" : "Choose this copy for Trash")
            .accessibilityLabel(isSelected ? "Chosen for Trash" : "Choose for Trash")
            .accessibilityAddTraits(isSelected ? .isSelected : [])
            .padding(6)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
        }
    }
}
