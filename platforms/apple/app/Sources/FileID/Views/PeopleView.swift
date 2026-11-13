// People tab: face-cluster viewer over `persons` + `face_prints`.
// Engine owns `runFaceClustering`; this view reads + names + merges.
import SwiftUI
import AppKit
import FileIDShared

struct PeopleView: View {
    let engine: EngineClient
    let store: ReadStore
    var onSwitchTab: (MainWindow.Tab) -> Void = { _ in }
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @State private var persons: [ReadStore.PersonRow] = []
    @State private var personByID: [Int64: ReadStore.PersonRow] = [:]
    @State private var totalFacePrints: Int = 0
    @State private var hiddenUnknownCount: Int = 0
    @State private var showHiddenUnknowns: Bool = false
    @State private var reloadTask: Task<Void, Never>?
    @State private var reloadPending: Bool = false

    /// Cards become checkboxes; "Merge selected" picks a target.
    @State private var mergeMode: Bool = false
    @State private var mergeChecked: Set<Int64> = []
    /// Same checkbox UX, but the bulk action sets `is_unknown = true`.
    @State private var unknownMode: Bool = false
    @State private var unknownChecked: Set<Int64> = []
    @State private var mergeStatus: String?
    @State private var statusOffersShowUnknowns: Bool = false
    @State private var mergeProgress: String?
    @State private var mergeStopRequested: Bool = false
    @State private var faceModelInstalled: Bool?
    /// A card dropped onto another; confirmed before anything is merged.
    @State private var pendingDragMerge: PendingDragMerge?
    private struct PendingDragMerge: Identifiable {
        let source: Int64
        let target: Int64
        let targetName: String
        var id: String { "\(source)-\(target)" }
    }

    @State private var suggestions: [MergeSuggestionPair] = []
    @State private var suggestionsLoading: Bool = false
    @State private var mergeInFlight: Bool = false

    /// Single sheet driver. Three stacked .sheet modifiers wedged the
    /// view graph on macOS 26 (blanked the sidebar on tab transition).
    enum ActiveSheet: Identifiable, Equatable {
        case personDetail(Int64)        // person id; we look up the row
        case suggestedMerges
        case mergeTargetPicker
        case faceReview
        var id: String {
            switch self {
            case .personDetail(let pid): return "personDetail-\(pid)"
            case .suggestedMerges:       return "suggestedMerges"
            case .mergeTargetPicker:     return "mergeTargetPicker"
            case .faceReview:            return "faceReview"
            }
        }
    }
    @State private var activeSheet: ActiveSheet?
    @State private var review: FaceReviewModel?
    @State private var mergeQueue: FaceQueueModel?
    @State private var splitQueue: FaceQueueModel?
    @State private var faceRefresh: FaceRefreshModel?

