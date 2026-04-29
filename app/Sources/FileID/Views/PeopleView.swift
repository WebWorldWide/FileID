// People tab: face-cluster viewer over `persons` + `face_prints`.
// Engine owns `runFaceClustering`; this view reads + names + merges.
import SwiftUI
import AppKit
import FileIDShared

struct PeopleView: View {
    let engine: EngineClient
    let store: ReadStore
    @AppStorage(AppSettings.useAIFaceClusteringKey) private var useAIFaceClustering: Bool = AppSettings.useAIFaceClusteringDefault

    @State private var persons: [ReadStore.PersonRow] = []
    @State private var personByID: [Int64: ReadStore.PersonRow] = [:]
    @State private var totalFacePrints: Int = 0
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
        // After Qwen auto-merges, recompute suggestions so removed pairs disappear.
        .onChange(of: engine.lastVLMFaceVerification?.pairsMerged) { _, _ in
            reload()
            if activeSheet == .suggestedMerges {
                Task {
                    let fresh = await Task.detached(priority: .userInitiated) {
                        ClusterSuggestions.findCandidates(
                            dbPath: ReadStore.defaultDBURL.path
                        )
                    }.value
                    suggestions = fresh
                }
            }
        }
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
                    vlmInFlight: engine.vlmFaceVerifyInFlight,
                    lastVLMResult: engine.lastVLMFaceVerification,
                    onAccept: { candidate in
                        let a = personByID[candidate.personA]
                        let b = personByID[candidate.personB]
                        let target = preferredTarget(a, b) ?? a ?? b
                        let source = (target?.id == a?.id) ? b : a
                        if let t = target, let s = source {
                            if let n = store.mergePersons(target: t.id, sources: [s.id]) {
                                mergeStatus = "Merged into \"\(t.displayName)\" (\(n) photos)."
                            }
                        }
                        suggestions.removeAll { $0.id == candidate.id }
                        reload()
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
                    onVerifyWithAI: {
                        let kind = DeepAnalyzeSettings.shared.activeKind.rawValue
                        engine.runVLMFaceVerification(modelKind: kind)
                    },
                    onDismiss: { activeSheet = nil }
                )
            case .mergeTargetPicker:
                MergeTargetPickerSheet(
                    checked: Array(mergeChecked).compactMap { personByID[$0] }
                        .sorted { $0.displayName < $1.displayName },
                    onPick: { target in
                        let sources = mergeChecked.filter { $0 != target.id }
                        if let newCount = store.mergePersons(target: target.id,
                                                              sources: Array(sources)) {
                            mergeStatus = "Merged \(sources.count + 1) clusters into \"\(target.displayName)\" (\(newCount) photos)."
                        } else {
                            mergeStatus = "Merge failed — see logs."
                        }
                        mergeMode = false
                        mergeChecked.removeAll()
                        activeSheet = nil
                        reload()
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

            Menu {
                Button {
                    mergeMode = true; mergeChecked.removeAll()
                } label: { Label("Merge people manually", systemImage: "arrow.triangle.merge") }
                Button {
                    unknownMode = true; unknownChecked.removeAll()
                } label: { Label("Mark people as unknown", systemImage: "person.crop.circle.badge.questionmark") }
            } label: {
                Image(systemName: "ellipsis.circle")
                    .font(.title3)
                    .foregroundStyle(.secondary)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
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
            if engine.vlmFaceVerifyInFlight {
                aiClusteringProgressView
            } else if let vlm = engine.lastVLMFaceVerification, vlm.pairsExamined > 0 {
                Text(String(format: "AI verified %d pairs · merged %d (%.1fs)",
                            vlm.pairsExamined, vlm.pairsMerged, vlm.durationSeconds))
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    private func formatETA(_ seconds: Double) -> String {
        let s = max(0, Int(seconds.rounded()))
        let h = s / 3600; let m = (s % 3600) / 60; let sec = s % 60
        if h > 0 { return String(format: "%dh %dm", h, m) }
        if m > 0 { return String(format: "%dm %ds", m, sec) }
        return "\(sec)s"
    }

    @ViewBuilder
    private var aiClusteringProgressView: some View {
        if let p = engine.vlmFaceVerifyProgress, p.pairsTotal > 0 {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    Image(systemName: "wand.and.stars")
                        .foregroundStyle(Theme.gold)
                    Text("AI clustering")
                        .font(.callout.bold())
                    Spacer()
                    if let eta = p.etaSeconds, eta > 0 {
                        Text("\(formatETA(eta)) left")
                            .font(.caption.bold().monospacedDigit())
                            .foregroundStyle(Theme.gold)
                    }
                    Text("\(p.pairsExamined) / \(p.pairsTotal)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                ProgressView(value: Double(p.pairsExamined),
                             total: Double(max(p.pairsTotal, 1)))
                    .tint(Theme.gold)
                Text("Auto-merged \(p.mergedSoFar) cluster\(p.mergedSoFar == 1 ? "" : "s") so far · the VLM compares two face crops at a time")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(Theme.gold.opacity(0.10))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(Theme.gold.opacity(0.4), lineWidth: 1)
            )
        } else {
            // Indeterminate spinner while we wait for the first progress
            // event (e.g. while the VLM container is loading).
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("AI clustering starting (loading model)…")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(Theme.gold.opacity(0.08))
            )
        }
    }

    private var headerSubtitle: String {
        let f = totalFacePrints
        let p = persons.count
        let unnamed = persons.filter { !$0.hasAnyName }.count
        if p == 0 && f == 0 {
            return "Run a scan first — Apple Vision will find every face in your library."
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
        if persons.isEmpty && totalFacePrints == 0 {
            emptyState
        } else if persons.isEmpty {
            noClustersYet
        } else {
            ScrollView {
                let unnamed = persons.filter { !$0.hasAnyName }
                if !unnamed.isEmpty {
                    HStack(spacing: 8) {
                        Image(systemName: "lightbulb.fill")
                            .foregroundStyle(.yellow)
                        VStack(alignment: .leading, spacing: 1) {
                            Text("Tip: name the most-photographed people")
                                .font(.callout.bold())
                            Text("Names get used by Deep Analyze in captions and suggested filenames. Click any card to add a name.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                    }
                    .padding(12)
                    .background(
                        RoundedRectangle(cornerRadius: 8)
                            .fill(Color.yellow.opacity(0.10))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 8)
                            .stroke(Color.yellow.opacity(0.4), lineWidth: 1)
                    )
                    .padding(.horizontal, 16)
                    .padding(.top, 8)
                }
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
                    }
                }
                .padding(16)
            }
        }
    }

    // Top-aligned, no .frame(maxHeight:) and no .fixedSize on Text:
    // either propagates an infinite height intrinsic through Detail and
    // collapses the sidebar to zero width on macOS 26.4.1.
    private var emptyState: some View {
        VStack(spacing: 14) {
            Image(systemName: "person.2.crop.square.stack")
                .font(.system(size: 64, weight: .light))
                .foregroundStyle(Theme.gold.opacity(0.5))
            Text("No people yet")
                .font(.title2.bold())
            Text("This page fills in as Apple Vision detects faces during a scan. Pick a folder in the sidebar and click Start Scan — face detection runs in parallel with everything else.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 480)
        }
        .padding(.horizontal, 40)
        .padding(.top, 40)
    }

    private var noClustersYet: some View {
        VStack(spacing: 14) {
            Image(systemName: "person.crop.circle.badge.questionmark")
                .font(.system(size: 64, weight: .light))
                .foregroundStyle(Theme.gold.opacity(0.5))
            Text("\(totalFacePrints) faces detected, ready to group")
                .font(.title2.bold())
            Text("Click Group photos by face above. We compare every face to every other and group ones that look like the same person. Takes a few seconds. After it runs, you'll see a card per person — click to give them names.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 520)
            if engine.faceClusteringInFlight {
                ProgressView("Clustering…").padding(.top, 8)
            }
        }
        .padding(.horizontal, 40)
        .padding(.top, 40)
    }

    // MARK: - Reload

    private func reload() {
        totalFacePrints = store.totalFacePrints()
        let rows = store.persons()
        persons = rows
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
            }
            .padding(16)
            Divider()
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 140), spacing: 8)], spacing: 8) {
                    ForEach(files) { f in
                        PersonFileTile(file: f)
                    }
                }
                .padding(12)
            }
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
    @State private var img: NSImage?

