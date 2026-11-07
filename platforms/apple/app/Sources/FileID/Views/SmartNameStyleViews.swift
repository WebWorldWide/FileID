// "Name style" controls and the "Preview names" sheet: proposeNames in the
// saved style, editable names, apply through renameFiles, Undo through the
// existing rename journal.
import SwiftUI
import FileIDShared

struct NameStyleControls: View {
    var engine: EngineClient? = nil
    var refresh = 0
    var onLearnedReset: () -> Void = {}
    @AppStorage(NameStyle.styleKey) private var style = NameStyle.defaultStyle
    @AppStorage(NameStyle.maxLengthKey) private var maxLength = NameStyle.defaultMaxLength
    @AppStorage(NameStyle.includePeopleKey) private var includePeople = NameStyle.defaultIncludePeople
    @AppStorage(NameStyle.includeEventKey) private var includeEvent = NameStyle.defaultIncludeEvent
    @AppStorage(NameStyle.includeDateKey) private var includeDate = NameStyle.defaultIncludeDate
    @AppStorage(NameStyle.learnKey) private var learn = NameStyle.defaultLearn

    private var lengthBinding: Binding<Double> {
        Binding(get: { Double(min(max(maxLength, NameStyle.lengthRange.lowerBound), NameStyle.lengthRange.upperBound)) },
                set: { maxLength = Int($0.rounded()) })
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Name style").font(Theme.Font.body(13, .semibold)).foregroundStyle(Theme.text1)
            Picker("Name style", selection: $style) {
                ForEach(NameStyle.styles, id: \.id) { Text($0.title).tag($0.id) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .tint(Theme.gold)
            HStack(spacing: 12) {
                Slider(value: lengthBinding, in: Double(NameStyle.lengthRange.lowerBound)...Double(NameStyle.lengthRange.upperBound), step: 1) {
                    Text("Longest name")
                }
                .labelsHidden()
                .tint(Theme.gold)
                .accessibilityLabel("Longest name")
                .accessibilityValue("\(maxLength) characters")
                Text("\(maxLength) characters")
                    .font(Theme.Font.mono(11.5)).foregroundStyle(Theme.text2).monospacedDigit()
                    .frame(minWidth: 96, alignment: .trailing)
                    .accessibilityHidden(true)
            }
            Toggle("Add confirmed people", isOn: $includePeople).toggleStyle(DSToggleStyle())
            Toggle("Add event", isOn: $includeEvent).toggleStyle(DSToggleStyle())
            Toggle("Add date", isOn: $includeDate).toggleStyle(DSToggleStyle())
                .disabled(!NameStyle.dateToggleEnabled(style: style))
                .opacity(NameStyle.dateToggleEnabled(style: style) ? 1 : 0.45)
                .help(NameStyle.dateToggleEnabled(style: style) ? "Add the capture date at the end" : "Date first already starts with the date")
            Toggle("Learn from my renames", isOn: $learn).toggleStyle(DSToggleStyle())
                .help("When you edit a suggested name before renaming, later suggestions follow your style. Stays on this Mac.")
            if let engine { LearnedStyleRow(engine: engine, enabled: learn, refresh: refresh, onReset: onLearnedReset) }
        }
        .font(Theme.Font.body(13))
    }
}

private struct LearnedStyleRow: View {
    let engine: EngineClient
    let enabled: Bool
    let refresh: Int
    let onReset: () -> Void
    @State private var learned = NameStyle.Learned()
    @State private var busy = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(NameStyle.learnedSummary(learned, enabled: enabled))
                .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 8)
            Button("Reset") { Task { await load(reset: true); onReset() } }
                .buttonStyle(DSSecondaryButtonStyle(height: 26))
                .disabled(busy || learned.samples == 0)
                .help("Forget the style FileID learned from your renames")
                .accessibilityLabel("Reset learned name style")
        }
        .task(id: refresh) { await load(reset: false) }
    }

    private func load(reset: Bool) async {
        busy = true
        defer { busy = false }
        let request = NameStyle.preferencesRequest(id: "names-learned-" + UUID().uuidString, reset: reset)
        guard let response = await TakeRequests.send(engine, request, timeout: 15), response.status == "ok" else { return }
        learned = NameStyle.learned(from: response.hits)
    }
}

struct SmartNamePreviewSheet: View {
    let engine: EngineClient
    let store: ReadStore
    let fileIDs: [Int64]

    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @AppStorage(NameStyle.styleKey) private var style = NameStyle.defaultStyle
    @AppStorage(NameStyle.maxLengthKey) private var maxLength = NameStyle.defaultMaxLength
    @AppStorage(NameStyle.includePeopleKey) private var includePeople = NameStyle.defaultIncludePeople
    @AppStorage(NameStyle.includeEventKey) private var includeEvent = NameStyle.defaultIncludeEvent
    @AppStorage(NameStyle.includeDateKey) private var includeDate = NameStyle.defaultIncludeDate
    @AppStorage(NameStyle.learnKey) private var learn = NameStyle.defaultLearn