    var body: some View {
        VStack(spacing: 0) {
            if showsHeader {
                headerBlock
                Divider().opacity(0.15)
            }
            if mergeProgress != nil || mergeStatus != nil { statusRow }
            content
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { refreshFaceModelInstalled(); reload(); refreshReview(); refreshMergeUndo() }
        .onChangeWhileActive(of: store.version) { reload(debounced: true) }
        .onChange(of: ModelInstaller.face.status) { _, _ in refreshFaceModelInstalled() }
        .onChangeWhileActive(of: engine.lastFaceClustering?.durationSeconds) { reload(); refreshReview(); refreshMergeUndo() }
        .onPageVisibility(shown: { refreshFaceModelInstalled() })
        .onChange(of: activeSheet) { _, sheet in
            publishSelection(sheet)
            if sheet == nil { refreshReview(); refreshMergeUndo() }
        }
        .onChange(of: mergeChecked) { _, _ in publishSelection(activeSheet) }
        .modifier(FaceQueueObservers(jobs: engine.catalogJobs, changes: queueChanges, refresh: faceRefresh, onChange: { reload() }))
        .confirmationDialog(
            "Merge \"\(pendingDragMerge?.targetName ?? "")\" with the dropped person?",
            isPresented: Binding(get: { pendingDragMerge != nil }, set: { if !$0 { pendingDragMerge = nil } }),
            titleVisibility: .visible
        ) {
            Button("Merge", role: .destructive) {
                guard let merge = pendingDragMerge else { return }
                pendingDragMerge = nil
                Task {
                    if let n = await CatalogWrites.merge(engine, store: store, target: merge.target, sources: [merge.source]) {
                        setStatus("Merged into \(merge.targetName) (\(n) photos).")
                    } else {
                        setStatus(Self.mergeFailed)
                    }
                    reload()
                }
            }
            Button("Cancel", role: .cancel) { pendingDragMerge = nil }
        } message: {
            Text(Self.mergeWarning)
        }
        .sheet(item: $activeSheet) { sheet in
            switch sheet {
            case .personDetail(let pid):
                if let person = personByID[pid] {
                    PersonDetailSheet(person: person, engine: engine, store: store)
                } else {
                    Text("This person no longer exists.")
                        .padding(40)
                }
            case .suggestedMerges:
                SuggestedMergesSheet(
                    candidates: suggestions,
                    personByID: personByID,
                    store: store,
                    inFlight: mergeInFlight,
                    status: mergeStatus,
                    progress: mergeProgress,
                    stopRequested: mergeStopRequested,
                    onStop: { mergeStopRequested = true },
                    onAccept: { candidate in
                        guard !mergeInFlight else { return }
                        let a = personByID[candidate.sourcePersonID]
                        let b = personByID[candidate.destinationPersonID]
                        let target = preferredTarget(a, b) ?? a ?? b
                        let source = (target?.id == a?.id) ? b : a
                        if let t = target, let s = source {
                            let candidateID = candidate.id
                            let displayName = t.displayName
                            mergeInFlight = true
                            Task {
                                let n = await CatalogWrites.merge(engine, store: store, target: t.id, sources: [s.id])
                                if let n {
                                    setStatus("Merged into \"\(displayName)\" (\(n) photos).")
                                    suggestions.removeAll { $0.id == candidateID }
                                } else {
                                    setStatus(Self.mergeFailed)
                                }
                                mergeInFlight = false
                                reload()
                            }
                        } else {
                            suggestions.removeAll { $0.id == candidate.id }
                            reload()
                        }
                    },
                    onAcceptMany: { batch in
                        guard !mergeInFlight else { return }
                        // ReadStore's union-find resolves chains (A→B, B→C →
                        // A→C), so we just submit raw (target, source) pairs.
                        var pairs: [(target: Int64, source: Int64)] = []
                        pairs.reserveCapacity(batch.count)
                        for candidate in batch {
                            let a = personByID[candidate.sourcePersonID]
                            let b = personByID[candidate.destinationPersonID]
                            guard let target = preferredTarget(a, b) ?? a ?? b else { continue }
                            let source = (target.id == a?.id) ? b : a
                            guard let s = source, s.id != target.id else { continue }
                            pairs.append((target.id, s.id))
                        }
                        let batchToRemove = Set(batch.map(\.id))
                        guard !pairs.isEmpty else {
                            suggestions.removeAll { batchToRemove.contains($0.id) }
                            reload()
                            return
                        }
                        mergeInFlight = true
                        let submitted = pairs
                        Task {
                            let plan = await Task.detached(priority: .userInitiated) { [store] in store.mergeBatchPlan(submitted) }.value
                            let total = plan.reduce(0) { sum, group in sum + group.sources.filter { $0 != group.target }.count }
                            mergeStopRequested = false
                            let merged = await CatalogWrites.mergeBatch(
                                engine, store: store, plan: plan,
                                progress: { current, total in mergeProgress = "Merging \(current) of \(total)…" },
                                shouldStop: { mergeStopRequested })
                            mergeProgress = nil
                            let alive = await Task.detached(priority: .userInitiated) { [store] in
                                Set(store.persons(includeUnknown: true).map(\.id))
                            }.value
                            suggestions.removeAll {
                                batchToRemove.contains($0.id)
                                    && !(alive.contains($0.sourcePersonID) && alive.contains($0.destinationPersonID))
                            }
                            if merged == 0 {
                                setStatus(mergeStopRequested ? "Stopped. Nothing was merged." : Self.mergeFailed)
                            } else if merged < total {
                                setStatus(mergeStopRequested
                                    ? "Stopped after \(merged) of \(total) merges. The rest are still in the list."
                                    : "Merged \(merged) of \(total). The rest didn't go through and are still in the list.")
                            } else {
                                setStatus("Merged \(merged) \(merged == 1 ? "pair" : "pairs") of cards.")
                            }
                            mergeStopRequested = false
                            mergeInFlight = false
                            reload()
                        }
                    },
                    onDismiss: { activeSheet = nil }
                )
            case .faceReview:
                if let review, let mergeQueue, let splitQueue {
                    FaceReviewSheet(model: review, merges: mergeQueue, splits: splitQueue, personByID: personByID,
                                    store: store) { activeSheet = nil }
                }
            case .mergeTargetPicker:
                MergeTargetPickerSheet(
                    checked: Array(mergeChecked).compactMap { personByID[$0] }
                        .sorted { $0.displayName < $1.displayName },
                    onPick: { target in
                        let sources = mergeChecked.filter { $0 != target.id }.sorted()
                        let displayName = target.displayName
                        let sourceCount = sources.count
                        guard !mergeInFlight else { return }
                        mergeMode = false
                        mergeChecked.removeAll()
                        activeSheet = nil
                        mergeInFlight = true
                        mergeStopRequested = false
                        Task {
                            let before = await Task.detached(priority: .userInitiated) { [store] in
                                Set(store.persons(includeUnknown: true).map(\.id))
                            }.value
                            let newCount = await CatalogWrites.merge(
                                engine, store: store, target: target.id, sources: sources,
                                progress: { current, total in mergeProgress = total > 1 ? "Merging \(current) of \(total)…" : "Merging…" },
                                shouldStop: { mergeStopRequested })
                            mergeProgress = nil
                            let after = await Task.detached(priority: .userInitiated) { [store] in
                                Set(store.persons(includeUnknown: true).map(\.id))
                            }.value
                            let merged = sources.filter { before.contains($0) && !after.contains($0) }.count
                            if let newCount, merged == sourceCount {
                                setStatus("Combined \(sourceCount + 1) cards into \"\(displayName)\" (\(newCount) photos).")
                            } else if newCount != nil {
                                setStatus(mergeStopRequested
                                    ? "Stopped after \(merged) of \(sourceCount) merges into \"\(displayName)\". The other cards were left as they were."
                                    : "Merged \(merged) of \(sourceCount) cards into \"\(displayName)\". The rest didn't go through.")
                            } else {
                                setStatus(mergeStopRequested ? "Stopped. Nothing was merged." : Self.mergeFailed)
                            }
                            mergeStopRequested = false
                            mergeInFlight = false
                            reload()
                        }
                    },
                    onCancel: { activeSheet = nil }
                )
            }
        }
    }

    static let mergeFailed = "The merge didn't go through. Try again in a moment."
    static let mergeWarning = "Their photos become one person. You can undo this right after."

    private func setStatus(_ text: String?, offersShowUnknowns: Bool = false) {
        mergeStatus = text
        statusOffersShowUnknowns = offersShowUnknowns
    }

    private func refreshFaceModelInstalled() {
        faceModelInstalled = !FaceEmbedderKind.installedKinds().isEmpty
    }

    @ViewBuilder
    private var statusRow: some View {
        HStack(spacing: 10) {
            if let progress = mergeProgress {
                ProgressView().controlSize(.small)
                Text(progress)
                    .font(Theme.Font.body(12))
                    .foregroundStyle(Theme.text2)
                Button(mergeStopRequested ? "Stopping…" : "Stop") { mergeStopRequested = true }
                    .buttonStyle(DSSecondaryButtonStyle(height: 22))
                    .disabled(mergeStopRequested)
                    .help("Finish the merge that is running, then stop. Merges already done stay.")
            } else if let status = mergeStatus {
                Text(status)
                    .font(Theme.Font.body(12))
                    .foregroundStyle(Theme.text3)
                if statusOffersShowUnknowns, hiddenUnknownCount > 0, !showHiddenUnknowns {
                    Button("Show them") {
                        showHiddenUnknowns = true
                        reload()
                    }
                    .buttonStyle(DSSecondaryButtonStyle(height: 22))
                    .help("Show the people you marked as unknown. To bring one back, open the card and untick \"I don't know who this is\".")
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
    }

    // MARK: - Header

    @ViewBuilder
    private var reviewButton: some View {
        let waiting = (review?.total ?? 0) + (mergeQueue?.total ?? 0) + (splitQueue?.total ?? 0)
        if waiting > 0, !mergeMode, !unknownMode {
            Button { activeSheet = .faceReview } label: {
                Label("\(FaceReviewText.title) (\(waiting))", systemImage: "person.crop.circle.badge.checkmark")
            }
            .buttonStyle(.dsAccentOutline)
            .accessibilityLabel(FaceReviewText.header(waiting))
            .help("New faces that look like people you already have. Nothing is assigned until you accept.")
        }
    }

    private func refreshMergeUndo() {
        Task { await MergeUndoModel.shared.refresh(engine) }
    }

    @ViewBuilder
    private var undoMergeButton: some View {
        let undo = MergeUndoModel.shared
        if undo.operationID != nil, !mergeMode, !unknownMode {
            Button {
                Task {
                    let result = await undo.undoLatest(engine, store: store)
                    setStatus(MergeUndoModel.summary(result, of: 1))
                    reload()
                }
            } label: {
                Label("Undo last merge", systemImage: "arrow.uturn.backward")
            }
            .buttonStyle(.dsSecondary)
            .disabled(undo.busy || mergeInFlight)
            .help("Separate the two people you merged last. Their names and photos go back to how they were. Works for a day after the merge.")
        }
    }

    private var queueChanges: Int { (mergeQueue?.changes ?? 0) + (splitQueue?.changes ?? 0) }

    private func refreshReview() {
        let model = review ?? FaceReviewModel(engine: engine)
        let merges = mergeQueue ?? FaceQueueModel(engine: engine, kind: .merges)
        let splits = splitQueue ?? FaceQueueModel(engine: engine, kind: .splits)
        let refresh = faceRefresh ?? FaceRefreshModel(engine: engine)
        review = model; mergeQueue = merges; splitQueue = splits; faceRefresh = refresh
        Task {
            await model.reload()
            await merges.reload()
            await splits.reload()
            await refresh.reload()
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                MetaText(headerCountLine, color: Theme.text2)
                Spacer()
                reviewButton
                undoMergeButton
                headerActions
            }
            if let faceRefresh { FaceRefreshRow(model: faceRefresh) }
            if let count = engine.lastFaceClustering?.deferredFaces, count > 0 {
                HStack(spacing: 10) {
                    Text("\(count) more face\(count == 1 ? " is" : "s are") waiting to be grouped. People you marked as unknown are left out.")
                        .font(Theme.Font.body(13.5))
                        .foregroundStyle(Theme.text3)
                    Button("Group faces") { engine.runFaceClustering() }
                        .buttonStyle(DSSecondaryButtonStyle(height: 24))
                        .disabled(engine.faceClusteringInFlight)
                        .help("Sort the waiting faces into people. Names you gave are kept.")
                }
            }
            if mergeMode || unknownMode {
                bulkActionStrip
            }
        }
    }

    /// True when there are person clusters but none of them have been
    /// given a name yet AND nothing's already running. Drives the
    /// visibility of the "Skip naming" row — once at least one person
    /// is named OR Deep Analyze is already in flight, the skip option
    /// is irrelevant noise and goes away.
    private var canSkipNaming: Bool {
        guard !persons.isEmpty else { return false }
        guard !engine.deepAnalyzeInFlight else { return false }
        return !persons.contains { $0.hasAnyName }
    }

    /// Item 2: true once at least one cluster is named (and nothing's running).
    /// Gates the positive "Continue to Deep Analyze" forward affordance — the
    /// counterpart to canSkipNaming. Mutually exclusive with it in the header.
    private var canContinueToDeepAnalyze: Bool {
        guard !persons.isEmpty else { return false }
        guard !engine.deepAnalyzeInFlight else { return false }
        return persons.contains { $0.hasAnyName }
    }

    @ViewBuilder
    private var headerActions: some View {
        if mergeMode || unknownMode {
            Button("Cancel") {
                mergeMode = false; unknownMode = false
                mergeChecked.removeAll(); unknownChecked.removeAll()
            }
            .buttonStyle(.dsSecondary)
        } else if persons.count >= 2 {
            Button {
                suggestionsLoading = true
                Task {
                    let reply = await engine.findMergeSuggestions()
                    suggestionsLoading = false
                    guard let result = reply else {
                        setStatus("Couldn't look for likely merges right now. Try again in a moment.")
                        return
                    }
                    // The scan is multi-second; don't preempt whatever the
                    // user navigated to while it ran. Only surface the sheet
                    // if they're still on a neutral People view — no sheet
                    // already up, not mid merge/unknown selection.
                    guard activeSheet == nil, !mergeMode, !unknownMode else { return }
                    suggestions = result
                    setStatus(nil)
                    activeSheet = .suggestedMerges
                }
            } label: {
                Label(suggestionsLoading ? "Looking for the same person twice…" : "Suggest merges",
                      systemImage: suggestionsLoading ? "hourglass" : "sparkle.magnifyingglass")
            }
            .buttonStyle(.dsPrimary)
            .tint(Theme.gold)
            .disabled(suggestionsLoading || mergeInFlight)
            .help("Find cards that might show the same person, so you can combine them.")

            Button {
                mergeMode = true; mergeChecked.removeAll()
            } label: {
                Label("Merge people", systemImage: "arrow.triangle.merge")
            }
            .buttonStyle(.dsSecondary)
            .help("Combine two or more cards that show the same person")

            Button {
                unknownMode = true; unknownChecked.removeAll()
            } label: {
                Label("Mark unknown", systemImage: "person.crop.circle.badge.questionmark")
            }
            .buttonStyle(.dsSecondary)
            .help("Hide people you don't need to name, like strangers in a crowd. Their photos stay in the library.")
        }
    }

    @ViewBuilder
    private var bulkActionStrip: some View {
        HStack(spacing: 10) {
            if mergeMode {
                Text("\(mergeChecked.count) selected to merge")
                    .font(Theme.Font.body(13.5))
                    .foregroundStyle(Theme.text3)
                Spacer()
                Button {
                    if mergeChecked.count >= 2 { activeSheet = .mergeTargetPicker }
                } label: {
                    Label("Merge \(mergeChecked.count) selected", systemImage: "arrow.triangle.merge")
                }
                .buttonStyle(.dsPrimary)
                .tint(Theme.gold)
                .disabled(mergeChecked.count < 2)
            } else if unknownMode {
                Text("\(unknownChecked.count) selected to mark unknown")
                    .font(Theme.Font.body(13.5))
                    .foregroundStyle(Theme.text3)
                Spacer()
                Button {
                    bulkMarkUnknown()
                } label: {
                    Label("Mark \(unknownChecked.count) as unknown", systemImage: "person.crop.circle.badge.questionmark")
                }
                .buttonStyle(.dsPrimary)
                .tint(.gray)
                .disabled(unknownChecked.isEmpty)
            }
        }
        .padding(.top, 4)
    }

    private var headerCountLine: String {
        let p = persons.count
        let unnamed = persons.filter { !$0.hasAnyName }.count
        if p == 0 && totalFacePrints == 0 { return "" }
        if p == 0 { return engine.faceClusteringInFlight ? "\(totalFacePrints.formatted()) faces · grouping…" : "\(totalFacePrints.formatted()) faces detected · not grouped yet" }
        if unnamed > 0 { return "\(p) people · \(unnamed) still unnamed" }
        return "\(p) people · all named"
    }

    private func bulkMarkUnknown() {
        let ids = unknownChecked.sorted()
        guard !ids.isEmpty else { return }
        unknownMode = false
        unknownChecked.removeAll()
        Task {
            let n = await CatalogWrites.markUnknown(engine, store: store, ids: ids)
            if n > 0 {
                setStatus("Marked \(n) \(n == 1 ? "person" : "people") as unknown and hid \(n == 1 ? "that card" : "those cards"). To bring one back, show them, open the card and untick \"I don't know who this is\".",
                          offersShowUnknowns: true)
            } else {
                setStatus("Marking as unknown didn't go through. Try again in a moment.")
            }
            reload()
        }
    }

    @ViewBuilder
    private var headerBlock: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            // Surface the "skip naming" escape hatch right at the top
            // of the People tab — that's where the user is when they
            // realize they don't want to name anyone. Quiet styling so
            // it doesn't compete with the recommended naming flow, but
            // visible enough that it's discoverable.
            if canSkipNaming {
                skipNamingRow
            } else if canContinueToDeepAnalyze {
                continueToDeepAnalyzeRow
            }
        }
        .padding(.horizontal, Theme.Metrics.pageInsetX)
        .padding(.vertical, 14)
    }

    /// Inline notice + button that lets the user bypass naming and
    /// run Deep Analyze immediately. Only shown when at least one
    /// person cluster exists but none have been named yet.
    @ViewBuilder
    private var skipNamingRow: some View {
        HStack(spacing: 10) {
            Image(systemName: "forward.circle")
                .foregroundStyle(Theme.text3)
                .font(Theme.Font.body(13.5))
            Text("Don't want to name anyone? Descriptions will say \"a person\" instead of a name.")
                .font(Theme.Font.body(12))
                .foregroundStyle(Theme.text3)
            Button {
                let kind = DeepAnalyzeSettings.shared.activeKind
                guard ModelLicenseGate.ensureAccepted(for: kind) else { return }
                engine.deepAnalyzeAll(modelKind: kind.rawValue, skipExisting: true)
                onSwitchTab(.deep)
            } label: {
                Label("Describe files without names",
                      systemImage: "forward.fill")
                    .font(Theme.Font.body(12, .semibold))
                    .padding(.horizontal, 10).padding(.vertical, 5)
                    .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Color.secondary.opacity(0.6), lineWidth: 1))
                    .foregroundStyle(Theme.text3)
            }
            .buttonStyle(.plain)
            .disabled(engine.deepAnalyzeInFlight || !engine.deepAnalyzeAvailable)
            .help(engine.deepAnalyzeInFlight ? "FileID is already describing files." : "Start describing every file now. You can name people later and describe again.")
            Spacer()
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Color.secondary.opacity(0.08)))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Color.secondary.opacity(0.20), lineWidth: 0.5))
    }

    /// Item 2: positive "keep moving" affordance — once at least one person is
    /// named, advance straight to Deep Analyze (which uses those names in its
    /// captions + smart filenames). The forward counterpart to skipNamingRow:
    /// "if they have named someone, switch it to continue."
    @ViewBuilder
    private var continueToDeepAnalyzeRow: some View {
        HStack(spacing: 10) {
            Image(systemName: "checkmark.seal.fill")
                .foregroundStyle(Theme.gold)
                .font(Theme.Font.body(13.5))
            Text("Names saved. Next, FileID can describe your files using these names.")
                .font(Theme.Font.body(12))
                .foregroundStyle(Theme.text3)
            Button {
                let kind = DeepAnalyzeSettings.shared.activeKind
                guard ModelLicenseGate.ensureAccepted(for: kind) else { return }
                engine.deepAnalyzeAll(modelKind: kind.rawValue, skipExisting: true)
                onSwitchTab(.deep)
            } label: {
                Label("Describe files", systemImage: "arrow.forward.circle.fill")
                    .font(Theme.Font.body(12, .semibold))
                    .padding(.horizontal, 10).padding(.vertical, 5)
                    .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.gold))
                    .foregroundStyle(.black)
            }
            .buttonStyle(.plain)
            .disabled(engine.deepAnalyzeInFlight || !engine.deepAnalyzeAvailable)
            .help(engine.deepAnalyzeInFlight ? "FileID is already describing files." : "Start writing a description and a suggested name for every file. Progress shows on the Describe Files page.")
            Spacer()
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.gold.opacity(0.10)))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.gold.opacity(0.30), lineWidth: 0.5))
    }

    // MARK: - Content

    @ViewBuilder
    private var content: some View {
        if !(faceModelInstalled ?? !FaceEmbedderKind.installedKinds().isEmpty) {
            modelMissingBanner
        } else if persons.isEmpty && totalFacePrints == 0 {
            emptyState
        } else if persons.isEmpty {
            noClustersYet
        } else {
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 160, maximum: 200), spacing: 14)], spacing: 14) {
                    ForEach(persons) { person in
                        let inMode = mergeMode || unknownMode
                        let checked = mergeMode ? mergeChecked.contains(person.id)
                                                : unknownChecked.contains(person.id)
                        PersonCard(person: person, store: store,
                                   selectionMode: inMode,
                                   isChecked: checked,
                                   selectionTint: unknownMode ? Color.gray : Theme.gold)
                            .onTapGesture {
                                if mergeMode {
                                    if mergeChecked.contains(person.id) { mergeChecked.remove(person.id) }
                                    else { mergeChecked.insert(person.id) }
                                } else if unknownMode {
                                    if unknownChecked.contains(person.id) { unknownChecked.remove(person.id) }
                                    else { unknownChecked.insert(person.id) }
                                } else {
                                    activeSheet = .personDetail(person.id)
                                }
                            }
                            // Drag a person card onto another to merge
                            // them. Source person's photos move into
                            // target; source row is deleted. Disabled
                            // while in merge / mark-unknown checkbox
                            // mode — otherwise an accidental drag
                            // mid-selection would merge instead of
                            // toggling a checkbox.
                            .modifier(PersonCardDragMergeModifier(
                                enabled: !mergeMode && !unknownMode,
                                personID: person.id,
                                personName: person.displayName,
                                engine: engine,
                                store: store,
                                onDropped: { sourceID in
                                    pendingDragMerge = PendingDragMerge(source: sourceID, target: person.id, targetName: person.displayName)
                                }
                            ))
                            // Spring entrance: cards scale + fade in when
                            // they appear. Disabled when the user has
                            // "Reduce Motion" turned on — instant fade
                            // only, no scale.
                            .transition(reduceMotion
                                ? .opacity
                                : .asymmetric(
                                    insertion: .scale(scale: 0.92).combined(with: .opacity),
                                    removal: .opacity
                                  ))
                    }
                }
                .padding(.horizontal, Theme.Metrics.pageInsetX)
                .padding(.vertical, 16)
                // Animate on count to avoid the per-render array
                // allocation that mapping IDs would cause.
                .animation(reduceMotion
                    ? .easeOut(duration: 0.15)
                    : .spring(response: 0.35, dampingFraction: 0.78),
                            value: persons.count)
                hiddenUnknownsFooter
            }
        }
    }

    /// Subtle footer revealing the count of unknown-marked people,
    /// with a toggle to show / hide them. Hidden by default so the
    /// grid stays clean (the user's whole point of marking them).
    @ViewBuilder
    private var hiddenUnknownsFooter: some View {
        if hiddenUnknownCount > 0 {
            HStack(spacing: 8) {
                Image(systemName: showHiddenUnknowns
                      ? "eye.slash" : "person.crop.circle.badge.questionmark")
                    .foregroundStyle(Theme.text3)
                Text(showHiddenUnknowns
                     ? "\(hiddenUnknownCount) marked unknown — currently visible"
                     : "\(hiddenUnknownCount) hidden as unknown")
                    .font(Theme.Font.body(12))
                    .foregroundStyle(Theme.text3)
                Spacer()
                Button(showHiddenUnknowns ? "Hide them" : "Show them") {
                    showHiddenUnknowns.toggle()
                    reload()
                }
                .buttonStyle(.borderless)
                .font(Theme.Font.body(12))
                .help("People you marked as unknown are hidden here. Show them to change your mind.")
            }
            .padding(.horizontal, Theme.Metrics.pageInsetX)
            .padding(.vertical, 8)
        }
    }

    private var showsHeader: Bool {
        !persons.isEmpty || totalFacePrints > 0 || mergeMode || unknownMode || (review?.total ?? 0) > 0
            || (engine.lastFaceClustering?.deferredFaces ?? 0) > 0
    }

    private var emptyState: some View {
        let images = store.totalImages
        return EmptyStateView(
            icon: "person.2.crop.square.stack",
            title: images > 0 ? "No faces found" : "No people yet",
            message: images > 0
                ? "FileID looked at \(images.formatted()) photo\(images == 1 ? "" : "s") in this library and didn't find any faces. People appear here when a scanned photo has one."
                : "Scan a folder with photos. Faces are detected as the scan runs and grouped into people you can name."
        )
    }

    @ViewBuilder
    private var noClustersYet: some View {
        VStack(spacing: 14) {
            EmptyStateView(
                icon: "person.crop.circle.badge.questionmark",
                title: "\(totalFacePrints) faces found",
                message: "Press Group faces to sort them into one card per person. It takes a few seconds.",
                primaryAction: engine.faceClusteringInFlight ? nil : ("Group faces", { engine.runFaceClustering() }),
                secondaryMessage: "Then click a card to type that person's name."
            )
            if engine.faceClusteringInFlight {
                ProgressView("Grouping faces…")
                    .padding(.top, 4)
            }
        }
    }

    private var modelMissingBanner: some View {
        FaceModelInstallPanel(engine: engine, detectedFaces: totalFacePrints)
            .frame(maxWidth: 560)
            .padding(.top, 40)
            .frame(maxWidth: .infinity)
    }

    // MARK: - Reload

    /// The person open in detail, or the people checked for merging, so Chat
    /// can resolve "this person" and "these people".
    private func publishSelection(_ sheet: ActiveSheet?) {
        if case .personDetail(let id) = sheet {
            ChatHandoff.shared.selectedPersonIDs = [id]
        } else {
            ChatHandoff.shared.selectedPersonIDs = mergeChecked.sorted()
        }
    }

    /// `debounced` is for catalog change notifications, which arrive in
    /// bursts during a scan; each would otherwise re-run the people query.
    /// A pending debounced reload is left to fire rather than pushed back,
    /// so a steady stream of changes still refreshes the grid.
    private func reload(debounced: Bool = false) {
        if debounced && reloadPending { return }
        let includeUnknown = showHiddenUnknowns
        reloadTask?.cancel()
        reloadPending = debounced
        reloadTask = Task {
            if debounced {
                try? await Task.sleep(for: .milliseconds(300))
                guard !Task.isCancelled else { return }
                reloadPending = false
            }
            // Drop emptied clusters: moving every face out of a person leaves a
            // 0-face row whose stale representative_face_id now points at the
            // target's face — a ghost card showing the wrong person. Hide it.
            let loaded = await Task.detached(priority: .userInitiated) { [store] in
                (faces: store.totalFacePrints(),
                 rows: store.persons(includeUnknown: includeUnknown).filter { $0.faceCount > 0 },
                 hidden: store.hiddenUnknownCount())
            }.value
            guard !Task.isCancelled else { return }
            totalFacePrints = loaded.faces
            persons = loaded.rows
            hiddenUnknownCount = loaded.hidden
            // `merging:` to tolerate duplicate ids (uniqueKeysWithValues traps).
            personByID = Dictionary(loaded.rows.map { ($0.id, $0) }, uniquingKeysWith: { lhs, _ in lhs })
        }
    }

    /// Named cluster wins (so a typed name sticks); else the larger count.
    private func preferredTarget(_ a: ReadStore.PersonRow?,
                                  _ b: ReadStore.PersonRow?) -> ReadStore.PersonRow? {
        switch (a, b) {
        case (nil, nil): return nil
        case (nil, let b?): return b
        case (let a?, nil): return a
        case (let a?, let b?):
            let aNamed = a.hasAnyName
            let bNamed = b.hasAnyName
            if aNamed && !bNamed { return a }
            if bNamed && !aNamed { return b }
            return a.fileCount >= b.fileCount ? a : b
        }
    }
}

