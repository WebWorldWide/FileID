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
                Text("Cleanup").font(.title.bold())
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
        VStack(spacing: 12) {
            Image(systemName: "checkmark.seal.fill")
                .font(.system(size: 56))
                .foregroundStyle(.green.opacity(0.6))
            if store.totalImages == 0 {
                Text("No images scanned yet").font(.title3.bold())
                Text("Run a scan first — duplicates appear here once images are tagged.")
                    .font(.callout).foregroundStyle(.secondary)
            } else if !skippedGroups.isEmpty {
                Text("All groups skipped").font(.title3.bold())
                Button("Show skipped groups again") { skippedGroups.removeAll() }
                    .buttonStyle(.bordered)
            } else {
                Text("No duplicates found").font(.title3.bold())
                Text("Files were compared by perceptual hash (dHash). \(store.totalImages) images checked.")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
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
                    Text(s).font(.caption).foregroundStyle(.secondary)
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

    private func confirmDeleteSelected() {
        let alert = NSAlert()
        alert.messageText = "Delete \(totalSelected) file\(totalSelected == 1 ? "" : "s")?"
        let mb = String(format: "%.1f MB", totalSelectedMB)
        alert.informativeText = "Moves the selected duplicates to Trash. Frees ~\(mb). You can restore from Trash."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Move to Trash")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task { await trashSelected(across: visibleGroups) }
    }

    private func trashSelectedInGroup(_ g: DuplicateGroup) async {
        await trashSelected(across: [g])
    }

    private func trashSelected(across groupsToScan: [DuplicateGroup]) async {
        var trashedIDs: [Int64] = []
        var freedBytes: Int64 = 0
        for group in groupsToScan {
            for f in group.files where selection.contains(f.id) {
                do {
                    try FileManager.default.trashItem(at: f.url, resultingItemURL: nil)
                    trashedIDs.append(f.id)
                    freedBytes += f.sizeBytes
                } catch {
                    NSLog("FileID v2 cleanup: could not trash %@: %@", f.url.path, "\(error)")
                }
            }
        }
        let mb = Double(freedBytes) / 1_048_576
        let pruned = store.deleteFiles(ids: trashedIDs)
        for id in trashedIDs { selection.remove(id) }
        status = "Trashed \(trashedIDs.count) file\(trashedIDs.count == 1 ? "" : "s") · freed \(String(format: "%.1f", mb)) MB · pruned \(pruned) DB rows"
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