    @State private var rows: [NameStyle.Row] = []
    @State private var loading = false
    @State private var applying = false
    @State private var status: String?
    @State private var generation = 0
    @State private var pending: Task<Void, Never>?
    @State private var lastBatch: [ReadStore.RenameOutcome] = []
    @State private var learnedRefresh = 0

    private var naming: CatalogNaming {
        NameStyle.naming(style: style, maxLength: maxLength, includePeople: includePeople, includeEvent: includeEvent,
                         includeDate: includeDate, learn: learn)
    }
    private var rowSpring: Animation? { reduceMotion ? nil : .spring(response: 0.35, dampingFraction: 0.8) }
    private var ready: [CatalogWrites.NamedRename] { NameStyle.renames(rows) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .firstTextBaseline) {
                Text("Preview names").font(Theme.Font.body(17, .semibold))
                Spacer()
                if loading { IndeterminateBar().frame(width: 60) }
                MetaText("\(fileIDs.count.formatted()) file\(fileIDs.count == 1 ? "" : "s")", size: 11.5)
            }
            Text("Check the new names and edit any you want. Only ticked files are renamed.")
                .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
            NameStyleControls(engine: engine, refresh: learnedRefresh, onLearnedReset: { schedulePreview() })
                .padding(12)
                .background(Color.black.opacity(0.22), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
            list
            footer
        }
        .padding(20)
        .frame(minWidth: 680, minHeight: 520)
        .background(LavaLampBackground())
        .preferredColorScheme(.dark)
        .task { await preview() }
        .onChange(of: naming) { _, _ in schedulePreview() }
        .onDisappear { pending?.cancel() }
    }

    @ViewBuilder
    private var list: some View {
        if rows.isEmpty {
            MetaText(loading ? "Working out names…" : (status ?? "No files to name."))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView {
                LazyVStack(spacing: 6) {
                    ForEach(rows) { row in
                        SmartNameRow(row: row,
                                     chip: NameStyle.companionChip(row, in: rows),
                                     label: NameStyle.accessibilityLabel(row, in: rows),
                                     checked: Binding(get: { row.checked }, set: { NameStyle.setChecked(&rows, fileID: row.fileID, $0) }),
                                     edited: Binding(get: { row.edited }, set: { NameStyle.edit(&rows, fileID: row.fileID, $0) }))
                            .transition(reduceMotion ? .identity : .opacity.combined(with: .move(edge: .top)))
                    }
                }
                .animation(rowSpring, value: rows.map(\.fileID))
            }
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            if let status { MetaText(status, size: 12).lineLimit(2) }
            Spacer()
            if !lastBatch.isEmpty {
                Button("Undo") { undo() }.buttonStyle(DSSecondaryButtonStyle(height: 30)).disabled(applying)
            }
            Button("Close") { dismiss() }.buttonStyle(DSSecondaryButtonStyle(height: 30)).keyboardShortcut(.cancelAction)
            Button("Rename \(ready.count.formatted()) file\(ready.count == 1 ? "" : "s")") { apply() }
                .buttonStyle(DSPrimaryButtonStyle(height: 30))
                .disabled(ready.isEmpty || applying || loading)
        }
    }

    private func schedulePreview() {
        pending?.cancel()
        pending = Task { @MainActor in
            try? await Task.sleep(for: NameStyle.previewDelay)
            guard !Task.isCancelled else { return }
            await preview()
        }
    }

    private func preview() async {
        generation += 1
        let mine = generation
        guard !fileIDs.isEmpty else { status = "Choose files in Library first."; return }
        loading = true
        let request = NameStyle.request(id: "names-" + UUID().uuidString, fileIDs: fileIDs, naming: naming)
        let response = await TakeRequests.send(engine, request, timeout: 30)
        guard mine == generation else { return }
        loading = false
        guard let response else { status = "FileID didn't answer in time. Try again in a moment."; return }
        guard response.status == "ok" else { status = response.message ?? "FileID couldn't suggest names right now."; return }
        let next = NameStyle.rows(from: response.hits, previous: rows)
        withAnimation(rowSpring) { rows = next }
        if lastBatch.isEmpty { status = nil }
    }

    private func apply() {
        let renames = ready
        guard !renames.isEmpty, !applying else { return }
        applying = true
        status = "Renaming…"
        let shown = rows, style = naming
        Task { @MainActor in
            let result = await CatalogWrites.applyNames(renames, engine: engine, store: store)
            if !result.renamed.isEmpty {
                BulkRenameSheet.saveLastBatch(result.renamed)
                lastBatch = result.renamed
            }
            await learnFrom(shown, renamed: result.renamed, naming: style)
            status = "Renamed \(result.renamed.count.formatted()) file\(result.renamed.count == 1 ? "" : "s")"
                + (result.failed > 0 ? " · \(result.failed.formatted()) couldn't be renamed" + (result.firstError.map { ": \($0)" } ?? "") : "") + "."
            applying = false
            await preview()
        }
    }

    private func learnFrom(_ shown: [NameStyle.Row], renamed: [ReadStore.RenameOutcome], naming: CatalogNaming) async {
        guard naming.learn else { return }
        let edits = NameStyle.edits(shown, renamed: Set(renamed.map(\.fileID)))
        guard !edits.isEmpty else { return }
        let request = NameStyle.learnRequest(id: "names-learn-" + UUID().uuidString, edits: edits, naming: naming)
        _ = await TakeRequests.send(engine, request, timeout: 15)
        learnedRefresh += 1
    }

    private func undo() {
        let batch = lastBatch
        guard !batch.isEmpty, !applying else { return }
        applying = true
        Task { @MainActor in
            let result = await CatalogWrites.undoRenames(batch, engine: engine, store: store)
            if result.failed == 0 && result.skipped == 0 { BulkRenameSheet.clearLastBatch() }
            lastBatch = []
            status = "Put back \(result.undone.formatted()) name\(result.undone == 1 ? "" : "s")"
                + (result.skipped > 0 ? " · \(result.skipped.formatted()) skipped because the file moved or changed" : "")
                + (result.failed > 0 ? " · \(result.failed.formatted()) couldn't be renamed back" : "") + "."
            applying = false
            await preview()
        }
    }
}