// MARK: - Card

/// Draggable + drop-target modifier on a PersonCard. Splits the
/// drag-merge logic out of PeopleView's body so we can conditionally
/// apply it (disabled in merge / mark-unknown selection modes).
private struct PersonCardDragMergeModifier: ViewModifier {
    let enabled: Bool
    let personID: Int64
    let personName: String
    let engine: EngineClient
    let store: ReadStore
    let onDropped: @MainActor (Int64) -> Void

    func body(content: Content) -> some View {
        if enabled {
            content
                .draggable("\(personID)")
                .dropDestination(for: String.self) { items, _ in
                    guard let s = items.first,
                          let sourceID = Int64(s),
                          sourceID != personID else { return false }
                    onDropped(sourceID)
                    return true
                } isTargeted: { _ in }
        } else {
            content
        }
    }
}

private struct PersonCard: View {
    let person: ReadStore.PersonRow
    let store: ReadStore
    var selectionMode: Bool = false
    var isChecked: Bool = false
    /// Gold for merge, gray for "mark unknown".
    var selectionTint: Color = Theme.gold

    @State private var thumbnail: NSImage?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Color.white.opacity(person.hasAnyName ? 0.04 : 0.03)
                .aspectRatio(1, contentMode: .fit)
                .overlay(faceCrop)
                .overlay(LinearGradient(colors: [.white.opacity(0.06), .black.opacity(0.3)], startPoint: .top, endPoint: .bottom))
                .overlay(selectionOverlay)
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                .overlay {
                    if person.hasAnyName {
                        RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairline, lineWidth: 1)
                    } else {
                        RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(style: StrokeStyle(lineWidth: 1, dash: [4, 3])).foregroundStyle(Color.white.opacity(0.2))
                    }
                }
            Text(displayName)
                .font(Theme.Font.body(14, .semibold))
                .foregroundStyle(person.hasAnyName ? Theme.text1 : Theme.text3)
                .lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
            MetaText("\(person.fileCount) photo\(person.fileCount == 1 ? "" : "s") · \(person.faceCount) face\(person.faceCount == 1 ? "" : "s")", size: 11)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: thumbnailIdentity) {
            await loadThumb()
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibleDescription)
        .accessibilityAddTraits(selectionMode ? [.isButton, isChecked ? .isSelected : []] : [.isButton])
        .accessibilityHint(accessibleHint)
    }

    private var accessibleDescription: String {
        let name = displayName
        let photos = "\(person.fileCount) photo\(person.fileCount == 1 ? "" : "s")"
        return "\(name), \(photos)"
    }
    private var accessibleHint: String {
        if selectionMode {
            return isChecked ? "Selected. Tap to deselect." : "Tap to select."
        }
        return "Opens photos and naming for this person."
    }

    private var displayName: String { person.displayName }

    @ViewBuilder
    private var faceCrop: some View {
        if let img = thumbnail {
            Image(nsImage: img)
                .resizable()
                .scaledToFill()
        } else {
            Text(person.hasAnyName ? String(displayName.split(separator: " ").prefix(2).compactMap(\.first)) : "?")
                .font(Theme.Font.display(44))
                .foregroundStyle(person.hasAnyName ? Theme.text1 : Theme.text3)
        }
    }

    @ViewBuilder
    private var selectionOverlay: some View {
        if selectionMode {
            ZStack {
                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                    .stroke(isChecked ? selectionTint : Color.white.opacity(0.25),
                            lineWidth: isChecked ? 3 : 1.5)
                if isChecked {
                    RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                        .fill(selectionTint.opacity(0.18))
                }
                VStack {
                    HStack { DSCheck(on: isChecked).padding(8); Spacer() }
                    Spacer()
                }
            }
        }
    }

    private var thumbnailIdentity: String {
        [String(person.id), String(person.representativeFaceID ?? 0),
         person.representativePath ?? "", person.representativeBBox ?? "",
         person.representativeCacheRevision ?? ""].joined(separator: "|")
    }

    private func loadThumb() async {
        guard !Task.isCancelled else { return }
        let key = thumbnailIdentity
        if let cached = FaceCropCache.image(for: key) {
            thumbnail = cached
            return
        }
        thumbnail = nil
        guard let path = person.representativePath else { return }
        let url = URL(fileURLWithPath: path)
        guard let full = await ThumbnailService.shared.thumbnail(for: url, size: 360,
                  revision: person.representativeCacheRevision), !Task.isCancelled else { return }
        guard let bbox = person.representativeBBox else {
            FaceCropCache.store(full, for: key)
            thumbnail = full
            return
        }
        let orientation: Int
        if bbox.trimmingCharacters(in: .whitespacesAndNewlines).hasPrefix("{") {
            guard let value = await ThumbnailService.sourceOrientation(for: url), !Task.isCancelled else {
                if !Task.isCancelled { thumbnail = full }
                return
            }
            orientation = value
        } else {
            orientation = 1
        }
        guard !Task.isCancelled else { return }
        let box = UncheckedImage(full)
        let cropped = await Task.detached(priority: .userInitiated) {
            ThumbnailService.cropFace(in: box.image, bbox: bbox, sourceOrientation: orientation).map(UncheckedImage.init)
        }.value
        guard !Task.isCancelled else { return }
        let face = cropped?.image ?? full
        FaceCropCache.store(face, for: key)
        thumbnail = face
    }

}