    var body: some View {
        VStack(spacing: 4) {
            Color.black.opacity(0.3)
                .aspectRatio(1, contentMode: .fit)
                .overlay(tile)
                .clipShape(RoundedRectangle(cornerRadius: 6))
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
    }

    @ViewBuilder
    private var tile: some View {
        if let i = img {
            Image(nsImage: i).resizable().scaledToFill()
        } else {
            Image(systemName: "photo").foregroundStyle(.secondary)
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
    let vlmInFlight: Bool
    let lastVLMResult: VLMFaceVerificationResult?
    let onAccept: (ClusterSuggestions.Candidate) -> Void
    let onAcceptMany: ([ClusterSuggestions.Candidate]) -> Void
    let onVerifyWithAI: () -> Void
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
            Text("Lower distance = more similar. Below 0.50 the original clustering would have merged automatically; above 0.70 they're reliably different. Anything in between is your call.")
                .font(.caption)
                .foregroundStyle(.secondary)
            HStack(spacing: 8) {
                Button {
                    onVerifyWithAI()
                } label: {
                    Label(vlmInFlight ? "Verifying with AI…" : "Verify with AI",
                          systemImage: vlmInFlight ? "hourglass" : "wand.and.stars")
                        .font(.callout.bold())
                        .padding(.horizontal, 12).padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: 8).stroke(Theme.gold, lineWidth: 1))
                        .foregroundStyle(Theme.gold)
                }
                .buttonStyle(.plain)
                .disabled(vlmInFlight || candidates.isEmpty)
                .help("Use the local Vision-Language Model (Qwen) to compare face crops side-by-side. Slower than centroid distance but more accurate at borderline cases.")
                // Bulk-accept the highest-confidence ("Very likely same",
                // L2 < 0.55) pairs in one click. Lets the user clear the
                // easy cases without 100 individual Merge clicks.
                let veryLikely = candidates.filter { $0.distance < 0.55 }
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
                .help("Bulk-merge every pair below L2 0.55 (the strongest 'same person' signal). The remaining pairs stay for manual review.")
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
                .help("Merge every pair in the list. Use carefully — pairs above L2 0.65 may not actually be the same person.")
                Spacer()
            }
            if let r = lastVLMResult {
                Text("AI verified \(r.pairsExamined) pairs · \(r.pairsConfirmedSame) same-person")
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
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
                    Text(String(format: "L2 %.2f", cand.distance))
                        .font(.caption.monospacedDigit().bold())
                        .foregroundStyle(distanceColor(cand.distance))
                    Text(distanceLabel(cand.distance))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
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

    private func distanceColor(_ d: Float) -> Color {
        if d < 0.55 { return .green }
        if d < 0.62 { return .yellow }
        return .orange
    }

    private func distanceLabel(_ d: Float) -> String {
        if d < 0.55 { return "Very likely same" }
        if d < 0.62 { return "Likely same" }
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
