// Cleanup: phash duplicate groups with per-tile selection. Default
// selection is "every non-keeper". The user can override per group, or
// trash across all groups at once.
import SwiftUI
import AppKit
import FileIDShared

struct CleanupView: View {
    let engine: EngineClient
    let store: ReadStore

    @State private var groups: [DuplicateGroup] = []
    @State private var lastSeenBatchIndex: Int = -1
    @State private var status: String?
    @State private var confirmDelete: Bool = false

    /// Initialized lazily on first reload to non-keepers per group.
    @State private var selection: Set<Int64> = []
    @State private var skippedGroups: Set<Int64> = []

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
        .onAppear {
            store.openIfPossible()
            reload()
        }
        .onChange(of: engine.lastBatch?.batchIndex ?? -1) { _, new in
            if new != lastSeenBatchIndex {
                lastSeenBatchIndex = new
                store.notifyChanged()
                reload()
            }
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
            Spacer()
            if !visibleGroups.isEmpty {
                HStack(spacing: 6) {
                    Button("Select all non-keepers") { selectAllNonKeepers() }
                        .buttonStyle(.bordered)
                        .help("Default: select every duplicate except the keeper in each group.")
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
        let mb = String(format: "%.1f", store.totalReclaimableMB)
        let skipped = skippedGroups.count
        let base = "\(g) duplicate group\(g == 1 ? "" : "s") · \(mb) MB reclaimable if you keep 1 per group"
        return skipped > 0 ? "\(base) · \(skipped) skipped" : base
    }

    // MARK: - Empty / list

    @ViewBuilder
    private var empty: some View {
        if store.totalImages == 0 {
            EmptyStateView(
                icon: "trash.slash",
                title: "Nothing to clean up yet",
                message: "Pick a folder in the sidebar and click Start Scan. Once images are tagged, any visual duplicates show up here grouped together — pick which copy to keep."
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
        } else {
            EmptyStateView(
                icon: "checkmark.seal.fill",
                title: "No duplicates found",
                message: "All \(store.totalImages) images compared — none look visually identical."
            )
        }
    }

    @ViewBuilder
    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
                // First-timer explainer above the groups. Inline (not a
                // tooltip) so the keeper concept is impossible to miss.
                HStack(spacing: 8) {
                    Image(systemName: "info.circle")
                        .foregroundStyle(.green)
                    Text("Each group is a set of duplicate copies. The **KEEPER** is the copy we recommend you keep — usually the largest. Click another tile in a group to make it the keeper instead. Selected copies move to Trash; you can restore them if you change your mind.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(10)
                .background(RoundedRectangle(cornerRadius: 6).fill(Color.green.opacity(0.08)))
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.green.opacity(0.3), lineWidth: 1))
                .padding(.bottom, 4)
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
                        Image(systemName: "trash.fill")
                            .foregroundStyle(.green)
                        Text(s)
                            .font(.callout)
                        Spacer()
                        Button("Open Trash") {
                            // Reveal the user's Trash in Finder. macOS
                            // Cmd+Z in Finder restores the most recent
                            // trash operation — that's the undo path.
                            NSWorkspace.shared.open(
                                URL(fileURLWithPath: NSHomeDirectory())
                                    .appendingPathComponent(".Trash")
                            )
                        }
                        .buttonStyle(.bordered)
                        Button("Dismiss") { status = nil }
                            .buttonStyle(.borderless)
                    }
                    .padding(12)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Color.green.opacity(0.10)))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.green.opacity(0.4), lineWidth: 1))
                    .help("Files moved to Trash can be restored. In Finder, open Trash and press ⌘Z (or right-click → Put Back) to restore the most recent items.")
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
        var trashedIDs: [Int64] = []
        var freedBytes: Int64 = 0
        // Track which duplicate groups had at least one file trashed —
        // their KEEPERS (the un-trashed survivors) are candidates for
        // the auto-tag step below.
        var keeperURLsToTag: [URL] = []
        for group in groupsToScan {
            var groupHadTrash = false
            for f in group.files where selection.contains(f.id) {
                do {
                    try FileManager.default.trashItem(at: f.url, resultingItemURL: nil)
                    trashedIDs.append(f.id)
                    freedBytes += f.sizeBytes
                    groupHadTrash = true
                } catch {
                    NSLog("FileID v2 cleanup: could not trash %@: %@", f.url.path, "\(error)")
                }
            }
            if groupHadTrash {
                // Keepers: anything in this group we did NOT trash.
                for f in group.files where !selection.contains(f.id) {
                    keeperURLsToTag.append(f.url)
                }
            }
        }
        let mb = Double(freedBytes) / 1_048_576
        let pruned = store.deleteFiles(ids: trashedIDs)
        for id in trashedIDs { selection.remove(id) }

        // P5 — auto-tag keepers (Settings toggle, default on). Useful so
        // the user can find "files I deduped this session" in Finder.
        var tagSummary = ""
        let autoTagOn = UserDefaults.standard.object(forKey: AppSettings.cleanupAutoTagKey) == nil
            ? AppSettings.cleanupAutoTagDefault
            : UserDefaults.standard.bool(forKey: AppSettings.cleanupAutoTagKey)
        if autoTagOn, !keeperURLsToTag.isEmpty {
            let result = TagWriter.addTagsBulk([AppSettings.cleanupAutoTagName],
                                                 to: keeperURLsToTag)
            if result.added > 0 {
                tagSummary = " · tagged \(result.added) keeper\(result.added == 1 ? "" : "s") with \"\(AppSettings.cleanupAutoTagName)\""
            }
            store.notifyChanged()
        }

        // Plain-language status. "DB rows pruned" is internal noise —
        // users care about file count + reclaimed space. Tag summary
        // appended only when the auto-tag toggle did something.
        _ = pruned // intentionally not surfaced in the UI string
        status = "Trashed \(trashedIDs.count) file\(trashedIDs.count == 1 ? "" : "s")"
            + " · freed \(String(format: "%.1f", mb)) MB"
            + tagSummary
        reload()
    }

    private func reload() {
        groups = store.duplicateGroups()
        let visibleIDs = Set(groups.flatMap { $0.files.map(\.id) })
        selection.formIntersection(visibleIDs)
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
                    BadgePill(label: "\(group.files.count) copies")
                    Text(String(format: "%.1f MB total · %.1f MB if you keep 1",
                                Double(group.totalBytes) / 1_048_576,
                                Double(group.reclaimableBytes) / 1_048_576))
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
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
                        Button("All except keeper") { onSelectAllExceptKeeper() }
                        Button("All") { onSelectAll() }
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
                        ForEach(Array(group.files.enumerated()), id: \.offset) { idx, file in
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
        .task { thumb = await ThumbnailService.shared.thumbnail(for: file.url, size: 264) }
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