/// Face crops by card identity. A card's `.task` restarts each time the lazy
/// grid scrolls it back into view; without this it re-read the photo's
/// orientation from disk and cropped again on every pass.
@MainActor
enum FaceCropCache {
    private static let cache: NSCache<NSString, NSImage> = {
        let cache = NSCache<NSString, NSImage>()
        cache.totalCostLimit = 64 * 1024 * 1024
        return cache
    }()

    static func image(for key: String) -> NSImage? { cache.object(forKey: key as NSString) }

    static func store(_ image: NSImage, for key: String) {
        let pixels = image.representations.first.map { $0.pixelsWide * $0.pixelsHigh } ?? Int(image.size.width * image.size.height * 4)
        cache.setObject(image, forKey: key as NSString, cost: max(pixels, 1) * 4)
    }
}

// MARK: - Person detail sheet

private struct PersonDetailSheet: View {
    let person: ReadStore.PersonRow
    let engine: EngineClient
    let store: ReadStore
    @Environment(\.dismiss) private var dismiss

    @State private var files: [FileRow] = []

    private func loadFiles() {
        let id = person.id
        Task {
            files = await Task.detached(priority: .userInitiated) { [store] in store.files(forPersonID: id) }.value
        }
    }
    @State private var title: String = ""
    @State private var firstName: String = ""
    @State private var middleName: String = ""
    @State private var lastName: String = ""
    @State private var suffix: String = ""
    @State private var isUnknown: Bool = false
    @State private var tagBatchStatus: String?
    @State private var tagBatchInFlight: Bool = false
    // Multi-select for moving photos to another person's cluster.
    // Common case: clusterer put a photo of Adam in Jack's cluster;
    // the user opens Jack, selects the wrong photos, picks Adam.
    @State private var selectMode: Bool = false
    @State private var checked: Set<Int64> = []
    @State private var showMoveTargetPicker: Bool = false
    @State private var moveStatus: String?
    @State private var pendingTag: PendingTag?
    private struct PendingTag {
        let adding: String
        var removing: String? = nil
    }

