// Bulk rename sheet — applies VLM-suggested filenames to many files at
// once with a preview-and-confirm step. Saves the batch to UserDefaults
// so the user can undo if they don't like the result.
import SwiftUI
import FileIDShared

private struct PersistedRenameBatch: Codable {
    let savedAt: Date
    let entries: [ReadStore.RenameOutcome]
}

struct BulkRenameSheet: View {
    let store: ReadStore
    @Environment(\.dismiss) private var dismiss

    @State private var files: [FileRow] = []
    @State private var selectedIDs: Set<Int64> = []
    @State private var inFlight: Bool = false
    @State private var status: String?
    @State private var confirmLargeBatch: Bool = false

    /// Above this many selected files, present a confirmation dialog
    /// before applying. Below, just go.
    private static let largeBatchThreshold = 50

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if files.isEmpty {
                emptyState
            } else {
                actionsBar
                Divider()
                renameList
            }
        }
        .frame(minWidth: 720, minHeight: 480)
        .onAppear {
            files = store.filesWithProposedNames()
            selectedIDs = Set(files.map(\.id))   // default: select all
        }
    }

    // MARK: - Sections

    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("Apply smart names")
                    .font(.title2.bold())
                Text("Deep Analyze produced a smart filename for each photo based on its content. Pick which ones to apply.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button("Done") { dismiss() }
                .keyboardShortcut(.defaultAction)
        }
        .padding(16)
    }

    private var emptyState: some View {
        VStack(spacing: 12) {
            Image(systemName: "wand.and.rays")
                .font(.system(size: 48, weight: .light))
                .foregroundStyle(.secondary)
            Text("No smart names yet").font(.title3.bold())
            Text("Run Deep Analyze on your photos first — smart filenames will appear here.")
                .font(.callout).foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }

    private var actionsBar: some View {
        HStack(spacing: 8) {
            Text("\(selectedIDs.count) of \(files.count) selected")
                .font(.caption.monospaced())
            Spacer()
            Button("Select all") { selectedIDs = Set(files.map(\.id)) }
                .buttonStyle(.bordered)
            Button("Clear") { selectedIDs.removeAll() }
                .buttonStyle(.bordered)
            Button {
                attemptApply()
            } label: {
                Label(inFlight
                      ? "Renaming…"
                      : "Rename \(selectedIDs.count) file\(selectedIDs.count == 1 ? "" : "s")",
                      systemImage: "wand.and.rays")
                    .padding(.horizontal, 12).padding(.vertical, 6)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Theme.gold))
                    .foregroundStyle(.black)
                    .font(.callout.bold())
            }
            .buttonStyle(.plain)
            .disabled(selectedIDs.isEmpty || inFlight)
            .confirmationDialog(
                "Rename \(selectedIDs.count) files?",
                isPresented: $confirmLargeBatch
            ) {
                Button("Rename \(selectedIDs.count) files", role: .destructive) {
                    applySelected()
                }
                Button("Cancel", role: .cancel) { }
            } message: {
                Text("This will rename \(selectedIDs.count) files on disk. The Library 'Undo last rename' button can revert it, but it won't restore filenames you've moved or renamed again afterwards.")
            }
            if let s = status {
                Text(s).font(.caption2).foregroundStyle(.secondary)
            }
        }
        .padding(12)
    }

    private var renameList: some View {
        ScrollView {
            LazyVStack(spacing: 4) {
                ForEach(files) { f in
                    renameRow(f)
                }
            }
            .padding(12)
        }
    }

    @ViewBuilder
    private func renameRow(_ f: FileRow) -> some View {
        BulkRenameRow(
            file: f,
            isSelected: selectedIDs.contains(f.id),
            onToggle: { id in
                if selectedIDs.contains(id) {
                    selectedIDs.remove(id)
                } else {
                    selectedIDs.insert(id)
                }
            }
        )
    }

    // MARK: - Actions

    private func attemptApply() {
        if selectedIDs.count >= Self.largeBatchThreshold {
            confirmLargeBatch = true
        } else {
            applySelected()
        }
    }

    private func applySelected() {
        let toRename = files.filter { selectedIDs.contains($0.id) }
        guard !toRename.isEmpty else { return }
        inFlight = true
        status = nil
        let storeRef = store
        Task.detached(priority: .userInitiated) {
            let result = storeRef.applyProposedNamesBulk(toRename)
            // Persist the batch to UserDefaults so the user can undo.
            BulkRenameSheet.saveLastBatch(result.renamed)
            await MainActor.run {
                inFlight = false
                files = storeRef.filesWithProposedNames()
                selectedIDs = Set(files.map(\.id))
                if result.failed == 0 {
                    status = "Renamed \(result.renamed.count) file\(result.renamed.count == 1 ? "" : "s")"
                } else {
                    status = "Renamed \(result.renamed.count), \(result.failed) failed"
                        + (result.firstError.map { " — \($0)" } ?? "")
                }
            }
        }
    }

    nonisolated static let lastBatchKey = "bulkRename.lastBatch.v2"
    /// Pre-expiry format (bare [RenameOutcome], no timestamp or file
    /// identity) — never read anymore, only cleaned up.
    nonisolated private static let legacyBatchKey = "bulkRename.lastBatch.v1"
    /// Journals older than this stop surfacing "Undo last rename" — a
    /// weeks-old batch is far more likely to hit a same-named
    /// replacement file than to be an intentional undo.
    nonisolated static let lastBatchMaxAge: TimeInterval = 7 * 24 * 60 * 60

    nonisolated static func saveLastBatch(_ entries: [ReadStore.RenameOutcome]) {
        // A zero-success batch (e.g. a bulk rename where every file failed)
        // must NOT overwrite a still-valid prior journal — that would silently
        // destroy "Undo last rename" for the earlier batch the user can still
        // legitimately revert. (F-C4-005)
        guard !entries.isEmpty else { return }
        let batch = PersistedRenameBatch(savedAt: Date(), entries: entries)
        guard let data = try? JSONEncoder().encode(batch) else { return }
        UserDefaults.standard.set(data, forKey: lastBatchKey)
        UserDefaults.standard.removeObject(forKey: legacyBatchKey)
    }

    /// Decode the most recent rename batch from UserDefaults. Returns
    /// nil if no batch has been recorded yet or the recorded batch is
    /// older than `lastBatchMaxAge`. Nonisolated so background tasks
    /// can persist the batch right after applying it.
    nonisolated static func loadLastBatch() -> [ReadStore.RenameOutcome]? {
        guard let data = UserDefaults.standard.data(forKey: lastBatchKey),
              let batch = try? JSONDecoder().decode(PersistedRenameBatch.self, from: data),
              Date().timeIntervalSince(batch.savedAt) < lastBatchMaxAge
        else { return nil }
        return batch.entries
    }

    /// Clear the persisted batch (after successful undo).
    nonisolated static func clearLastBatch() {
        UserDefaults.standard.removeObject(forKey: lastBatchKey)
        UserDefaults.standard.removeObject(forKey: legacyBatchKey)
    }

    // MARK: - Person tag history (P10)
    //
    // Track which Finder tag we last applied per person so that when
    // the user renames a cluster ("Alex" → "Alex Doe") we can offer
    // to retag previously-tagged photos. Stored as a tiny JSON dict in
    // UserDefaults — `[Int64-as-string : last-tag]`. Engineering-wise
    // this lives next to lastBatchKey because both are "remembering
    // something about an irreversible mutation we just made."

    nonisolated static let personTagHistoryKey = "personTagHistory.v1"

    nonisolated static func recordPersonTag(personID: Int64, tag: String) {
        var dict = loadPersonTagHistory()
        dict[String(personID)] = tag
        if let data = try? JSONEncoder().encode(dict) {
            UserDefaults.standard.set(data, forKey: personTagHistoryKey)
        }
    }

    nonisolated static func lastPersonTag(personID: Int64) -> String? {
        loadPersonTagHistory()[String(personID)]
    }

    nonisolated private static func loadPersonTagHistory() -> [String: String] {
        guard let data = UserDefaults.standard.data(forKey: personTagHistoryKey),
              let dict = try? JSONDecoder().decode([String: String].self, from: data)
        else { return [:] }
        return dict
    }
}

