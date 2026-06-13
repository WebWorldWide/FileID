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
    @State private var lastVersionSeen: Int = -1

    /// Cards become checkboxes; "Merge selected" picks a target.
    @State private var mergeMode: Bool = false
    @State private var mergeChecked: Set<Int64> = []
    /// Same checkbox UX, but the bulk action sets `is_unknown = true`.
    @State private var unknownMode: Bool = false
    @State private var unknownChecked: Set<Int64> = []
    @State private var mergeStatus: String?

    @State private var suggestions: [ClusterSuggestions.Candidate] = []
    @State private var suggestionsLoading: Bool = false

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
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 6)
            }
            content
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { reload() }
        .onChange(of: store.version) { _, _ in reload() }
        .onChange(of: engine.lastFaceClustering?.personCount) { _, _ in reload() }
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
                    onAccept: { candidate in
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
                            Task.detached(priority: .userInitiated) {
                                let n = storeRef.mergePersons(target: t.id, sources: [s.id])
                                await MainActor.run {
                                    if let n {
                                        mergeStatus = "Merged into \"\(displayName)\" (\(n) photos)."
                                    }
                                    suggestions.removeAll { $0.id == candidateID }
                                    reload()
                                }
                            }
                        } else {
                            suggestions.removeAll { $0.id == candidate.id }
                            reload()
                        }
                    },
                    onAcceptMany: { batch in
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
                        let storeRef = store
                        Task.detached(priority: .userInitiated) {
                            let merged = storeRef.mergePersonsBatch(pairs)
                            await MainActor.run {
                                suggestions.removeAll { batchToRemove.contains($0) }
                                mergeStatus = "Bulk-merged \(merged) cluster\(merged == 1 ? "" : "s")."
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
                Text("People")
                    .font(.largeTitle.bold())
                Text(headerCountLine)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Spacer()
                headerActions
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

    @ViewBuilder
    private var headerActions: some View {
        if mergeMode || unknownMode {
            Button("Cancel") {
                mergeMode = false; unknownMode = false
                mergeChecked.removeAll(); unknownChecked.removeAll()
            }
            .buttonStyle(.bordered)
        } else if persons.count >= 2 {
            Button {
                suggestionsLoading = true
                Task {
                    let result = await Task.detached(priority: .userInitiated) {
                        ClusterSuggestions.findCandidates(
                            dbPath: ReadStore.defaultDBURL.path
                        )
                    }.value
                    suggestions = result
                    suggestionsLoading = false
                    activeSheet = .suggestedMerges
                }
            } label: {
                Label(suggestionsLoading ? "Scanning…" : "Suggest merges",
                      systemImage: suggestionsLoading ? "hourglass" : "sparkle.magnifyingglass")
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.gold)
            .disabled(suggestionsLoading)

            Button {
                mergeMode = true; mergeChecked.removeAll()
            } label: {
                Label("Merge people", systemImage: "arrow.triangle.merge")
            }
            .buttonStyle(.bordered)
            .help("Pick two or more people to combine into one. Use this when face clustering split the same person into multiple cards.")

            Button {
                unknownMode = true; unknownChecked.removeAll()
            } label: {
                Label("Mark unknown", systemImage: "person.crop.circle.badge.questionmark")
            }
            .buttonStyle(.bordered)
            .help("Mark people you don't want to identify (strangers, crowd extras). They're hidden from the People tab and won't be merged into named clusters on the next run.")
        }
    }

    @ViewBuilder
    private var bulkActionStrip: some View {
        HStack(spacing: 10) {
            if mergeMode {
                Text("\(mergeChecked.count) selected to merge")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    if mergeChecked.count >= 2 { activeSheet = .mergeTargetPicker }
                } label: {
                    Label("Merge \(mergeChecked.count) selected", systemImage: "arrow.triangle.merge")
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.gold)
                .disabled(mergeChecked.count < 2)
            } else if unknownMode {
                Text("\(unknownChecked.count) selected to mark unknown")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    bulkMarkUnknown()
                } label: {
                    Label("Mark \(unknownChecked.count) as unknown", systemImage: "person.crop.circle.badge.questionmark")
                }
                .buttonStyle(.borderedProminent)
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
        if p == 0 { return "\(totalFacePrints) faces · clustering…" }
        if unnamed > 0 { return "\(p) people · \(unnamed) still unnamed" }
        return "\(p) people · all named"
    }

    private func bulkMarkUnknown() {
        let ids = Array(unknownChecked)
        for id in ids {
            guard let p = personByID[id] else { continue }
            store.updatePerson(id: p.id,
                                title: p.title, firstName: p.firstName,
                                middleName: p.middleName, lastName: p.lastName,
                                suffix: p.suffix, isUnknown: true)
        }
        mergeStatus = "Marked \(ids.count) cluster\(ids.count == 1 ? "" : "s") as unknown."
        unknownMode = false
        unknownChecked.removeAll()
        reload()
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
                .foregroundStyle(.secondary)
                .font(.callout)
            Text("Don't want to name anyone?")
                .font(.caption)
                .foregroundStyle(.secondary)
            Button {
                let modelKind = DeepAnalyzeSettings.shared.activeKind.rawValue
                engine.deepAnalyzeAll(modelKind: modelKind, skipExisting: true)
                onSwitchTab(.deep)
            } label: {
                Label("Skip — run Deep Analyze with generic captions",
                      systemImage: "forward.fill")
                    .font(.caption.bold())
                    .padding(.horizontal, 10).padding(.vertical, 5)
                    .background(Capsule().stroke(Color.secondary.opacity(0.6), lineWidth: 1))
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .disabled(engine.deepAnalyzeInFlight || !engine.deepAnalyzeAvailable)
            .help("Run Deep Analyze without naming people. Captions will use generic descriptions like \"a person playing piano\" instead of real names.")
            Spacer()
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.secondary.opacity(0.08)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.secondary.opacity(0.20), lineWidth: 0.5))
    }

    private var headerSubtitle: String {
        let f = totalFacePrints
        let p = persons.count
        let unnamed = persons.filter { !$0.hasAnyName }.count
        if p == 0 && f == 0 {
            return "Run a scan first — face detection runs as part of the scan."
        }
        if p == 0 {
            return "\(f) faces detected. Click the button to group them into people."
        }
        if unnamed > 0 {
            return "\(p) people identified · \(unnamed) still unnamed. Naming them lets Deep Analyze use real names in captions."
        }
        return "\(p) people identified · all named ✓"
    }

    @ViewBuilder
    private var stepsBar: some View {
        let totalNamed = persons.filter { $0.hasAnyName }.count
        let scanned = totalFacePrints > 0
        let clustered = !persons.isEmpty
        let named = totalNamed > 0
        HStack(spacing: 6) {
            stepBadge(idx: 1, label: "Scan finds faces",
                       done: scanned)
            stepArrow(active: scanned)
            stepBadge(idx: 2, label: "Cluster groups them",
                       done: clustered)
            stepArrow(active: clustered)
            stepBadge(idx: 3, label: "You name a few",
                       done: named)
            stepArrow(active: named)
            stepBadge(idx: 4, label: "Deep Analyze uses names",
                       done: named && persons.contains(where: { $0.fileCount > 0 }))
            Spacer()
        }
        .font(.system(size: 10, weight: .semibold))
    }

    @ViewBuilder
    private func stepBadge(idx: Int, label: String, done: Bool) -> some View {
        HStack(spacing: 4) {
            Image(systemName: done ? "checkmark.circle.fill" : "\(idx).circle")
                .foregroundStyle(done ? .green : .secondary)
            Text(label)
                .foregroundStyle(done ? Color.primary : Color.secondary)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(done ? Color.green.opacity(0.10) : Color.white.opacity(0.04))
        )
    }

    @ViewBuilder
    private func stepArrow(active: Bool) -> some View {
        Image(systemName: "chevron.right")
            .font(.system(size: 8))
            .foregroundStyle(active ? Theme.gold.opacity(0.7) : Color.secondary.opacity(0.5))
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
                    .foregroundStyle(.tertiary)
                Text(showHiddenUnknowns
                     ? "\(hiddenUnknownCount) marked unknown — currently visible"
                     : "\(hiddenUnknownCount) hidden as unknown")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button(showHiddenUnknowns ? "Hide them" : "Show them") {
                    showHiddenUnknowns.toggle()
                    reload()
                }
                .buttonStyle(.borderless)
                .font(.caption)
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
        EmptyStateView(
            icon: "person.crop.circle.badge.exclamationmark",
            title: "Face recognition needs a model",
            message: "FileID uses a small AI model to find and group faces in your photos.",
            secondaryMessage: "Open Settings → AI Models. Pick the standard model (166 MB) or the lightweight model (13 MB)."
        )
    }

    // MARK: - Reload

    private func reload() {
        totalFacePrints = store.totalFacePrints()
        // Drop emptied clusters: moving every face out of a person leaves a
        // 0-face row whose stale representative_face_id now points at the
        // target's face — a ghost card showing the wrong person. Hide it.
        let rows = store.persons(includeUnknown: showHiddenUnknowns)
            .filter { $0.faceCount > 0 }
        persons = rows
        hiddenUnknownCount = store.hiddenUnknownCount()
        // `merging:` to tolerate duplicate ids (uniqueKeysWithValues traps).
        personByID = Dictionary(rows.map { ($0.id, $0) }, uniquingKeysWith: { lhs, _ in lhs })
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
    let onMerged: (Int) -> Void

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
                        await MainActor.run { completion(n) }
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
            Color.black.opacity(0.4)
                .aspectRatio(1, contentMode: .fit)
                .overlay(faceCrop)
                .overlay(selectionOverlay)
                .clipShape(RoundedRectangle(cornerRadius: 10))
            VStack(alignment: .leading, spacing: 2) {
                Text(displayName)
                    .font(.callout.bold())
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text("\(person.fileCount) photo\(person.fileCount == 1 ? "" : "s") · \(person.faceCount) face\(person.faceCount == 1 ? "" : "s")")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(.horizontal, 4)
        }
        .padding(8)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(.ultraThinMaterial)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(Theme.gold.opacity(0.18), lineWidth: 1)
        )
        .task(id: person.id) {
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
            Image(systemName: "person.crop.circle.fill")
                .font(.system(size: 48))
                .foregroundStyle(Theme.gold.opacity(0.4))
        }
    }

    @ViewBuilder
    private var selectionOverlay: some View {
        if selectionMode {
            ZStack {
                RoundedRectangle(cornerRadius: 10)
                    .stroke(isChecked ? selectionTint : Color.white.opacity(0.25),
                            lineWidth: isChecked ? 3 : 1.5)
                if isChecked {
                    RoundedRectangle(cornerRadius: 10)
                        .fill(selectionTint.opacity(0.18))
                }
                VStack {
                    HStack {
                        Image(systemName: isChecked ? "checkmark.circle.fill" : "circle")
                            .font(.system(size: 26))
                            .foregroundStyle(isChecked ? selectionTint : Color.white.opacity(0.85))
                            .background(Circle().fill(.black.opacity(0.45)))
                            .padding(8)
                        Spacer()
                    }
                    Spacer()
                }
            }
        }
    }

    /// Thumbnail for the representative file, cropped to the face bbox
    /// (falls back to the whole image when bbox is missing).
    private func loadThumb() async {
        guard let path = person.representativePath else { return }
        let url = URL(fileURLWithPath: path)
        guard let full = await ThumbnailService.shared.thumbnail(for: url, size: 360) else { return }
        guard let bboxStr = person.representativeBBox,
              let cropped = Self.cropFace(in: full, bbox: bboxStr) else {
            self.thumbnail = full
            return
        }
        self.thumbnail = cropped
    }

    /// Crop an NSImage to a Vision normalized "x,y,w,h" bbox, padded 20%.
    static func cropFace(in img: NSImage, bbox: String) -> NSImage? {
        let parts = bbox.split(separator: ",").compactMap { Double($0) }
        guard parts.count == 4 else { return nil }
        let pad: CGFloat = 0.20
        let x = max(0, parts[0] - parts[2] * pad)
        let y = max(0, parts[1] - parts[3] * pad)
        let w = min(1 - x, parts[2] * (1 + 2 * pad))
        let h = min(1 - y, parts[3] * (1 + 2 * pad))
        let imgSize = img.size
        let pixelRect = NSRect(
            x: x * imgSize.width,
            y: y * imgSize.height,
            width: w * imgSize.width,
            height: h * imgSize.height
        )
        guard pixelRect.width > 4, pixelRect.height > 4 else { return nil }
        let out = NSImage(size: NSSize(width: pixelRect.width, height: pixelRect.height))
        out.lockFocus()
        defer { out.unlockFocus() }
        img.draw(
            in: NSRect(origin: .zero, size: pixelRect.size),
            from: pixelRect,
            operation: .copy,
            fraction: 1.0
        )
        return out
    }
}