    @ViewBuilder
    private var photoGridToolbar: some View {
        HStack(spacing: 8) {
            if selectMode {
                Text("\(checked.count) selected")
                    .font(Theme.Font.mono(11))
                    .foregroundStyle(Theme.text3)
                Spacer()
                Button {
                    showMoveTargetPicker = true
                } label: {
                    Label("Move to another person…",
                          systemImage: "arrow.right.circle.fill")
                        .font(Theme.Font.body(12, .semibold))
                }
                .buttonStyle(.dsPrimary)
                .tint(Theme.gold)
                .disabled(checked.isEmpty)
                Button("Cancel") {
                    selectMode = false
                    checked.removeAll()
                }
                .buttonStyle(.dsSecondary)
                .keyboardShortcut(.escape, modifiers: [])
            } else if files.count >= 2 {
                Spacer()
                Button {
                    selectMode = true
                    checked.removeAll()
                    moveStatus = nil
                } label: {
                    Label("Select photos", systemImage: "checkmark.square")
                        .font(Theme.Font.body(12))
                }
                .buttonStyle(.dsSecondary)
                .help("Move photos that show someone else to the right person")
            } else {
                Spacer()
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
    }

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .firstTextBaseline) {
                    Text(headerTitle)
                        .font(Theme.Font.body(20, .semibold))
                    Spacer()
                    MetaText("\(person.fileCount) photo\(person.fileCount == 1 ? "" : "s") · \(person.faceCount) face\(person.faceCount == 1 ? "" : "s")")
                    Button("Done") { dismiss() }
                        .buttonStyle(.dsPrimary)
                        .keyboardShortcut(.defaultAction)
                }
                Toggle(isOn: $isUnknown) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text("I don't know who this is")
                            .font(Theme.Font.body(13.5))
                        Text("Hides this person and leaves them out of descriptions. Their photos stay in the library.")
                            .font(Theme.Font.body(11))
                            .foregroundStyle(Theme.text3)
                    }
                }
                .toggleStyle(DSCheckboxToggleStyle())
                if !isUnknown {
                    Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 8) {
                        GridRow {
                            namedField("Title", placeholder: "Uncle, Grandma…", text: $title)
                            namedField("First name", placeholder: "optional", text: $firstName)
                            namedField("Middle", placeholder: "optional", text: $middleName)
                        }
                        GridRow {
                            namedField("Last name", placeholder: "optional", text: $lastName)
                            namedField("Suffix", placeholder: "Jr, III…", text: $suffix)
                            Color.clear   // grid alignment only
                        }
                    }
                    Text("Descriptions will call this person **“\(deepAnalyzeRef)”**.")
                        .font(Theme.Font.body(11.5))
                        .foregroundStyle(Theme.text3)
                }
                // Only a real name is worth writing into Finder tags; "Person 1" is a placeholder.
                if !isUnknown && person.fileCount > 0 && hasTypedName {
                    tagAllPhotosButton
                }
            }
            .padding(16)
            FaceReviewSection(engine: engine, person: person, onChange: loadFiles)
            Divider()
            // Photo grid + multi-select toolbar. Toolbar only shows when
            // we have ≥ 2 photos and ≥ 2 named persons in total — moving
            // makes no sense otherwise.
            photoGridToolbar
            if let status = moveStatus {
                Text(status).font(Theme.Font.body(12))
                    .foregroundStyle(Theme.text3)
                    .padding(.horizontal, 16).padding(.vertical, 4)
            }
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 140), spacing: 8)], spacing: 8) {
                    ForEach(files) { f in
                        PersonFileTile(
                            file: f,
                            selectMode: selectMode,
                            isChecked: checked.contains(f.id)
                        )
                            .onTapGesture {
                                if selectMode {
                                    if checked.contains(f.id) { checked.remove(f.id) }
                                    else { checked.insert(f.id) }
                                } else {
                                    NSWorkspace.shared.open(f.url)
                                }
                            }
                    }
                }
                .padding(16)
            }
        }
        .sheet(isPresented: $showMoveTargetPicker) {
            MovePhotosTargetPicker(
                sourcePerson: person,
                engine: engine,
                store: store,
                fileIDs: Array(checked),
                onMoved: { count, targetName in
                    moveStatus = "Moved \(count) photo\(count == 1 ? "" : "s") to \(targetName)."
                    selectMode = false
                    checked.removeAll()
                    loadFiles()
                }
            )
        }
        .confirmationDialog(
            pendingTagTitle,
            isPresented: Binding(get: { pendingTag != nil }, set: { if !$0 { pendingTag = nil } }),
            titleVisibility: .visible
        ) {
            Button(pendingTag?.removing == nil ? "Add tag" : "Replace tag") {
                guard let tag = pendingTag else { return }
                pendingTag = nil
                if let old = tag.removing { retagAllPhotos(removing: old, adding: tag.adding) }
                else { applyTagToAllPhotos(tag.adding) }
            }
            Button("Cancel", role: .cancel) { pendingTag = nil }
        } message: {
            Text(pendingTagMessage)
        }
        .frame(minWidth: 720, minHeight: 520)
        .onAppear {
            loadFiles()
            title = person.title ?? ""
            firstName = person.firstName ?? person.name ?? ""
            middleName = person.middleName ?? ""
            lastName = person.lastName ?? ""
            suffix = person.suffix ?? ""
            isUnknown = person.isUnknown
        }
        .onDisappear(perform: saveName)
    }

    private func saveName() {
        let fields = [title, firstName, middleName, lastName, suffix].map { $0.trimmingCharacters(in: .whitespaces) }
        let saved = [person.title, person.firstName ?? person.name, person.middleName, person.lastName, person.suffix]
            .map { ($0 ?? "").trimmingCharacters(in: .whitespaces) }
        guard isUnknown != person.isUnknown || (!isUnknown && fields != saved) else { return }
        let id = person.id, unknown = isUnknown, shownName = person.displayName
        Task { [engine, store] in
            if unknown {
                if await CatalogWrites.markUnknown(engine, store: store, ids: [id]) == 0 {
                    ToastCenter.shared.show("Couldn't mark \(shownName) as unknown. Open the card and try again.", duration: 8)
                }
            } else {
                let value: (Int) -> String? = { fields[$0].isEmpty ? nil : fields[$0] }
                let saved = await CatalogWrites.renamePerson(engine, store: store, id: id, title: value(0), firstName: value(1),
                                                             middleName: value(2), lastName: value(3), suffix: value(4))
                if !saved {
                    ToastCenter.shared.show("The name for \(shownName) wasn't saved. Open the card and type it again.", duration: 8)
                }
            }
        }
    }

    private var pendingTagTitle: String {
        guard let tag = pendingTag else { return "" }
        let count = files.count
        let noun = "\(count.formatted()) file\(count == 1 ? "" : "s")"
        if let old = tag.removing { return "Replace the tag \"\(old)\" with \"\(tag.adding)\" on \(noun)?" }
        return "Add the tag \"\(tag.adding)\" to \(noun)?"
    }

    private var pendingTagMessage: String {
        guard let tag = pendingTag else { return "" }
        let count = files.count
        let these = count == 1 ? "this photo" : "all \(count.formatted()) photos of this person"
        if let old = tag.removing {
            return "FileID removes the Finder tag \"\(old)\" from \(these) and adds \"\(tag.adding)\". Other tags are kept. You can change tags later in Finder."
        }
        return "FileID writes the Finder tag \"\(tag.adding)\" onto \(these). It shows in Finder and Spotlight. Other tags are kept, and you can remove it later in Finder."
    }

    @ViewBuilder
    private var tagAllPhotosButton: some View {
        let tagName = primaryTagName
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Button {
                    pendingTag = PendingTag(adding: tagName)
                } label: {
                    Label(tagBatchInFlight
                          ? "Tagging…"
                          : "Tag all \(person.fileCount) photo\(person.fileCount == 1 ? "" : "s") with \"\(tagName)\"",
                          systemImage: "tag.fill")
                }
                .buttonStyle(.dsSecondary)
                .disabled(tagBatchInFlight || tagName.isEmpty || files.isEmpty)
                .help("Adds the Finder tag \"\(tagName)\" to every photo of this person. Visible in Finder, Spotlight, and Smart Folders.")

                // re-tag affordance. Shows only when this person
                // was previously tagged with a DIFFERENT name (e.g. user
                // renamed "Alex" → "Alex Doe" after a tag pass).
                if let oldTag = previousTagIfDifferent(currentTag: tagName) {
                    Button {
                        pendingTag = PendingTag(adding: tagName, removing: oldTag)
                    } label: {
                        Label("Replace \"\(oldTag)\" with \"\(tagName)\"",
                              systemImage: "arrow.triangle.2.circlepath")
                    }
                    .buttonStyle(.dsSecondary)
                    .disabled(tagBatchInFlight || files.isEmpty)
                    .help("Photos were tagged \"\(oldTag)\" earlier. Removes the old tag and adds the new one.")
                }
            }
            if let s = tagBatchStatus {
                Text(s).font(Theme.Font.body(11)).foregroundStyle(Theme.text3)
            }
        }
    }

    /// nil unless we previously tagged this person AND the previous tag
    /// is different from the current display name.
    private func previousTagIfDifferent(currentTag: String) -> String? {
        guard let prev = BulkRenameSheet.lastPersonTag(personID: person.id) else { return nil }
        guard prev.caseInsensitiveCompare(currentTag) != .orderedSame else { return nil }
        guard !currentTag.isEmpty else { return nil }
        return prev
    }

    private func retagAllPhotos(removing oldTag: String, adding newTag: String) {
        tagBatchInFlight = true
        tagBatchStatus = nil
        let urls = files.map(\.url)
        let storeRef = store
        Task.detached(priority: .userInitiated) {
            var updated = 0
            var failed = 0
            for url in urls {
                do {
                    _ = try TagWriter.removeTags([oldTag], at: url)
                    _ = try TagWriter.addTags([newTag], at: url)
                    updated += 1
                } catch {
                    failed += 1
                }
            }
            let doneCount = updated
            let failCount = failed
            await MainActor.run {
                tagBatchInFlight = false
                tagBatchStatus = "Replaced \"\(oldTag)\" → \"\(newTag)\" on \(doneCount) file\(doneCount == 1 ? "" : "s")"
                    + (failCount > 0 ? ", \(failCount) failed" : "")
                storeRef.notifyChanged()
                BulkRenameSheet.recordPersonTag(personID: person.id, tag: newTag)
            }
        }
    }

    private var hasTypedName: Bool {
        [title, firstName, lastName].contains { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
    }

    private var primaryTagName: String {
        let trimmed = [title, firstName, lastName]
            .compactMap { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        if !trimmed.isEmpty { return trimmed.joined(separator: " ") }
        return person.displayName
    }

    private func applyTagToAllPhotos(_ tag: String) {
        guard !tag.isEmpty else { return }
        tagBatchInFlight = true
        tagBatchStatus = nil
        let urls = files.map(\.url)
        let storeRef = store
        Task.detached(priority: .userInitiated) {
            let result = TagWriter.addTagsBulk([tag], to: urls)
            await MainActor.run {
                tagBatchInFlight = false
                if result.failed == 0 {
                    let added = result.added
                    let unchanged = result.unchanged
                    if unchanged == 0 {
                        tagBatchStatus = "Tagged \(added) file\(added == 1 ? "" : "s") with \"\(tag)\""
                    } else if added == 0 {
                        tagBatchStatus = "All \(unchanged) file\(unchanged == 1 ? "" : "s") already had \"\(tag)\""
                    } else {
                        tagBatchStatus = "Tagged \(added) · \(unchanged) already had \"\(tag)\""
                    }
                } else {
                    tagBatchStatus = "Tagged \(result.added), \(result.failed) failed"
                        + (result.firstError.map { " — \($0)" } ?? "")
                }
                storeRef.notifyChanged()
                BulkRenameSheet.recordPersonTag(personID: person.id, tag: tag)
            }
        }
    }

    @ViewBuilder
    private func namedField(_ label: String, placeholder: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(Theme.Font.body(12, .semibold))
                .foregroundStyle(Theme.text3)
            TextField(placeholder, text: text)
                .textFieldStyle(.plain).font(Theme.Font.body(13.5)).padding(.horizontal, 10).frame(height: 30).background(Color.black.opacity(0.3), ignoresSafeAreaEdges: []).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
                .frame(minWidth: 140)
        }
    }

    private var headerTitle: String {
        if isUnknown { return "Unknown person" }
        let preview = [title, firstName, middleName, lastName, suffix]
            .compactMap { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        return preview.isEmpty ? "Unnamed person" : preview
    }

    /// `[Title] [First]`, else first, else title, else "a person".
    private var deepAnalyzeRef: String {
        let t = title.trimmingCharacters(in: .whitespaces)
        let f = firstName.trimmingCharacters(in: .whitespaces)
        if !t.isEmpty && !f.isEmpty { return "\(t) \(f)" }
        if !f.isEmpty { return f }
        if !t.isEmpty { return t }
        return "a person"
    }
}

private struct PersonFileTile: View {
    let file: FileRow
    var selectMode: Bool = false
    var isChecked: Bool = false

    var body: some View {
        VStack(spacing: 4) {
            Color.black.opacity(0.3)
                .aspectRatio(1, contentMode: .fit)
                .overlay(tile)
                .overlay(selectionOverlay)
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous)
                        .stroke(isChecked ? Theme.gold : Color.clear,
                                lineWidth: isChecked ? 2 : 0)
                )
            Text(file.url.lastPathComponent)
                .font(Theme.Font.body(11))
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .center)
        }
        .contextMenu {
            Button("Open") { NSWorkspace.shared.open(file.url) }
            Button("Show in Finder") {
                NSWorkspace.shared.activateFileViewerSelecting([file.url])
            }
        }
        .help(selectMode ? "Click to select" : "Click to open. Right-click to show it in Finder.")
        .accessibilityElement(children: .combine)
        .accessibilityLabel(file.url.lastPathComponent)
        .accessibilityAddTraits(selectMode ? [.isButton, isChecked ? .isSelected : []] : [.isImage, .isButton])
        .accessibilityHint(selectMode
            ? (isChecked ? "Selected. Tap to deselect."
                         : "Tap to select for moving to another person.")
            : "Opens the photo.")
    }

    private var tile: some View {
        AsyncThumbnail(url: file.url, size: 240) { Image(systemName: "photo").foregroundStyle(Theme.text3) }
    }

    @ViewBuilder
    private var selectionOverlay: some View {
        if selectMode {
            ZStack(alignment: .topTrailing) {
                Color.clear
                Image(systemName: isChecked ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 18))
                    .foregroundStyle(isChecked ? Theme.gold : Color.white.opacity(0.85))
                    .background(Circle().fill(.black.opacity(0.4)))
                    .padding(6)
            }
        }
    }
}

