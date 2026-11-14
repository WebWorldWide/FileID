// Suggested-names sheet: every file that has a suggested name, with a
// thumbnail, named in the saved Name style exactly as "Preview names" does.
// Also owns the rename journal behind "Undo last rename".
import SwiftUI
import FileIDShared

private struct PersistedRenameBatch: Codable {
    let savedAt: Date
    let entries: [ReadStore.RenameOutcome]
}

struct BulkRenameSheet: View {
    let engine: EngineClient
    let store: ReadStore
    @Environment(\.dismiss) private var dismiss

    @State private var rows: [NameStyle.Row] = []
    @State private var kinds: [Int64: String] = [:]
    @State private var loading = true
    @State private var loadError: String?
    @State private var inFlight = false
    @State private var status: String?
    @State private var confirmRename = false
    @State private var lastBatch: [ReadStore.RenameOutcome] = []

    private var ready: [CatalogWrites.NamedRename] { NameStyle.renames(rows) }
    private var nameable: [NameStyle.Row] { rows.filter(\.canCheck) }

    static let confirmMessage = "Only the names change; the files stay in their folders. To put the old names back, press Undo here or Undo last rename at the top of Library."

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if nameable.isEmpty {
                emptyState
            } else {
                actionsBar
                Divider()
                renameList
            }
        }
        .frame(minWidth: 720, minHeight: 480)
        .task { await load() }
    }

    // MARK: - Sections

    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("Suggested names")
                    .font(Theme.Font.body(20, .semibold))
                Text("FileID suggested a clearer name for each file, in your Name style. Edit any name, tick the ones you want, then press Rename.")
                    .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
            }
            Spacer()
            Button("Done") { dismiss() }
                .buttonStyle(.dsSecondary)
                .keyboardShortcut(.cancelAction)
        }
        .padding(16)
    }

    @ViewBuilder
    private var emptyState: some View {
        VStack(spacing: 12) {
            if loading {
                ProgressView().controlSize(.small)
                Text("Working out names…").font(Theme.Font.body(13.5)).foregroundStyle(Theme.text3)
            } else if let loadError {
                Image(systemName: "exclamationmark.triangle")
                    .font(.system(size: 36, weight: .light))
                    .foregroundStyle(Theme.warning)
                Text(loadError).font(Theme.Font.body(13.5)).foregroundStyle(Theme.text2)
                    .multilineTextAlignment(.center).frame(maxWidth: 420)
                Button("Try again") { Task { await load() } }.buttonStyle(.dsSecondary)
            } else {
                Image(systemName: "wand.and.rays")
                    .font(.system(size: 48, weight: .light))
                    .foregroundStyle(Theme.text3)
                Text(rows.isEmpty ? "No suggested names yet" : "Every file already has a good name")
                    .font(Theme.Font.body(16, .semibold))
                Text(rows.isEmpty
                     ? "Describe your files first (Describe Files page). Suggested names appear here."
                     : "Nothing needs renaming in your Name style.")
                    .font(Theme.Font.body(13.5)).foregroundStyle(Theme.text3)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 420)
                if let status { MetaText(status, size: 12) }
                if !lastBatch.isEmpty {
                    Button("Undo") { undo() }.buttonStyle(.dsSecondary).disabled(inFlight)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }

    private var actionsBar: some View {
        HStack(spacing: 8) {
            Text("\(ready.count.formatted()) of \(nameable.count.formatted()) ticked")
                .font(Theme.Font.mono(11))
            if loading { IndeterminateBar().frame(width: 60) }
            if let status {
                Text(status).font(Theme.Font.body(11)).foregroundStyle(Theme.text3).lineLimit(2)
            }
            Spacer()
            Button("Select all") { setAll(true) }
                .buttonStyle(.dsSecondary)
            Button("Clear") { setAll(false) }
                .buttonStyle(.dsSecondary)
            if !lastBatch.isEmpty {
                Button("Undo") { undo() }
                    .buttonStyle(.dsSecondary)
                    .disabled(inFlight)
                    .help("Put back the names from the rename you just made")
            }
            Button {
                confirmRename = true
            } label: {
                Label(inFlight
                      ? "Renaming…"
                      : "Rename \(ready.count.formatted()) file\(ready.count == 1 ? "" : "s")…",
                      systemImage: "wand.and.rays")
            }
            .buttonStyle(DSPrimaryButtonStyle(height: 30))
            .disabled(ready.isEmpty || inFlight || loading)
            .confirmationDialog(
                "Rename \(ready.count.formatted()) file\(ready.count == 1 ? "" : "s")?",
                isPresented: $confirmRename,
                titleVisibility: .visible
            ) {
                Button("Rename \(ready.count.formatted()) file\(ready.count == 1 ? "" : "s")") { apply() }
                Button("Cancel", role: .cancel) { }
            } message: {
                Text(Self.confirmMessage)
            }
        }
        .padding(12)
    }

    private var renameList: some View {
        ScrollView {
            LazyVStack(spacing: 4) {
                ForEach(nameable) { row in
                    BulkRenameRow(
                        row: row,
                        kind: kinds[row.fileID],
                        checked: Binding(get: { row.checked }, set: { NameStyle.setChecked(&rows, fileID: row.fileID, $0) }),
                        edited: Binding(get: { row.edited }, set: { NameStyle.edit(&rows, fileID: row.fileID, $0) }))
                }
            }
            .padding(12)
        }
    }

    // MARK: - Actions

    private func setAll(_ checked: Bool) {
        for index in rows.indices where rows[index].canCheck { rows[index].checked = checked }
    }

    private func load() async {
        loading = true
        loadError = nil
        let files = await Task.detached(priority: .userInitiated) { [store] in store.filesWithProposedNames() }.value
        kinds = Dictionary(files.map { ($0.id, $0.kind) }, uniquingKeysWith: { first, _ in first })
        switch await StyledNames.rows(engine: engine, fileIDs: files.map(\.id), naming: StyledNames.savedNaming(), previous: rows) {
        case .rows(let next): rows = next
        case .failed(let why): rows = []; loadError = why
        }
        loading = false
    }

    private func apply() {
        let renames = ready
        guard !renames.isEmpty, !inFlight else { return }
        inFlight = true
        status = "Renaming…"
        let shown = rows, naming = StyledNames.savedNaming()
        Task { @MainActor in
            let result = await CatalogWrites.applyNames(renames, engine: engine, store: store)
            if !result.renamed.isEmpty {
                BulkRenameSheet.saveLastBatch(result.renamed)
                lastBatch = result.renamed
            }
            await StyledNames.learn(engine: engine, shown: shown, renamed: result.renamed, naming: naming)
            status = StyledNames.renamedSummary(result)
            inFlight = false
            await load()
        }
    }

    private func undo() {
        let batch = lastBatch
        guard !batch.isEmpty, !inFlight else { return }
        inFlight = true
        Task { @MainActor in
            let result = await CatalogWrites.undoRenames(batch, engine: engine, store: store)
            if result.failed == 0 && result.skipped == 0 { BulkRenameSheet.removeBatch(batch) }
            lastBatch = []
            status = StyledNames.undoneSummary(result)
            inFlight = false
            await load()
        }
    }

    // MARK: - Rename journal
    //
    // A short stack, newest last, so one single-file rename can't bury the
    // bulk rename before it. "Undo last rename" takes the newest batch.

    nonisolated static let batchesKey = "bulkRename.batches.v3"
    nonisolated static let lastBatchKey = "bulkRename.lastBatch.v2"
    nonisolated private static let legacyBatchKey = "bulkRename.lastBatch.v1"
    nonisolated static let maxBatches = 10
    /// Journals older than this stop surfacing "Undo last rename" — a
    /// weeks-old batch is far more likely to hit a same-named
    /// replacement file than to be an intentional undo.
    nonisolated static let lastBatchMaxAge: TimeInterval = 7 * 24 * 60 * 60

    nonisolated private static func loadBatches() -> [PersistedRenameBatch] {
        let defaults = UserDefaults.standard
        var batches: [PersistedRenameBatch] = []
        if let data = defaults.data(forKey: batchesKey),
           let stored = try? JSONDecoder().decode([PersistedRenameBatch].self, from: data) {
            batches = stored
        } else if let data = defaults.data(forKey: lastBatchKey),
                  let single = try? JSONDecoder().decode(PersistedRenameBatch.self, from: data) {
            batches = [single]
        }
        let now = Date()
        return batches.filter { !$0.entries.isEmpty && now.timeIntervalSince($0.savedAt) < lastBatchMaxAge }
    }

    nonisolated private static func storeBatches(_ batches: [PersistedRenameBatch]) {
        let defaults = UserDefaults.standard
        defaults.removeObject(forKey: lastBatchKey)
        defaults.removeObject(forKey: legacyBatchKey)
        if batches.isEmpty {
            defaults.removeObject(forKey: batchesKey)
        } else if let data = try? JSONEncoder().encode(Array(batches.suffix(maxBatches))) {
            defaults.set(data, forKey: batchesKey)
        }
    }

    /// Adds a batch on top of the journal. A zero-success batch is ignored so
    /// it can't cover an earlier batch the user can still undo.
    nonisolated static func saveLastBatch(_ entries: [ReadStore.RenameOutcome]) {
        guard !entries.isEmpty else { return }
        storeBatches(loadBatches() + [PersistedRenameBatch(savedAt: Date(), entries: entries)])
    }

    /// The newest rename batch that is still recent enough to undo.
    nonisolated static func loadLastBatch() -> [ReadStore.RenameOutcome]? {
        loadBatches().last?.entries
    }

    /// Drops the newest batch (after it was undone); the one before it becomes
    /// the next "Undo last rename".
    nonisolated static func clearLastBatch() {
        storeBatches(Array(loadBatches().dropLast()))
    }

    /// Drops the batch holding exactly these renames, wherever it sits.
    nonisolated static func removeBatch(_ entries: [ReadStore.RenameOutcome]) {
        let key = entries.map { "\($0.fileID)\t\($0.newPath)" }
        var batches = loadBatches()
        guard let at = batches.lastIndex(where: { $0.entries.map { "\($0.fileID)\t\($0.newPath)" } == key }) else { return }
        batches.remove(at: at)
        storeBatches(batches)
    }

    // MARK: - Person tag history
    //
    // Track which Finder tag we last applied per person so that when
    // the user renames a cluster ("Alex" → "Alex Doe") we can offer
    // to retag previously-tagged photos. Stored as a tiny JSON dict in
    // UserDefaults — `[Int64-as-string : last-tag]`. Engineering-wise
    // this lives next to the rename journal because both are "remembering
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

/// One rename row with a thumbnail, so the user can see which photo, video or
/// PDF "IMG_4823.heic" is before giving it a new name.
private struct BulkRenameRow: View {
    let row: NameStyle.Row
    let kind: String?
    @Binding var checked: Bool
    @Binding var edited: String

    var body: some View {
        let url = URL(fileURLWithPath: row.path)
        HStack(spacing: 10) {
            Toggle(isOn: $checked) { EmptyView() }
                .toggleStyle(DSCheckboxToggleStyle())
                .accessibilityLabel("Rename \(row.currentName)")

            ZStack {
                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                    .fill(Color.secondary.opacity(0.10))
                    .frame(width: 64, height: 64)
                AsyncThumbnail(url: url, size: 96) {
                    Image(systemName: kind.map(FileKind.symbolName(forKind:)) ?? FileTypes.symbolName(forExtension: url.pathExtension))
                        .font(Theme.Font.body(16, .semibold))
                        .foregroundStyle(Theme.text3)
                }
                .frame(width: 64, height: 64)
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
            }
            .overlay(
                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                    .stroke(Color.secondary.opacity(0.20), lineWidth: 1)
            )

            VStack(alignment: .leading, spacing: 4) {
                Text(row.currentName)
                    .font(Theme.Font.mono(11))
                    .foregroundStyle(Theme.text3)
                    .strikethrough(checked, color: .secondary.opacity(0.5))
                    .lineLimit(1).truncationMode(.middle)
                    .help(row.path)
                HStack(spacing: 6) {
                    Image(systemName: "arrow.right")
                        .font(Theme.Font.body(11))
                        .foregroundStyle(Theme.text3)
                        .accessibilityHidden(true)
                    if row.isFollower {
                        Text(edited)
                            .font(Theme.Font.mono(11, .semibold))
                            .foregroundStyle(Theme.gold.opacity(checked ? 0.8 : 0.4))
                            .lineLimit(1).truncationMode(.middle)
                            .help("Follows the name of the file it belongs with")
                    } else {
                        TextField("New name", text: $edited)
                            .textFieldStyle(.plain)
                            .font(Theme.Font.mono(11, .semibold))
                            .foregroundStyle(Theme.gold)
                            .padding(.horizontal, 6).padding(.vertical, 3)
                            .background(Color.black.opacity(0.25), in: RoundedRectangle(cornerRadius: Theme.Radius.xs))
                            .disabled(!checked)
                            .accessibilityLabel("New name for \(row.currentName)")
                    }
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 4)
        .padding(.horizontal, 8)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                .fill(checked ? Theme.gold.opacity(0.05) : Color.clear)
        )
    }
}