/// Single rename row with a Quick Look thumbnail on the left so the
/// user can see the photo / video / PDF they're renaming. Without
/// this, "IMG_4823.heic → Mia at the piano" is meaningless — the
/// user has no idea which photo IMG_4823 actually is.
private struct BulkRenameRow: View {
    let file: FileRow
    let isSelected: Bool
    let onToggle: (Int64) -> Void

    @State private var thumb: NSImage?

    var body: some View {
        let oldName = file.url.lastPathComponent
        let proposed = file.vlmProposedName ?? ""
        let newName: String = {
            let ext = file.url.pathExtension
            if proposed.isEmpty { return oldName }
            return ext.isEmpty ? proposed : "\(proposed).\(ext)"
        }()
        HStack(spacing: 10) {
            Image(systemName: isSelected ? "checkmark.square.fill" : "square")
                .foregroundStyle(isSelected ? Theme.gold : .secondary)
                .onTapGesture { onToggle(file.id) }

            // Thumbnail — reuses ThumbnailService (QuickLookThumbnailing
            // backed). Works for images, videos, PDFs, and Office docs.
            ZStack {
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color.secondary.opacity(0.10))
                    .frame(width: 64, height: 64)
                if let thumb {
                    Image(nsImage: thumb)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                        .frame(width: 64, height: 64)
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                        .transition(.opacity)
                } else {
                    Image(systemName: kindFallbackIcon(file.kind))
                        .font(.title3)
                        .foregroundStyle(.secondary)
                }
            }
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .stroke(Color.secondary.opacity(0.20), lineWidth: 1)
            )

            VStack(alignment: .leading, spacing: 2) {
                Text(oldName)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .strikethrough(isSelected, color: .secondary.opacity(0.5))
                    .lineLimit(1).truncationMode(.middle)
                HStack(spacing: 4) {
                    Image(systemName: "arrow.right")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                    Text(newName)
                        .font(.caption.monospaced().bold())
                        .foregroundStyle(Theme.gold)
                        .lineLimit(1).truncationMode(.middle)
                }
            }
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
        .onTapGesture { onToggle(file.id) }
        .padding(.vertical, 4)
        .padding(.horizontal, 8)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(isSelected ? Theme.gold.opacity(0.05) : Color.clear)
        )
        .task(id: file.id) {
            thumb = await ThumbnailService.shared.thumbnail(for: file.url, size: 96)
        }
    }

    private func kindFallbackIcon(_ kind: String) -> String {
        switch kind {
        case "image": return "photo"
        case "video": return "video"
        case "pdf":   return "doc.richtext"
        case "doc":   return "doc.text"
        case "audio": return "waveform"
        default:      return "doc"
        }
    }
}