// MARK: - Move-photos target picker

/// Picker shown when the user wants to reassign photos from one
/// person's cluster to another. Lists every other person, named or not
/// (skips self and people marked unknown), with their representative
/// thumbnail and photo count. Picking one asks before moving.
private struct MovePhotosTargetPicker: View {
    let sourcePerson: ReadStore.PersonRow
    let engine: EngineClient
    let store: ReadStore
    let fileIDs: [Int64]
    let onMoved: @Sendable @MainActor (_ count: Int, _ targetName: String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var candidates: [ReadStore.PersonRow] = []
    @State private var pendingTarget: ReadStore.PersonRow?

    private func move(to target: ReadStore.PersonRow) {
        let sourceID = sourcePerson.id
        let targetID = target.id
        let ids = fileIDs
        let targetName = target.displayName
        let completion = onMoved
        dismiss()
        Task { [engine, store] in
            let moved = await CatalogWrites.moveFaces(engine, store: store, from: sourceID, to: targetID, fileIDs: ids)
            completion(moved, targetName)
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Move \(fileIDs.count) photo\(fileIDs.count == 1 ? "" : "s") to…")
                        .font(Theme.Font.body(16, .semibold))
                    Text("Pick the person these photos actually show. Only the selected photos move; the rest of \(sourcePerson.displayName)'s group stays put.")
                        .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                }
                Spacer()
                Button("Cancel") { dismiss() }
                    .buttonStyle(.dsSecondary)
                    .keyboardShortcut(.cancelAction)
            }
            .padding(16)
            Divider()
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 160, maximum: 200), spacing: 12)],
                          spacing: 12) {
                    ForEach(candidates) { p in
                        Button { pendingTarget = p } label: {
                            PersonCard(person: p, store: store)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(16)
            }
        }
        .frame(minWidth: 640, minHeight: 480)
        .confirmationDialog(
            "Move \(fileIDs.count) photo\(fileIDs.count == 1 ? "" : "s") from \(sourcePerson.displayName) to \(pendingTarget?.displayName ?? "")?",
            isPresented: Binding(get: { pendingTarget != nil }, set: { if !$0 { pendingTarget = nil } }),
            titleVisibility: .visible
        ) {
            Button("Move") {
                guard let target = pendingTarget else { return }
                pendingTarget = nil
                move(to: target)
            }
            Button("Cancel", role: .cancel) { pendingTarget = nil }
        } message: {
            Text("\(sourcePerson.displayName)'s face in \(fileIDs.count == 1 ? "this photo" : "these photos") will count as \(pendingTarget?.displayName ?? "the other person") instead. The files themselves aren't changed. To put them back, move them the same way from the other card.")
        }
        .onAppear {
            let sourceID = sourcePerson.id
            Task {
                candidates = await Task.detached(priority: .userInitiated) { [store] in
                    store.persons().filter { $0.id != sourceID }
                }.value
            }
        }
    }
}