// MARK: - Person detail sheet

private struct PersonDetailSheet: View {
    let person: ReadStore.PersonRow
    let store: ReadStore
    @Environment(\.dismiss) private var dismiss

    @State private var files: [FileRow] = []
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
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    showMoveTargetPicker = true
                } label: {
                    Label("Move to another person…",
                          systemImage: "arrow.right.circle.fill")
                        .font(.caption.bold())
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.gold)
                .disabled(checked.isEmpty)
                Button("Cancel") {
                    selectMode = false
                    checked.removeAll()
                }
                .buttonStyle(.bordered)
                .keyboardShortcut(.escape, modifiers: [])
            } else if files.count >= 2 {
                Spacer()
                Button {
                    selectMode = true
                    checked.removeAll()
                    moveStatus = nil
                } label: {
                    Label("Select photos", systemImage: "checkmark.square")
                        .font(.caption)
                }
                .buttonStyle(.bordered)
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
                        .font(.title2.bold())
                    Spacer()
                    Text("\(person.fileCount) photo\(person.fileCount == 1 ? "" : "s") · \(person.faceCount) face\(person.faceCount == 1 ? "" : "s")")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                    Button("Done") { dismiss() }
                        .keyboardShortcut(.defaultAction)
                }
                Toggle(isOn: $isUnknown) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text("I don't know who this is")
                            .font(.callout)
                        Text("Excludes this person from AI clustering and from Deep Analyze captions. Their photos stay in the library.")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
                .toggleStyle(.checkbox)
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
                        .font(.caption2)
                        .foregroundStyle(.secondary)
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
                Text(status).font(.caption)
                    .foregroundStyle(.secondary)
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
                    files = store.files(forPersonID: person.id)
                }
            )
        }
        .frame(minWidth: 720, minHeight: 520)
        .onAppear {
            files = store.files(forPersonID: person.id)
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
                .buttonStyle(.borderedProminent)
                .tint(Theme.gold)
                .disabled(tagBatchInFlight || tagName.isEmpty)
                .help("Adds the Finder tag \"\(tagName)\" to every photo of this person. Visible in Finder, Spotlight, and Smart Folders.")

                // P10 — re-tag affordance. Shows only when this person
                // was previously tagged with a DIFFERENT name (e.g. user
                // renamed "Alex" → "Alex Doe" after a tag pass).
                if let oldTag = previousTagIfDifferent(currentTag: tagName) {
                    Button {
                        retagAllPhotos(removing: oldTag, adding: tagName)
                    } label: {
                        Label("Replace \"\(oldTag)\" with \"\(tagName)\"",
                              systemImage: "arrow.triangle.2.circlepath")
                    }
                    .buttonStyle(.bordered)
                    .disabled(tagBatchInFlight)
                    .help("Photos were tagged \"\(oldTag)\" earlier. Removes the old tag and adds the new one.")
                }
            }
            if let s = tagBatchStatus {
                Text(s).font(.caption2).foregroundStyle(.secondary)
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
            await MainActor.run {
                tagBatchInFlight = false
                tagBatchStatus = "Replaced \"\(oldTag)\" → \"\(newTag)\" on \(updated) file\(updated == 1 ? "" : "s")"
                    + (failed > 0 ? ", \(failed) failed" : "")
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
                .font(.caption.bold())
                .foregroundStyle(.secondary)
            TextField(placeholder, text: text)
                .textFieldStyle(.roundedBorder)
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
    @State private var img: NSImage?

    var body: some View {
        VStack(spacing: 4) {
            Color.black.opacity(0.3)
                .aspectRatio(1, contentMode: .fit)
                .overlay(tile)
                .overlay(selectionOverlay)
                .clipShape(RoundedRectangle(cornerRadius: 6))
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(isChecked ? Theme.gold : Color.clear,
                                lineWidth: isChecked ? 2 : 0)
                )
            Text(file.url.lastPathComponent)
                .font(.caption2)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .center)
        }
        .task(id: file.id) {
            img = await ThumbnailService.shared.thumbnail(for: file.url, size: 240)
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

    @ViewBuilder
    private var tile: some View {
        if let i = img {
            Image(nsImage: i).resizable().scaledToFill()
        } else {
            Image(systemName: "photo").foregroundStyle(.secondary)
        }
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
    let onMoved: (_ count: Int, _ targetName: String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var candidates: [ReadStore.PersonRow] = []

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Move \(fileIDs.count) photo\(fileIDs.count == 1 ? "" : "s") to…")
                        .font(.title3.bold())
                    Text("Pick the person these photos actually show. Only the selected photos move; the rest of \(sourcePerson.displayName)'s group stays put.")
                        .font(.caption).foregroundStyle(.secondary)
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
                            let moved = store.movePersonFaces(
                                fromPersonID: sourcePerson.id,
                                toPersonID: p.id,
                                fileIDs: fileIDs
                            )
                            dismiss()
                            onMoved(moved, p.displayName)
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
            candidates = store.persons().filter { $0.id != sourcePerson.id }
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
    let onAccept: (ClusterSuggestions.Candidate) -> Void
    let onAcceptMany: ([ClusterSuggestions.Candidate]) -> Void
    let onDismiss: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: "sparkle.magnifyingglass")
                    .foregroundStyle(Theme.gold)
                    .font(.title3)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Suggested merges")
                        .font(.headline)
                    Text("\(candidates.count) cluster pairs look like they might be the same person.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done", action: onDismiss)
                    .keyboardShortcut(.defaultAction)
            }
            Text("These pairs were close enough to look like the same person, but not close enough to merge automatically. Review and merge the ones that match.")
                .font(.caption)
                .foregroundStyle(.secondary)
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
                        .font(.callout.bold())
                        .padding(.horizontal, 12).padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: 8).fill(Color.green.opacity(0.18)))
                        .foregroundStyle(.green)
                }
                .buttonStyle(.plain)
                .disabled(veryLikely.isEmpty)
                .help("Bulk-merge every pair with cosine similarity ≥ 0.55 (the strongest 'same person' signal). The remaining pairs stay for manual review.")
                Button {
                    onAcceptMany(candidates)
                } label: {
                    Label("Merge all \(candidates.count)",
                          systemImage: "rectangle.stack.fill")
                        .font(.callout.bold())
                        .padding(.horizontal, 12).padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: 8).stroke(Color.secondary, lineWidth: 1))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .disabled(candidates.isEmpty)
                .help("Merge every pair in the list. Use carefully — pairs near the lower end of the borderline band may not actually be the same person.")
                Spacer()
            }
            // Outcome already rendered above the action row, no duplicate here.
            Divider().opacity(0.3)
            if candidates.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "checkmark.seal.fill")
                        .font(.system(size: 48))
                        .foregroundStyle(.green)
                    Text("No borderline pairs found.")
                        .font(.callout.bold())
                    Text("Every cluster centroid is either reliably the same person (already merged) or reliably different. Re-run after adding more photos.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
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
                    .foregroundStyle(.secondary)
                miniCard(b)
                Spacer()
                VStack(alignment: .trailing, spacing: 4) {
                    Text(similarityLabel(cand.similarity))
                        .font(.caption.bold())
                        .foregroundStyle(similarityColor(cand.similarity))
                    Button("Merge") { onAccept(cand) }
                        .buttonStyle(.borderedProminent)
                        .tint(Theme.gold)
                }
            }
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(.ultraThinMaterial)
            )
        }
    }

    @ViewBuilder
    private func miniCard(_ p: ReadStore.PersonRow) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "person.crop.circle")
                .font(.title2)
                .foregroundStyle(Theme.gold)
            VStack(alignment: .leading, spacing: 1) {
                Text(p.displayName)
                    .font(.callout.bold())
                    .lineLimit(1)
                Text("\(p.fileCount) photo\(p.fileCount == 1 ? "" : "s")")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(width: 170, alignment: .leading)
    }

    private func similarityColor(_ s: Float) -> Color {
        if s >= 0.55 { return .green }
        if s >= 0.50 { return .yellow }
        return .orange
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
                    .font(.headline)
                Spacer()
                Button("Cancel", action: onCancel)
                    .keyboardShortcut(.cancelAction)
            }
            Text("Pick which one becomes the primary. The others will be absorbed — their photos move into the primary, and the source clusters disappear.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Divider().opacity(0.3)
            ScrollView {
                VStack(spacing: 8) {
                    ForEach(sortedCandidates) { person in
                        Button { onPick(person) } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "person.crop.circle")
                                    .font(.title2)
                                    .foregroundStyle(Theme.gold)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(person.displayName)
                                        .font(.callout.bold())
                                    Text("\(person.fileCount) photo\(person.fileCount == 1 ? "" : "s") · \(person.faceCount) face\(person.faceCount == 1 ? "" : "s")")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                                Spacer()
                                Image(systemName: "arrow.triangle.merge")
                                    .foregroundStyle(.secondary)
                            }
                            .padding(10)
                            .background(
                                RoundedRectangle(cornerRadius: 8)
                                    .fill(.ultraThinMaterial)
                            )
                            .overlay(
                                RoundedRectangle(cornerRadius: 8)
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
