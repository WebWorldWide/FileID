// Bulk tag-apply sheet — presented from Library multi-select mode.
// Takes a comma-separated list of Finder tags and writes them to every
// selected file via TagWriter.addTagsBulk. Reports added vs unchanged
// counts so the user can tell what actually changed.
import SwiftUI
import FileIDShared

struct BulkTagSheet: View {
    let files: [FileRow]
    let store: ReadStore
    let onComplete: () -> Void
    @Environment(\.dismiss) private var dismiss

    @State private var rawTags: String = ""
    @State private var inFlight: Bool = false
    @State private var status: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Apply tags to \(files.count) file\(files.count == 1 ? "" : "s")")
                        .font(.title2.bold())
                    Text("Comma-separated. Existing tags on each file are preserved — only new tags get added.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            TextField("e.g. Vacation, Family, Important", text: $rawTags)
                .textFieldStyle(.roundedBorder)
                .font(.callout)
            if let s = status {
                Text(s).font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                Spacer()
                Button(action: apply) {
                    Label(inFlight ? "Tagging…" : "Apply",
                          systemImage: "tag.fill")
                        .padding(.horizontal, 16).padding(.vertical, 6)
                        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.gold))
                        .foregroundStyle(.black)
                        .font(.callout.bold())
                }
                .buttonStyle(.plain)
                .keyboardShortcut(.defaultAction)
                .disabled(inFlight || parsedTags.isEmpty || files.isEmpty)
            }
        }
        .padding(20)
        .frame(width: 520)
    }

    private var parsedTags: [String] {
        rawTags.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }

    private func apply() {
        let tags = parsedTags
        guard !tags.isEmpty, !files.isEmpty else { return }
        inFlight = true
        status = nil
        let urls = files.map(\.url)
        // Work off-MainActor via `Task.detached().value`, then resume on
        // MainActor for the UI updates. Avoids Swift 6 Sendable-capture
        // issues with the view's onComplete closure.
        Task {
            let result = await Task.detached(priority: .userInitiated) {
                // journal: nil — the sheet owns the journal write so a
                // total-failure batch can't clear a still-valid prior one
                // (TagWriter clears on any empty batch). (F-C4-005)
                let detailed = TagWriter.addTagsBulkDetailed(tags, to: urls, journal: nil)
                BulkTagSheet.recordOrPreserveJournal(outcomes: detailed.outcomes,
                                                     failed: detailed.failed)
                return detailed
            }.value
            inFlight = false
            if result.failed == 0 && result.unchanged == 0 {
                status = "Tagged \(result.added) file\(result.added == 1 ? "" : "s")"
            } else if result.failed == 0 {
                status = "Tagged \(result.added) · \(result.unchanged) already had these tags"
            } else {
                status = "Tagged \(result.added) · \(result.unchanged) unchanged · \(result.failed) failed"
                    + (result.firstError.map { " — \($0)" } ?? "")
            }
            store.notifyChanged()
            if result.failed == 0 {
                onComplete()
                dismiss()
            }
        }
    }

    nonisolated static let lastBatchKey = "bulkTag.lastBatch.v1"

    /// Update the undo journal for a finished bulk-tag batch:
    /// • successful additions → record them so "Undo last tags" strips ONLY
    ///   what FileID just added.
    /// • a clean all-unchanged batch (no failures, nothing new added) → clear
    ///   it, so a stale earlier batch can't be undone by mistake (F-C3-034).
    /// • a batch with failures and no successes → leave the prior journal
    ///   intact; a failed batch must not destroy "Undo last tags". (F-C4-005)
    nonisolated static func recordOrPreserveJournal(outcomes: [TagWriter.TagOutcome],
                                                    failed: Int,
                                                    in defaults: UserDefaults = .standard) {
        if !outcomes.isEmpty {
            if let data = try? JSONEncoder().encode(outcomes) {
                defaults.set(data, forKey: lastBatchKey)
            }
        } else if failed == 0 {
            defaults.removeObject(forKey: lastBatchKey)
        }
    }

    /// Decode the most recent tag batch from UserDefaults. Nil when no
    /// batch has been recorded yet.
    nonisolated static func loadLastBatch() -> [TagWriter.TagOutcome]? {
        guard let data = UserDefaults.standard.data(forKey: lastBatchKey) else { return nil }
        return try? JSONDecoder().decode([TagWriter.TagOutcome].self, from: data)
    }

    /// Clear the persisted batch (after successful undo).
    nonisolated static func clearLastBatch() {
        UserDefaults.standard.removeObject(forKey: lastBatchKey)
    }
}