// MARK: - Suggested merges sheet

/// Borderline-similar centroid pairs. Merge picks the named (or larger)
/// cluster as target; dismissed rows drop from the list.
private struct SuggestedMergesSheet: View {
    let candidates: [MergeSuggestionPair]
    let personByID: [Int64: ReadStore.PersonRow]
    let store: ReadStore
    let inFlight: Bool
    let status: String?
    let progress: String?
    let stopRequested: Bool
    let onStop: () -> Void
    let onAccept: (MergeSuggestionPair) -> Void
    let onAcceptMany: ([MergeSuggestionPair]) -> Void
    let onDismiss: () -> Void

    @State private var pending: PendingMerge?
    private struct PendingMerge {
        let pairs: [MergeSuggestionPair]
        let single: Bool
        let title: String
    }

    private func name(_ id: Int64) -> String { personByID[id]?.displayName ?? "this person" }

    @ViewBuilder
    private var outcomeRow: some View {
        if let progress {
            HStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text(progress).font(Theme.Font.body(12)).foregroundStyle(Theme.text2)
                Button(stopRequested ? "Stopping…" : "Stop", action: onStop)
                    .buttonStyle(DSSecondaryButtonStyle(height: 22))
                    .disabled(stopRequested)
                    .help("Finish the merge that is running, then stop. Merges already done stay.")
                Spacer(minLength: 0)
            }
        } else if let status {
            Text(status).font(Theme.Font.body(12)).foregroundStyle(Theme.text2)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: "sparkle.magnifyingglass")
                    .foregroundStyle(Theme.gold)
                    .font(Theme.Font.body(16, .semibold))
                VStack(alignment: .leading, spacing: 1) {
                    Text("Suggested merges")
                        .font(Theme.Font.body(14, .semibold))
                    Text("\(candidates.count) pairs of cards might be the same person.")
                        .font(Theme.Font.body(12))
                        .foregroundStyle(Theme.text3)
                }
                Spacer()
                Button("Done", action: onDismiss)
                    .buttonStyle(.dsPrimary)
                    .keyboardShortcut(.defaultAction)
            }
            Text("These pairs were close enough to look like the same person, but not close enough to merge automatically. Review and merge the ones that match.")
                .font(Theme.Font.body(12))
                .foregroundStyle(Theme.text3)
            HStack(spacing: 8) {
                // Bulk-accept the highest-confidence ("Very likely same",
                // cosine ≥ 0.55) pairs in one click. Lets the user clear
                // the easy cases without 100 individual Merge clicks.
                let veryLikely = candidates.filter { $0.similarity >= 0.55 }
                Button {
                    pending = PendingMerge(pairs: veryLikely, single: false,
                                           title: "Merge \(veryLikely.count) \(veryLikely.count == 1 ? "pair" : "pairs") that very likely match?")
                } label: {
                    Label("Merge \(veryLikely.count) very likely matches",
                          systemImage: "checkmark.circle.fill")
                        .font(Theme.Font.body(13.5, .semibold))
                        .padding(.horizontal, 12).padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.delight.opacity(0.18)))
                        .foregroundStyle(Theme.delight)
                }
                .buttonStyle(.plain)
                .disabled(veryLikely.isEmpty || inFlight)
                .help("Combine only the pairs FileID is most sure about. The rest stay here for you to check.")
                Button {
                    pending = PendingMerge(pairs: candidates, single: false,
                                           title: "Merge all \(candidates.count) \(candidates.count == 1 ? "pair" : "pairs")?")
                } label: {
                    Label("Merge all \(candidates.count)",
                          systemImage: "rectangle.stack.fill")
                        .font(Theme.Font.body(13.5, .semibold))
                        .padding(.horizontal, 12).padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Color.secondary, lineWidth: 1))
                        .foregroundStyle(Theme.text3)
                }
                .buttonStyle(.plain)
                .disabled(candidates.isEmpty || inFlight)
                .help("Merge every pair in the list. Be careful: the pairs marked \"Possibly same\" may be two different people.")
                Spacer()
            }
            outcomeRow
            Divider().opacity(0.3)
            if candidates.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "checkmark.seal.fill")
                        .font(.system(size: 48))
                        .foregroundStyle(Theme.delight)
                    Text("No likely matches found.")
                        .font(Theme.Font.body(13.5, .semibold))
                    Text("Every card is clearly a different person. Check again after adding more photos.")
                        .font(Theme.Font.body(12))
                        .foregroundStyle(Theme.text3)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 400)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(40)
            } else {
                ScrollView {
                    // LazyVStack: a few hundred candidates in a plain
                    // VStack overflows SwiftUI's AttributeGraph.
                    LazyVStack(spacing: 10) {
                        ForEach(candidates) { cand in
                            row(for: cand)
                        }
                    }
                }
            }
        }
        .padding(20)
        .frame(minWidth: 560, minHeight: 420)
        .background(LavaLampBackground())
        .preferredColorScheme(.dark)
        .confirmationDialog(
            pending?.title ?? "",
            isPresented: Binding(get: { pending != nil }, set: { if !$0 { pending = nil } }),
            titleVisibility: .visible
        ) {
            Button("Merge", role: .destructive) {
                guard let merge = pending else { return }
                pending = nil
                if merge.single, let pair = merge.pairs.first { onAccept(pair) } else { onAcceptMany(merge.pairs) }
            }
            Button("Cancel", role: .cancel) { pending = nil }
        } message: {
            Text(pending?.single == false
                 ? "Each pair becomes one person. You can undo this right after. You can stop partway; merges already done stay."
                 : PeopleView.mergeWarning)
        }
    }

    @ViewBuilder
    private func row(for cand: MergeSuggestionPair) -> some View {
        let a = personByID[cand.sourcePersonID]
        let b = personByID[cand.destinationPersonID]
        // Skip rows where one side has been merged-away.
        if let a, let b {
            HStack(spacing: 12) {
                miniCard(a)
                Image(systemName: "arrow.left.and.right")
                    .foregroundStyle(Theme.text3)
                miniCard(b)
                Spacer()
                VStack(alignment: .trailing, spacing: 4) {
                    Text(similarityLabel(cand.similarity))
                        .font(Theme.Font.body(12, .semibold))
                        .foregroundStyle(similarityColor(cand.similarity))
                        .help(FaceReviewText.scoreHelp(cand.similarity))
                    Button("Merge") {
                        pending = PendingMerge(pairs: [cand], single: true,
                                               title: "Merge \(name(cand.sourcePersonID)) and \(name(cand.destinationPersonID))?")
                    }
                        .buttonStyle(.dsPrimary)
                        .tint(Theme.gold)
                        .disabled(inFlight)
                }
            }
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                    .fill(.ultraThinMaterial)
            )
        }
    }

    @ViewBuilder
    private func miniCard(_ p: ReadStore.PersonRow) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "person.crop.circle")
                .font(Theme.Font.body(20, .semibold))
                .foregroundStyle(Theme.gold)
            VStack(alignment: .leading, spacing: 1) {
                Text(p.displayName)
                    .font(Theme.Font.body(13.5, .semibold))
                    .lineLimit(1)
                Text("\(p.fileCount) photo\(p.fileCount == 1 ? "" : "s")")
                    .font(Theme.Font.body(11))
                    .foregroundStyle(Theme.text3)
            }
        }
        .frame(width: 170, alignment: .leading)
    }

    private func similarityColor(_ s: Double) -> Color {
        if s >= 0.55 { return Theme.delight }
        if s >= 0.50 { return Theme.gold }
        return Theme.warning
    }

    private func similarityLabel(_ s: Double) -> String {
        if s >= 0.55 { return "Very likely same" }
        if s >= 0.50 { return "Likely same" }
        return "Possibly same"
    }
}

