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

    /// Cards become checkboxes; "Merge selected" picks a target.
    @State private var mergeMode: Bool = false
    @State private var mergeChecked: Set<Int64> = []
    /// Same checkbox UX, but the bulk action sets `is_unknown = true`.
    @State private var unknownMode: Bool = false
    @State private var unknownChecked: Set<Int64> = []
    @State private var mergeStatus: String?

    @State private var suggestions: [ClusterSuggestions.Candidate] = []
    @State private var suggestionsLoading: Bool = false
    @State private var mergeInFlight: Bool = false

    /// Single sheet driver. Three stacked .sheet modifiers wedged the
    /// view graph on macOS 26 (blanked the sidebar on tab transition).
    enum ActiveSheet: Identifiable, Equatable {
        case personDetail(Int64)        // person id; we look up the row
        case suggestedMerges
        case mergeTargetPicker
        var id: String {
            switch self {
            case .personDetail(let pid): return "personDetail-\(pid)"
            case .suggestedMerges:       return "suggestedMerges"
            case .mergeTargetPicker:     return "mergeTargetPicker"
            }
        }
    }
    @State private var activeSheet: ActiveSheet?

    var body: some View {
        VStack(spacing: 0) {
            headerBlock
            Divider().opacity(0.15)
            if let s = mergeStatus {
                Text(s)
                    .font(Theme.Font.body(12))
                    .foregroundStyle(Theme.text3)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 6)
            }
            content
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { reload() }
        .onChange(of: store.version) { _, _ in reload() }
        .onChange(of: engine.lastFaceClustering?.durationSeconds) { _, _ in reload() }
        .sheet(item: $activeSheet) { sheet in
            switch sheet {
            case .personDetail(let pid):
                if let person = personByID[pid] {
                    PersonDetailSheet(person: person, store: store)
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
                    onAccept: { candidate in
                        guard !mergeInFlight else { return }
                        let a = personByID[candidate.personA]
                        let b = personByID[candidate.personB]
                        let target = preferredTarget(a, b) ?? a ?? b
                        let source = (target?.id == a?.id) ? b : a
                        if let t = target, let s = source {
                            // Run on a detached task — the mergePersons
                            // SQL touches face_prints + persons in a
                            // transaction; on libraries with thousands
                            // of clusters this can take 5–15 s and would
                            // freeze the UI on the main thread.
                            let storeRef = store
                            let candidateID = candidate.id
                            let displayName = t.displayName
                            mergeInFlight = true
                            Task.detached(priority: .userInitiated) {
                                let n = storeRef.mergePersons(target: t.id, sources: [s.id])
                                await MainActor.run {
                                    if let n {
                                        mergeStatus = "Merged into \"\(displayName)\" (\(n) photos)."
                                        suggestions.removeAll { $0.id == candidateID }
                                    } else {
                                        mergeStatus = "Merge failed — see logs."
                                    }
                                    mergeInFlight = false
                                    reload()
                                }
                            }
                        } else {
                            suggestions.removeAll { $0.id == candidate.id }
                            reload()
                        }
                    },
                    onAcceptMany: { batch in
                        guard !mergeInFlight else { return }
                        // One transaction for the whole batch. ReadStore's
                        // union-find resolves chains (A→B, B→C → A→C),
                        // so we just submit raw (target, source) pairs.
                        var pairs: [(target: Int64, source: Int64)] = []
                        pairs.reserveCapacity(batch.count)
                        for candidate in batch {
                            let a = personByID[candidate.personA]
                            let b = personByID[candidate.personB]
                            guard let target = preferredTarget(a, b) ?? a ?? b else { continue }
                            let source = (target.id == a?.id) ? b : a
                            guard let s = source, s.id != target.id else { continue }
                            pairs.append((target.id, s.id))
                        }
                        let batchToRemove = batch
                        guard !pairs.isEmpty else {
                            suggestions.removeAll { batchToRemove.contains($0) }
                            reload()
                            return
                        }
                        let storeRef = store
                        mergeInFlight = true
                        Task.detached(priority: .userInitiated) {
                            let merged = storeRef.mergePersonsBatch(pairs)
                            await MainActor.run {
                                if merged > 0 {
                                    suggestions.removeAll { batchToRemove.contains($0) }
                                    mergeStatus = "Bulk-merged \(merged) cluster\(merged == 1 ? "" : "s")."
                                } else {
                                    mergeStatus = "Merge failed — see logs."
                                }
                                mergeInFlight = false
                                reload()
                            }
                        }
                    },
                    onDismiss: { activeSheet = nil }
                )
            case .mergeTargetPicker:
                MergeTargetPickerSheet(
                    checked: Array(mergeChecked).compactMap { personByID[$0] }
                        .sorted { $0.displayName < $1.displayName },
                    onPick: { target in
                        let sources = mergeChecked.filter { $0 != target.id }
                        let storeRef = store
                        let displayName = target.displayName
                        let sourceCount = sources.count
                        // Off the main thread — large merges hit
                        // face_prints + persons in a transaction and
                        // can take seconds.
                        mergeMode = false
                        mergeChecked.removeAll()
                        activeSheet = nil
                        Task.detached(priority: .userInitiated) {
                            let newCount = storeRef.mergePersons(target: target.id,
                                                                  sources: Array(sources))
                            await MainActor.run {
                                if let newCount {
                                    mergeStatus = "Merged \(sourceCount + 1) clusters into \"\(displayName)\" (\(newCount) photos)."
                                } else {
                                    mergeStatus = "Merge failed — see logs."
                                }
                                reload()
                            }
                        }
                    },
                    onCancel: { activeSheet = nil }
                )
            }
        }
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                MetaText(headerCountLine, color: Theme.text2)
                Spacer()
                headerActions
            }
            if let count = engine.lastFaceClustering?.deferredFaces, count > 0 {
                Text("\(count) face\(count == 1 ? "" : "s") await processing. Unknown people are excluded.")
                    .font(Theme.Font.body(13.5))
                    .foregroundStyle(Theme.text3)
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
                    let result = await Task.detached(priority: .userInitiated) {
                        ClusterSuggestions.findCandidates(
                            dbPath: ReadStore.defaultDBURL.path
                        )
                    }.value
                    suggestionsLoading = false
                    // The scan is multi-second; don't preempt whatever the
                    // user navigated to while it ran. Only surface the sheet
                    // if they're still on a neutral People view — no sheet
                    // already up, not mid merge/unknown selection.
                    guard activeSheet == nil, !mergeMode, !unknownMode else { return }
                    suggestions = result
                    activeSheet = .suggestedMerges
                }
            } label: {
                Label(suggestionsLoading ? "Scanning…" : "Suggest merges",
                      systemImage: suggestionsLoading ? "hourglass" : "sparkle.magnifyingglass")
            }
            .buttonStyle(.dsPrimary)
            .tint(Theme.gold)
            .disabled(suggestionsLoading)

            Button {
                mergeMode = true; mergeChecked.removeAll()
            } label: {
                Label("Merge people", systemImage: "arrow.triangle.merge")
            }
            .buttonStyle(.dsSecondary)
            .help("Pick two or more people to combine into one. Use this when face clustering split the same person into multiple cards.")

            Button {
                unknownMode = true; unknownChecked.removeAll()
            } label: {
                Label("Mark unknown", systemImage: "person.crop.circle.badge.questionmark")
            }
            .buttonStyle(.dsSecondary)
            .help("Mark people you don't want to identify (strangers, crowd extras). They're hidden from the People tab and won't be merged into named clusters on the next run.")
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
        // One batched transaction off the main thread (the per-id
        // updatePerson loop opened N connections + ran N main-thread transactions,
        // freezing the UI for crowd-sized selections), mirroring the merge paths.
        let ids = Array(unknownChecked)
        guard !ids.isEmpty else { return }
        let storeRef = store
        unknownMode = false
        unknownChecked.removeAll()
        Task.detached(priority: .userInitiated) {
            let n = storeRef.markUnknownBatch(ids: ids)
            await MainActor.run {
                mergeStatus = "Marked \(n) cluster\(n == 1 ? "" : "s") as unknown."
                reload()
            }
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
        .padding(.horizontal, 20)
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
            Text("Don't want to name anyone?")
                .font(Theme.Font.body(12))
                .foregroundStyle(Theme.text3)
            Button {
                let modelKind = DeepAnalyzeSettings.shared.activeKind.rawValue
                engine.deepAnalyzeAll(modelKind: modelKind, skipExisting: true)
                onSwitchTab(.deep)
            } label: {
                Label("Skip — run Deep Analyze with generic captions",
                      systemImage: "forward.fill")
                    .font(Theme.Font.body(12, .semibold))
                    .padding(.horizontal, 10).padding(.vertical, 5)
                    .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Color.secondary.opacity(0.6), lineWidth: 1))
                    .foregroundStyle(Theme.text3)
            }
            .buttonStyle(.plain)
            .disabled(engine.deepAnalyzeInFlight || !engine.deepAnalyzeAvailable)
            .help("Run Deep Analyze without naming people. Captions will use generic descriptions like \"a person playing piano\" instead of real names.")
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
            Text("Names set — keep going.")
                .font(Theme.Font.body(12))
                .foregroundStyle(Theme.text3)
            Button {
                let modelKind = DeepAnalyzeSettings.shared.activeKind.rawValue
                engine.deepAnalyzeAll(modelKind: modelKind, skipExisting: true)
                onSwitchTab(.deep)
            } label: {
                Label("Continue to Deep Analyze", systemImage: "arrow.forward.circle.fill")
                    .font(Theme.Font.body(12, .semibold))
                    .padding(.horizontal, 10).padding(.vertical, 5)
                    .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.gold))
                    .foregroundStyle(.black)
            }
            .buttonStyle(.plain)
            .disabled(engine.deepAnalyzeInFlight || !engine.deepAnalyzeAvailable)
            .help("Generate captions and smart filenames using the names you've set.")
            Spacer()
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.gold.opacity(0.10)))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.gold.opacity(0.30), lineWidth: 0.5))
    }

    // MARK: - Content

    @ViewBuilder
    private var content: some View {
        if FaceEmbedderKind.installedKinds().isEmpty {
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
                                store: store,
                                onMerged: { count in
                                    mergeStatus = "Merged into \(person.displayName) (\(count) photos)."
                                    reload()
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
                .padding(16)
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
                .help("People marked as unknown stay hidden so the grid only shows folks you might want to identify. They also don't get re-grouped when face clustering re-runs.")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
    }

    private var emptyState: some View {
        EmptyStateView(
            icon: "person.2.crop.square.stack",
            title: "No people yet",
            message: "Pick a folder in the sidebar and click Start Scan. As the scan runs, faces in your photos get detected and grouped — they'll appear here as cards you can name."
        )
    }

    @ViewBuilder
    private var noClustersYet: some View {
        VStack(spacing: 14) {
            EmptyStateView(
                icon: "person.crop.circle.badge.questionmark",
                title: "\(totalFacePrints) faces detected — ready to group",
                message: "Click Group photos by face. The app compares every face to every other and creates one card per person. Takes a few seconds.",
                secondaryMessage: "Once grouped, click any card to add a name. Names get used by Deep Analyze in captions and smart filenames."
            )
            if engine.faceClusteringInFlight {
                ProgressView("Grouping…")
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

    private func reload() {
        let includeUnknown = showHiddenUnknowns
        reloadTask?.cancel()
        reloadTask = Task {
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
    let store: ReadStore
    let onMerged: @Sendable @MainActor (Int) -> Void

    func body(content: Content) -> some View {
        if enabled {
            content
                .draggable("\(personID)")
                .dropDestination(for: String.self) { items, _ in
                    guard let s = items.first,
                          let sourceID = Int64(s),
                          sourceID != personID else { return false }
                    // Merge reassigns face_prints + deletes the source row;
                    // on large clusters it blocks for seconds. Run it off the
                    // main thread, then report the result on the main actor.
                    let storeRef = store
                    let targetID = personID
                    let completion = onMerged
                    Task.detached(priority: .userInitiated) {
                        let n = storeRef.mergePersons(target: targetID,
                                                      sources: [sourceID]) ?? 0
                        await completion(n)
                    }
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
        thumbnail = nil
        guard let path = person.representativePath else { return }
        let url = URL(fileURLWithPath: path)
        guard let full = await ThumbnailService.shared.thumbnail(for: url, size: 360,
                  revision: person.representativeCacheRevision), !Task.isCancelled else { return }
        guard let bbox = person.representativeBBox else {
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
        thumbnail = cropped?.image ?? full
    }

}

// MARK: - Person detail sheet

private struct PersonDetailSheet: View {
    let person: ReadStore.PersonRow
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
                .help("Move individual photos to another person's group when face clustering put them in the wrong place.")
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
                    Text("\(person.fileCount) photo\(person.fileCount == 1 ? "" : "s") · \(person.faceCount) face\(person.faceCount == 1 ? "" : "s")")
                        .font(Theme.Font.mono(11))
                        .foregroundStyle(Theme.text3)
                    Button("Done") { dismiss() }
                        .keyboardShortcut(.defaultAction)
                }
                Toggle(isOn: $isUnknown) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text("I don't know who this is")
                            .font(Theme.Font.body(13.5))
                        Text("Excludes this person from AI clustering and from Deep Analyze captions. Their photos stay in the library.")
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
                    Text("Deep Analyze captions will reference this person as **“\(deepAnalyzeRef)”**.")
                        .font(Theme.Font.body(11))
                        .foregroundStyle(Theme.text3)
                }
                if !isUnknown && person.fileCount > 0 {
                    tagAllPhotosButton
                }
            }
            .padding(16)
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
                                }
                            }
                    }
                }
                .padding(12)
            }
        }
        .sheet(isPresented: $showMoveTargetPicker) {
            MovePhotosTargetPicker(
                sourcePerson: person,
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
        .onDisappear {
            store.updatePerson(id: person.id,
                                title: title, firstName: firstName,
                                middleName: middleName, lastName: lastName,
                                suffix: suffix, isUnknown: isUnknown)
        }
    }

    @ViewBuilder
    private var tagAllPhotosButton: some View {
        let tagName = primaryTagName
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Button {
                    applyTagToAllPhotos(tagName)
                } label: {
                    Label(tagBatchInFlight
                          ? "Tagging…"
                          : "Tag all \(person.fileCount) photo\(person.fileCount == 1 ? "" : "s") with \"\(tagName)\"",
                          systemImage: "tag.fill")
                }
                .buttonStyle(.dsPrimary)
                .tint(Theme.gold)
                .disabled(tagBatchInFlight || tagName.isEmpty)
                .help("Adds the Finder tag \"\(tagName)\" to every photo of this person. Visible in Finder, Spotlight, and Smart Folders.")

                // re-tag affordance. Shows only when this person
                // was previously tagged with a DIFFERENT name (e.g. user
                // renamed "Alex" → "Alex Doe" after a tag pass).
                if let oldTag = previousTagIfDifferent(currentTag: tagName) {
                    Button {
                        retagAllPhotos(removing: oldTag, adding: tagName)
                    } label: {
                        Label("Replace \"\(oldTag)\" with \"\(tagName)\"",
                              systemImage: "arrow.triangle.2.circlepath")
                    }
                    .buttonStyle(.dsSecondary)
                    .disabled(tagBatchInFlight)
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
            Button("Show in Finder") {
                NSWorkspace.shared.activateFileViewerSelecting([file.url])
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(file.url.lastPathComponent)
        .accessibilityAddTraits(selectMode ? [.isButton, isChecked ? .isSelected : []] : [.isImage])
        .accessibilityHint(selectMode
            ? (isChecked ? "Selected. Tap to deselect."
                         : "Tap to select for moving to another person.")
            : "")
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
/// person's cluster to another. Lists every other named person
/// (skips unknown + self), with their representative thumbnail and
/// photo count. Tap to commit the move.
private struct MovePhotosTargetPicker: View {
    let sourcePerson: ReadStore.PersonRow
    let store: ReadStore
    let fileIDs: [Int64]
    let onMoved: @Sendable @MainActor (_ count: Int, _ targetName: String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var candidates: [ReadStore.PersonRow] = []

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
                    .keyboardShortcut(.cancelAction)
            }
            .padding(16)
            Divider()
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 160, maximum: 200), spacing: 12)],
                          spacing: 12) {
                    ForEach(candidates) { p in
                        Button {
                            // Off the main thread — movePersonFaces opens a
                            // write connection and runs an UPDATE + two
                            // COUNT(DISTINCT) recounts in a transaction over the
                            // user's full multi-selection; on big clusters this
                            // blocks for seconds. Mirror the merge paths.
                            let storeRef = store
                            let sourceID = sourcePerson.id
                            let targetID = p.id
                            let ids = fileIDs
                            let targetName = p.displayName
                            let completion = onMoved
                            dismiss()
                            Task.detached(priority: .userInitiated) {
                                let moved = storeRef.movePersonFaces(
                                    fromPersonID: sourceID,
                                    toPersonID: targetID,
                                    fileIDs: ids
                                )
                                await completion(moved, targetName)
                            }
                        } label: {
                            PersonCard(person: p, store: store)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(16)
            }
        }
        .frame(minWidth: 640, minHeight: 480)
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
    let candidates: [ClusterSuggestions.Candidate]
    let personByID: [Int64: ReadStore.PersonRow]
    let store: ReadStore
    let inFlight: Bool
    let onAccept: (ClusterSuggestions.Candidate) -> Void
    let onAcceptMany: ([ClusterSuggestions.Candidate]) -> Void
    let onDismiss: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: "sparkle.magnifyingglass")
                    .foregroundStyle(Theme.gold)
                    .font(Theme.Font.body(16, .semibold))
                VStack(alignment: .leading, spacing: 1) {
                    Text("Suggested merges")
                        .font(Theme.Font.body(14, .semibold))
                    Text("\(candidates.count) cluster pairs look like they might be the same person.")
                        .font(Theme.Font.body(12))
                        .foregroundStyle(Theme.text3)
                }
                Spacer()
                Button("Done", action: onDismiss)
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
                    onAcceptMany(veryLikely)
                } label: {
                    Label("Merge \(veryLikely.count) very-likely-same",
                          systemImage: "checkmark.circle.fill")
                        .font(Theme.Font.body(13.5, .semibold))
                        .padding(.horizontal, 12).padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.delight.opacity(0.18)))
                        .foregroundStyle(Theme.delight)
                }
                .buttonStyle(.plain)
                .disabled(veryLikely.isEmpty || inFlight)
                .help("Bulk-merge every pair with cosine similarity ≥ 0.55 (the strongest 'same person' signal). The remaining pairs stay for manual review.")
                Button {
                    onAcceptMany(candidates)
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
                .help("Merge every pair in the list. Use carefully — pairs near the lower end of the borderline band may not actually be the same person.")
                Spacer()
            }
            // Outcome already rendered above the action row, no duplicate here.
            Divider().opacity(0.3)
            if candidates.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "checkmark.seal.fill")
                        .font(.system(size: 48))
                        .foregroundStyle(Theme.delight)
                    Text("No borderline pairs found.")
                        .font(Theme.Font.body(13.5, .semibold))
                    Text("Every cluster centroid is either reliably the same person (already merged) or reliably different. Re-run after adding more photos.")
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
    }

    @ViewBuilder
    private func row(for cand: ClusterSuggestions.Candidate) -> some View {
        let a = personByID[cand.personA]
        let b = personByID[cand.personB]
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
                    Button("Merge") { onAccept(cand) }
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

    private func similarityColor(_ s: Float) -> Color {
        if s >= 0.55 { return Theme.delight }
        if s >= 0.50 { return Theme.gold }
        return Theme.warning
    }

    private func similarityLabel(_ s: Float) -> String {
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

    private var sortedCandidates: [ReadStore.PersonRow] {
        // Named first (alphabetical), then unnamed (by file count desc).
        let named = checked.filter { $0.hasAnyName }
            .sorted { $0.displayName < $1.displayName }
        let unnamed = checked.filter { !$0.hasAnyName }
            .sorted { $0.fileCount > $1.fileCount }
        return named + unnamed
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Merge \(checked.count) clusters into one person")
                    .font(Theme.Font.body(14, .semibold))
                Spacer()
                Button("Cancel", action: onCancel)
                    .keyboardShortcut(.cancelAction)
            }
            Text("Pick which one becomes the primary. The others will be absorbed — their photos move into the primary, and the source clusters disappear.")
                .font(Theme.Font.body(13.5))
                .foregroundStyle(Theme.text3)
                .fixedSize(horizontal: false, vertical: true)
            Divider().opacity(0.3)
            ScrollView {
                VStack(spacing: 8) {
                    ForEach(sortedCandidates) { person in
                        Button { onPick(person) } label: {
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
                Text("Install the face model to group these faces into people").font(Theme.Font.body(15, .semibold)).foregroundStyle(Theme.text1)
                Text("\(kind.displayName) finds and groups faces on this Mac. Once people are grouped you can name them, merge them, mark unknowns and tag every photo they appear in.")
                    .font(Theme.Font.body(13.5)).foregroundStyle(Theme.text2).fixedSize(horizontal: false, vertical: true)
                statusRow
            }
            .padding(14)
            PanelFooter {
                MetaText("Downloads from Hugging Face only when you click. Runs on this Mac.", size: 11)
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
            MetaText("Installed. Group faces to see people here.", color: Theme.delight, size: 11)
        default:
            MetaText("\(kind.displayName) · about \(ByteCountFormatter.string(fromByteCount: Int64(kind.approxBytes), countStyle: .file))", size: 11)
        }
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
            Button { installer.install() } label: { Label("Install face model", systemImage: "arrow.down") }
                .buttonStyle(DSPrimaryButtonStyle(height: 28))
        }
    }
}

/// Moves an NSImage across a task boundary; the image is never mutated.
struct UncheckedImage: @unchecked Sendable {
    let image: NSImage
    init(_ image: NSImage) { self.image = image }
}