private struct SmartNameRow: View {
    let row: NameStyle.Row
    let chip: String?
    let label: String
    @Binding var checked: Bool
    @Binding var edited: String

    var body: some View {
        HStack(spacing: 10) {
            Toggle(isOn: $checked) { EmptyView() }
                .toggleStyle(DSCheckboxToggleStyle())
                .disabled(!row.canCheck)
                .accessibilityLabel("Rename \(row.currentName)")
            HStack(spacing: 5) {
                if row.isFollower {
                    Image(systemName: "arrow.turn.down.right").font(.system(size: 9, weight: .semibold)).foregroundStyle(Theme.text3)
                        .accessibilityHidden(true)
                }
                Text(row.currentName)
                    .font(Theme.Font.body(12.5)).foregroundStyle(Theme.text2)
                    .lineLimit(1).truncationMode(.middle)
                    .help(row.path)
            }
            .padding(.leading, row.isFollower ? 14 : 0)
            .frame(width: 190, alignment: .leading)
            Image(systemName: "arrow.right").font(.system(size: 10, weight: .semibold)).foregroundStyle(Theme.text3)
                .accessibilityHidden(true)
            if row.canCheck && row.isFollower {
                Text(edited)
                    .font(Theme.Font.body(12.5, .medium))
                    .foregroundStyle(Theme.gold.opacity(checked ? 0.8 : 0.4))
                    .lineLimit(1).truncationMode(.middle)
                    .padding(.horizontal, 7).padding(.vertical, 4)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .help("Follows the name above")
            } else if row.canCheck {
                TextField("New name", text: $edited)
                    .textFieldStyle(.plain)
                    .font(Theme.Font.body(12.5, .medium))
                    .foregroundStyle(Theme.gold)
                    .padding(.horizontal, 7).padding(.vertical, 4)
                    .background(Color.black.opacity(0.25), in: RoundedRectangle(cornerRadius: Theme.Radius.xs))
                    .disabled(!checked)
            } else {
                Text("Keeps its name").font(Theme.Font.body(12.5)).foregroundStyle(Theme.text3)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if let chip {
                Text(chip)
                    .font(Theme.Font.mono(11, .medium))
                    .foregroundStyle(Theme.gold)
                    .padding(.horizontal, 7).padding(.vertical, 2)
                    .background(Theme.gold.opacity(0.12), in: RoundedRectangle(cornerRadius: Theme.Radius.xs))
                    .lineLimit(1)
                    .fixedSize()
                    .help("Renamed together with this file")
            }
            Text(NameStyle.reasonChip(row))
                .help(NameStyle.learnedHelp(row.learned) ?? "")
                .font(Theme.Font.body(11.5, .medium))
                .foregroundStyle(Theme.ai)
                .padding(.horizontal, 7).padding(.vertical, 2)
                .background(Theme.ai.opacity(0.12), in: RoundedRectangle(cornerRadius: Theme.Radius.xs))
                .lineLimit(1)
                .fixedSize()
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
        .background(Color.white.opacity(row.canCheck ? 0.04 : 0.02), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
        .opacity(row.canCheck ? 1 : 0.5)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(label)
    }
}