// MARK: - Merge target picker

/// Pick which checked person becomes the merge target. Named first.
private struct MergeTargetPickerSheet: View {
    let checked: [ReadStore.PersonRow]
    let onPick: (ReadStore.PersonRow) -> Void
    let onCancel: () -> Void
    private let sortedCandidates: [ReadStore.PersonRow]
    @State private var pendingTarget: ReadStore.PersonRow?

    init(checked: [ReadStore.PersonRow], onPick: @escaping (ReadStore.PersonRow) -> Void, onCancel: @escaping () -> Void) {
        self.checked = checked
        self.onPick = onPick
        self.onCancel = onCancel
        // Named first (alphabetical), then unnamed (by file count desc).
        let named = checked.filter { $0.hasAnyName }
            .sorted { $0.displayName < $1.displayName }
        let unnamed = checked.filter { !$0.hasAnyName }
            .sorted { $0.fileCount > $1.fileCount }
        sortedCandidates = named + unnamed
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Combine \(checked.count) cards into one person")
                    .font(Theme.Font.body(14, .semibold))
                Spacer()
                Button("Cancel", action: onCancel)
                    .buttonStyle(.dsSecondary)
                    .keyboardShortcut(.cancelAction)
            }
            Text("Choose the card to keep. Photos from the other cards move into it, and those cards go away.")
                .font(Theme.Font.body(13.5))
                .foregroundStyle(Theme.text3)
                .fixedSize(horizontal: false, vertical: true)
            Divider().opacity(0.3)
            ScrollView {
                LazyVStack(spacing: 8) {
                    ForEach(sortedCandidates) { person in
                        Button { pendingTarget = person } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "person.crop.circle")
                                    .font(Theme.Font.body(20, .semibold))
                                    .foregroundStyle(Theme.gold)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(person.displayName)
                                        .font(Theme.Font.body(13.5, .semibold))
                                    Text("\(person.fileCount) photo\(person.fileCount == 1 ? "" : "s") · \(person.faceCount) face\(person.faceCount == 1 ? "" : "s")")
                                        .font(Theme.Font.body(12))
                                        .foregroundStyle(Theme.text3)
                                }
                                Spacer()
                                Image(systemName: "arrow.triangle.merge")
                                    .foregroundStyle(Theme.text3)
                            }
                            .padding(10)
                            .background(
                                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                                    .fill(.ultraThinMaterial)
                            )
                            .overlay(
                                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                                    .stroke(person.hasAnyName
                                              ? Theme.gold.opacity(0.6)
                                              : Color.white.opacity(0.10),
                                            lineWidth: 1)
                            )
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
        .padding(20)
        .frame(minWidth: 460, minHeight: 320)
        .background(LavaLampBackground())
        .preferredColorScheme(.dark)
        .confirmationDialog(
            "Combine \(checked.count) cards into \"\(pendingTarget?.displayName ?? "")\"?",
            isPresented: Binding(get: { pendingTarget != nil }, set: { if !$0 { pendingTarget = nil } }),
            titleVisibility: .visible
        ) {
            Button("Merge", role: .destructive) {
                guard let target = pendingTarget else { return }
                pendingTarget = nil
                onPick(target)
            }
            Button("Cancel", role: .cancel) { pendingTarget = nil }
        } message: {
            Text("The other \(checked.count - 1 == 1 ? "card goes" : "\(checked.count - 1) cards go") away and \(checked.count - 1 == 1 ? "its" : "their") photos move to this one. You can undo this right after.")
        }
    }
}


/// Inline install for the face model, so People can be grouped, named and
/// tagged without a trip to Settings. Downloads only when the user clicks.
private struct FaceModelInstallPanel: View {
    let engine: EngineClient
    let detectedFaces: Int
    @State private var installer = ModelInstaller.face
    private let kind = FaceEmbedderKind.defaultFor(ramGB: Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824)

    var body: some View {
        Panel {
            PanelHeader("Face recognition") {
                MetaText(detectedFaces > 0 ? "\(detectedFaces.formatted()) faces detected" : "No faces yet", size: 11)
            }
            VStack(alignment: .leading, spacing: 10) {
                Text("Download the Faces model to sort photos by person").font(Theme.Font.body(15, .semibold)).foregroundStyle(Theme.text1)
                Text("It finds the same person across your photos. Then you can name people, combine duplicates and tag every photo they appear in.")
                    .font(Theme.Font.body(13.5)).foregroundStyle(Theme.text2).fixedSize(horizontal: false, vertical: true)
                statusRow
            }
            .padding(14)
            PanelFooter {
                MetaText("Downloads once (\(downloadSize)), then works offline. It runs on this Mac; your photos never leave it.", size: 11)
                    .help("Face model: \(kind.displayName)")
                Spacer()
                actions
            }
        }
        .onAppear { installer.refreshStatus() }
    }

    @ViewBuilder
    private var statusRow: some View {
        switch installer.status {
        case .downloading(let fraction, let message, _, _):
            VStack(alignment: .leading, spacing: 6) {
                GrainProgressBar(progress: fraction, tone: .gold)
                MetaText(message, size: 11)
            }
        case .installFailed(let reason):
            MetaText(reason, color: Theme.danger, size: 11)
        case .installed:
            MetaText("Ready. Press Group faces to see people here.", color: Theme.delight, size: 11)
        default:
            EmptyView()
        }
    }

    private var downloadSize: String {
        "about \(Int((Double(kind.approxBytes) / 1_048_576).rounded())) MB"
    }

    @ViewBuilder
    private var actions: some View {
        switch installer.status {
        case .downloading:
            Button("Cancel") { installer.cancel() }.buttonStyle(DSSecondaryButtonStyle(height: 28))
        case .installed:
            Button { engine.runFaceClustering() } label: { Label("Group faces", systemImage: "person.2") }
                .buttonStyle(DSPrimaryButtonStyle(height: 28))
                .disabled(engine.faceClusteringInFlight)
        case .installFailed:
            Button("Retry") { installer.install() }.buttonStyle(DSPrimaryButtonStyle(height: 28))
        default:
            Button { installer.install() } label: { Label("Download Faces model", systemImage: "arrow.down") }
                .buttonStyle(DSPrimaryButtonStyle(height: 28))
        }
    }
}

/// Moves an NSImage across a task boundary; the image is never mutated.
struct UncheckedImage: @unchecked Sendable {
    let image: NSImage
    init(_ image: NSImage) { self.image = image }
}

private struct FaceQueueObservers: ViewModifier {
    let jobs: [CatalogJob]
    let changes: Int
    let refresh: FaceRefreshModel?
    let onChange: () -> Void

    func body(content: Content) -> some View {
        content
            .onChangeWhileActive(of: jobs) {
                if jobs.contains(where: { $0.kind == "faceRefresh" }) { Task { await refresh?.reload() } }
            }
            .onChange(of: changes) { _, _ in onChange() }
    }
}
