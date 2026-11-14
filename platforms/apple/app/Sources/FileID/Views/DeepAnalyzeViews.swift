// Deep Analyze UI: model picker card, dedicated tab, per-file button,
// status bar. Active model selection lives in `DeepAnalyzeSettings`
// (Services/), which the engine reads from UserDefaults at spawn.
import SwiftUI
import AppKit
import FileIDShared

// MARK: - Settings card

struct DeepAnalyzeModelPickerCard: View {
    let engine: EngineClient
    @State private var settings = DeepAnalyzeSettings.shared
    @State private var catalogPresented = false
    @State private var requested: String?
    @State private var failure: (text: String, detail: String)?
    @State private var removeTarget: AIModelKind?
    @State private var onDiskBytes: Int64 = 0
    @State private var diskRefresh = 0

    private var ramGB: Double {
        Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
    }
    private var top3: [AIModelKind] {
        AIModelKind.recommendedFor(ramGB: ramGB)
    }

    var body: some View {
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("Descriptions and Chat").font(Theme.Font.body(14, .semibold))
                    Spacer()
                    Text("Suggested for this Mac (\(Int(ramGB)) GB memory)")
                        .font(Theme.Font.mono(11)).foregroundStyle(Theme.text3)
                }
                Text("The AI that writes descriptions and suggested names, and answers Chat. Bigger models write better but need more memory.")
                    .font(Theme.Font.body(13.5)).foregroundStyle(Theme.text3)
                Divider().opacity(0.3)
                if !AIModelKind.favorites.contains(settings.activeKind) {
                    Text("Selected model").font(Theme.Font.body(12, .semibold)).foregroundStyle(Theme.ai)
                    modelOptionRow(settings.activeKind)
                    Divider().opacity(0.3)
                }
                ForEach(top3, id: \.rawValue) { kind in
                    modelOptionRow(kind)
                }
                Divider().opacity(0.3)
                selectedModelActions
                Divider().opacity(0.3)
                Button { catalogPresented = true } label: {
                    Label("Browse \(MLXModelCatalog.entries.count) more models", systemImage: "magnifyingglass")
                }.buttonStyle(.dsSecondary)
                DisclosureGroup("More favorites") {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(AIModelKind.favorites.filter { !top3.contains($0) }, id: \.rawValue) { kind in
                            modelOptionRow(kind)
                        }
                    }
                    .padding(.top, 6)
                }
                .font(Theme.Font.body(12))
            }
        }
        .sheet(isPresented: $catalogPresented) {
            ModelCatalogBrowser(engine: engine, visionOnly: true, initialSelection: settings.activeKind) { settings.activeKind = $0 }
        }
        .task(id: "\(settings.activeKind.rawValue)|\(ModelInstallStatus.isInstalled(kind: settings.activeKind))|\(diskRefresh)") {
            let kind = settings.activeKind
            onDiskBytes = await Task.detached(priority: .utility) { DescriptionModelStore.sizeOnDisk(kind) }.value
        }
        .onChange(of: settings.activeKind.rawValue) { _, _ in failure = nil }
        .onChange(of: engine.lastErrorSignal) { _, _ in
            guard requested != nil, let error = engine.lastError else { return }
            if error.kind == "prewarm_cancelled" { requested = nil; return }
            guard ModelFailureText.isModelFailure(error) else { return }
            failure = (ModelFailureText.plain(error), error.message)
            requested = nil
        }
        .confirmationDialog("Remove this description model?",
                            isPresented: Binding(get: { removeTarget != nil }, set: { if !$0 { removeTarget = nil } }),
                            titleVisibility: .visible, presenting: removeTarget) { kind in
            Button("Remove", role: .destructive) { remove(kind) }
            Button("Keep", role: .cancel) {}
        } message: { _ in
            Text("Frees about \(ByteCountFormatter.string(fromByteCount: onDiskBytes, countStyle: .file)). Describe Files and Chat download it again the next time they need it.")
        }
    }

    @ViewBuilder
    private var selectedModelActions: some View {
        let kind = settings.activeKind
        let installed = ModelInstallStatus.isInstalled(kind: kind)
        let progress = engine.modelDownloadProgress.flatMap { $0.modelKind == kind.rawValue ? $0 : nil }
        VStack(alignment: .leading, spacing: 8) {
            if let progress {
                HStack(spacing: 10) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(installed || progress.fraction >= 0.99
                             ? "Getting the selected model ready…"
                             : "Downloading the selected model… \(Int((progress.fraction * 100).rounded()))%")
                            .font(Theme.Font.body(12, .semibold)).monospacedDigit()
                        ProgressView(value: min(max(progress.fraction, 0), 1)).tint(Theme.gold)
                    }
                    .help(progress.message)
                    Button("Cancel") { cancelDownload(kind) }.buttonStyle(.dsSecondary)
                }
            } else if installed {
                HStack(spacing: 8) {
                    MetaText("The selected model is downloaded · \(ByteCountFormatter.string(fromByteCount: onDiskBytes, countStyle: .file)) on this Mac", size: 11.5)
                        .help(DescriptionModelStore.folder(for: kind)?.path ?? "")
                    Spacer()
                    Button("Remove…") { removeTarget = kind }
                        .buttonStyle(DSGhostButtonStyle(color: Theme.danger))
                        .disabled(engine.deepAnalyzeInFlight || engine.modelDownloadProgress != nil)
                        .help(engine.deepAnalyzeInFlight ? "Wait until FileID finishes describing files" : "Delete this model's files to free space. Your own files are not touched.")
                }
            } else if requested == kind.rawValue {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    MetaText("Starting the download…", size: 11.5)
                    Spacer()
                    Button("Cancel") { cancelDownload(kind) }.buttonStyle(.dsSecondary)
                }
            } else {
                HStack(spacing: 8) {
                    MetaText("The selected model isn't downloaded yet. It downloads once from the internet (huggingface.co).", size: 11.5)
                    Spacer()
                    Button { startDownload(kind) } label: {
                        Label("Download · \(DescriptionModelStore.downloadSizeText(kind))", systemImage: "arrow.down.circle.fill")
                    }
                    .buttonStyle(.dsPrimary)
                    .disabled(!kind.fits(ramGB: ramGB) || engine.modelDownloadProgress != nil)
                }
            }
            if let failure {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    MetaText(failure.text, color: Theme.danger, size: 12).help(failure.detail)
                    Spacer()
                    if !installed {
                        Button("Try again") { startDownload(kind) }.buttonStyle(.dsSecondary)
                    }
                }
            }
        }
    }

    private func startDownload(_ kind: AIModelKind) {
        failure = nil
        if let free = DescriptionModelStore.freeDiskBytes(), !kind.fits(freeDiskBytes: free) {
            failure = (ModelInstaller.notEnoughSpace(needed: kind.requiredFreeBytes, available: free), "")
            return
        }
        engine.clearModelDownloadProgress(forModelKind: kind.rawValue)
        if engine.prewarmModel(kind.rawValue) {
            requested = kind.rawValue
        } else if ModelLicenseAcceptance.isAccepted(for: kind) {
            failure = ("FileID's helper isn't running. Quit and reopen FileID, then try again.", "")
        }
    }

    private func cancelDownload(_ kind: AIModelKind) {
        requested = nil
        engine.cancelPrewarm()
        engine.clearModelDownloadProgress(forModelKind: kind.rawValue)
    }

    private func remove(_ kind: AIModelKind) {
        do {
            try DescriptionModelStore.remove(kind)
            failure = nil
        } catch {
            failure = ("FileID couldn't remove this model.", error.localizedDescription)
        }
        diskRefresh += 1
    }

    @ViewBuilder
    private func modelOptionRow(_ kind: AIModelKind) -> some View {
        ModelOptionRow(
            kind: kind,
            isActive: settings.activeKind == kind,
            installed: ModelInstallStatus.isInstalled(kind: kind),
            fits: kind.fits(ramGB: ramGB),
            ramGB: ramGB,
            onPick: { settings.activeKind = kind }
        )
    }

}

// Extracted from DeepAnalyzeModelPickerCard to keep the type-checker happy.
private struct ModelOptionRow: View {
    let kind: AIModelKind
    let isActive: Bool
    let installed: Bool
    let fits: Bool
    let ramGB: Double
    let onPick: () -> Void

    var body: some View {
        Button {
            guard fits else { return }
            onPick()
        } label: {
            HStack(alignment: .top, spacing: 10) {
                indicatorIcon
                VStack(alignment: .leading, spacing: 2) {
                    titleRow
                    Text(kind.subtitle)
                        .font(Theme.Font.body(12))
                        .foregroundStyle(Theme.text3)
                    statsLine
                    if !fits {
                        Text("Too big for this Mac's memory. Choose a smaller model.")
                            .font(Theme.Font.body(11))
                            .foregroundStyle(Theme.warning)
                    }
                }
                Spacer()
            }
            .padding(8)
            .background(rowBackground)
            .overlay(rowBorder)
            .opacity(fits ? 1.0 : 0.55)
        }
        .buttonStyle(.plain)
        .disabled(!fits)
        .help(fits ? "License: \(kind.licenseName)" : "This model needs \(String(format: "%.1f", kind.ramBudgetGB)) GB of free memory. Choose a smaller model for this \(Int(ramGB)) GB Mac. FileID checks free memory again before it starts.")
    }

    @ViewBuilder
    private var indicatorIcon: some View {
        let name: String = {
            if isActive { return "largecircle.fill.circle" }
            return fits ? "circle" : "exclamationmark.triangle.fill"
        }()
        let color: Color = {
            if isActive { return Theme.gold }
            return fits ? .secondary : Theme.warning
        }()
        Image(systemName: name)
            .font(Theme.Font.body(16, .semibold))
            .foregroundStyle(color)
    }

    @ViewBuilder
    private var titleRow: some View {
        HStack(spacing: 6) {
            Text(kind.displayName)
                .font(Theme.Font.body(13.5, .semibold))
            badgeView
        }
    }

    @ViewBuilder
    private var badgeView: some View {
        if !fits {
            BadgePill(label: Self.memoryBadge(need: kind.ramBudgetGB, have: ramGB), color: Theme.warning)
        } else if installed {
            BadgePill(label: "Downloaded", color: Theme.delight)
        } else {
            let gb = String(format: "%.1f GB", Double(kind.approxBytes) / 1_073_741_824)
            BadgePill(label: "\(gb) download", color: .secondary)
        }
    }

    /// Whole numbers, unless they would read the same ("needs 8, has 8").
    static func memoryBadge(need: Double, have: Double) -> String {
        let whole = Int(need.rounded()) != Int(have.rounded())
        let format = whole ? "%.0f" : "%.1f"
        return "Needs \(String(format: format, need)) GB of free memory (this Mac has \(String(format: format, have)) GB in total)"
    }

    @ViewBuilder
    private var statsLine: some View {
        Text("Needs about \(String(format: "%.1f", kind.ramBudgetGB)) GB memory · " + (kind.secondsPerImage > 0 ? "about \(String(format: "%.0f", kind.secondsPerImage)) s a photo" : "speed not measured yet"))
            .font(Theme.Font.mono(10.5))
            .foregroundStyle(Theme.text3)
    }

    @ViewBuilder
    private var rowBackground: some View {
        RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
            .fill(isActive ? Theme.gold.opacity(0.10) : Color.clear)
    }

    @ViewBuilder
    private var rowBorder: some View {
        let color: Color = {
            if isActive { return Theme.gold.opacity(0.6) }
            return fits ? Color.white.opacity(0.08) : Theme.warning.opacity(0.4)
        }()
        RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
            .stroke(color, lineWidth: 1)
    }
}

// Sentinel-based install check. The engine writes
// `.fileid-installed` only after ensureLoaded succeeded — i.e. every
// weight shard is on disk AND MLX successfully built a ModelContainer.
// Checking config.json (which Hub creates very early) flips green
// while gigabytes of safetensors are still streaming in.
enum ModelInstallStatus {
    static func isInstalled(kind: AIModelKind) -> Bool {
        let url = ModelCachePaths.huggingFaceModels
            .appendingPathComponent(kind.sourceRepo, isDirectory: true)
            .appendingPathComponent(".fileid-installed")
        return FileManager.default.fileExists(atPath: url.path)
    }
}

// MARK: - Deep Analyze full-page view

struct DeepAnalyzeView: View {
    let engine: EngineClient
    let store: ReadStore
    var onSwitchTab: (MainWindow.Tab) -> Void = { _ in }
    @State private var settings = DeepAnalyzeSettings.shared
    @State private var skipExisting = true
    @State private var bulkRenameSheetOpen = false
    @State private var pendingRenameCount: Int = 0
    @State private var namePreviewOpen = false
    @State private var namePreviewIDs: [Int64] = []

    // Item 4: lay-person explainer distinguishing automatic in-scan Tagging
    // from the opt-in Deep Analyze pass. Dismissible; stays hidden once read.
    @AppStorage("hideDeepAnalyzeExplainer") private var hideExplainer = false

    // Item 5: "Apply to your files" — write tags / people / smart names onto the
    // actual files. Single in-flight gate + a short result line.
    @State private var applyInFlight = false
    @State private var applyStatus: String? = nil
    @State private var applyConfirm: ApplyConfirm?
    @State private var confirmDownload = false
    @State private var runFailure: (text: String, detail: String)?

    // Cached. body re-evaluates at the 4 Hz deepAnalyzeProgress rate during
    // a run, and these fed synchronous SQLite COUNT(*) on the main thread on every
    // pass (multi-hour overnight runs). Refreshed on the same coarse triggers as
    // pendingRenameCount, plus active-model changes (pending is keyed by model).
    @State private var pendingTotals: (total: Int, pending: Int) = (0, 0)

    /// True when at least one person cluster has been given a name.
    /// Hard requirement before Deep Analyze can run — the VLM uses the
    /// names in captions, and "a person doing X" captions are nearly
    /// useless. User must visit People and name at least one cluster.
    @State private var hasNamedAnyone = false
    @State private var hasPeople = false

    /// Throttle state for the `store.version` refresh coalescer (see
    /// `throttledRefresh`): leading-edge timestamp + trailing debounce.
    @State private var lastReloadAt: Date = .distantPast
    @State private var refreshDebounce: Task<Void, Never>?

    // Engine is alive (not crashed / mid-respawn); the live run cards key
    // off this so a crash mid-run tears them down at once.
    private var engineLive: Bool {
        if case .ready = engine.state { return true }
        return false
    }

    // Run requested but no per-file progress yet — the VLM cold-load /
    // model-download window where the "Starting…" card shows.
    private var isStartingDeepAnalyze: Bool {
        engine.deepAnalyzeInFlight && engine.deepAnalyzeProgress == nil
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                header
                if !hideExplainer {
                    explainerCard
                }
                if !engine.deepAnalyzeAvailable {
                    unavailableCard
                }
                statusCard
                actionsCard
                if let runFailure, !engine.deepAnalyzeInFlight {
                    failureCard(runFailure)
                }
                // Smart names produced by Deep Analyze stack up here for
                // bulk apply. Lives in this tab (not Library) because
                // smart names ARE Deep Analyze's output — putting the
                // bulk-rename trigger anywhere else split the workflow.
                if pendingRenameCount > 0 || !ChatHandoff.shared.selectedFileIDs.isEmpty {
                    smartNamesCard
                }
                if pendingTotals.total > 0 || pendingRenameCount > 0 || hasNamedAnyone {
                    applyActionsCard
                }
                // Show a "Starting…" card the instant Deep Analyze is
                // requested, even before the first progress event lands.
                // Without this, hitting Skip from the People tab feels
                // like a 10s freeze (the wait for the VLM model to load).
                if isStartingDeepAnalyze, engineLive {
                    startingCard
                        .animation(.spring(response: 0.35, dampingFraction: 0.78),
                                   value: engine.deepAnalyzeInFlight)
                }
                if engine.deepAnalyzeProgress != nil, engineLive {
                    progressCard
                }
                if let lastDone = engine.deepAnalyzeComplete {
                    completionCard(lastDone)
                }
                // Suppress the prior run's caption while a fresh run spins
                // up — otherwise a stale caption sits beside "Starting…"
                // (deepAnalyzeLast is private(set) on EngineClient
                // so it can't be cleared from here).
                if let lastFile = engine.deepAnalyzeLast, !isStartingDeepAnalyze {
                    lastFileCard(lastFile)
                }
                Spacer(minLength: 40)
            }
            .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, Theme.Metrics.pageInsetY)
        }
        .background(Color.clear, ignoresSafeAreaEdges: [])
        .onAppear { refreshPendingRenameCount(); refreshStatusCounts() }
        .onChangeWhileActive(of: engine.deepAnalyzeComplete?.processed ?? -1) {
            refreshPendingRenameCount()
            refreshStatusCounts()
        }
        .onChangeWhileActive(of: store.version) { throttledRefresh() }
        .onChangeWhileActive(of: settings.activeKind.rawValue) {
            refreshStatusCounts()   // pending is keyed by active model
        }
        .onChange(of: engine.lastErrorSignal) { _, _ in
            guard let error = engine.lastError, ModelFailureText.isModelFailure(error) else { return }
            runFailure = (ModelFailureText.plain(error), error.message)
        }
        .onChange(of: engine.deepAnalyzeInFlight) { _, running in
            if running { runFailure = nil }
        }
        .sheet(isPresented: $bulkRenameSheetOpen, onDismiss: refreshPendingRenameCount) {
            BulkRenameSheet(engine: engine, store: store)
        }
        .sheet(isPresented: $namePreviewOpen, onDismiss: refreshPendingRenameCount) {
            SmartNamePreviewSheet(engine: engine, store: store, fileIDs: namePreviewIDs)
        }
    }

    private func refreshPendingRenameCount() {
        Task {
            pendingRenameCount = await Task.detached(priority: .userInitiated) { [store] in
                store.countFilesWithProposedNames()
            }.value
        }
    }

    /// Coalesce the ~1/sec `store.version` bumps a live scan emits into at
    /// most one refresh per second so the 3 status COUNT(*) queries
    /// (countFilesWithProposedNames / deepAnalyzePending / namedPersonCount)
    /// don't fire 1000×. Leading
    /// edge refreshes at once when the last was ≥1s ago; otherwise a trailing
    /// debounce guarantees a final refresh after the burst settles. Mirrors
    /// LibraryView's batch throttle + search debounce.
    private func throttledRefresh() {
        let now = Date()
        if now.timeIntervalSince(lastReloadAt) >= 1.0 {
            lastReloadAt = now
            refreshDebounce?.cancel()
            refreshPendingRenameCount()
            refreshStatusCounts()
        } else {
            refreshDebounce?.cancel()
            refreshDebounce = Task { @MainActor in
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                guard !Task.isCancelled else { return }
                lastReloadAt = Date()
                refreshPendingRenameCount()
                refreshStatusCounts()
            }
        }
    }

    // Refresh the cached status counts off the 4 Hz body-eval path.
    private func refreshStatusCounts() {
        let modelKey = settings.activeKind.rawValue
        Task {
            let counts = await Task.detached(priority: .userInitiated) { [store] in
                (store.deepAnalyzePending(modelKey: modelKey), store.namedPersonCount() > 0, !store.persons().isEmpty)
            }.value
            pendingTotals = counts.0
            hasNamedAnyone = counts.1
            hasPeople = counts.2
        }
    }

    // Item 4: plain-language explanation of the two AI passes so non-technical
    // users don't conflate automatic in-scan tagging with the heavier, opt-in
    // Deep Analyze step. Dismissible.
    private var explainerCard: some View {
        Panel(padding: 16) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "sparkles")
                    .font(Theme.Font.body(16, .semibold))
                    .foregroundStyle(Theme.gold)
                VStack(alignment: .leading, spacing: 6) {
                    Text("Tags and descriptions")
                        .font(Theme.Font.body(14, .semibold))
                    Text("Tags like sunset, beach or dog are added automatically during the scan, so search already works. Describing is an optional extra step: the AI writes a full sentence about each file and suggests a clearer name, using the names you gave people. It is slower, so it runs only when you ask.")
                        .font(Theme.Font.body(12))
                        .foregroundStyle(Theme.text3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
                DSIconButton("Hide this explanation", systemImage: "xmark", tint: Theme.text3) {
                    withAnimation(Theme.springStandard) { hideExplainer = true }
                }
            }
        }
    }

    // Item 5: write Deep Analyze + tagging output onto the actual files —
    // separate actions for keyword tags and named people, plus Apply all (which
    // also renames to the smart names). Smart-name review lives in
    // smartNamesCard above. Tag writes are additive (never clobber the user's
    // own Finder tags) and reversible by removing the tag.
    private var applyActionsCard: some View {
        Panel {
            PanelHeader("Save to your files") {
                if applyInFlight { IndeterminateBar().frame(width: 60) }
            }
            VStack(alignment: .leading, spacing: 12) {
                Text("Add what FileID found to the files themselves as Finder tags, so Finder and Spotlight can find them too. Your own tags are kept.")
                    .font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 10) {
                    secondaryApplyButton("Add tags to files…", systemImage: "tag",
                                         disabled: applyInFlight) { prepareApply(.tags) }
                    secondaryApplyButton("Add people's names to files…", systemImage: "person.crop.square",
                                         disabled: applyInFlight || !hasNamedAnyone) { prepareApply(.people) }
                        .help(hasNamedAnyone ? "Tag each photo with the names of the people in it. You confirm first." : "Name someone on the People page first")
                }
                Button {
                    prepareApply(.all)
                } label: {
                    Label(pendingRenameCount > 0 ? "Add tags and rename files…" : "Add tags and names…", systemImage: "checkmark.circle")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(DSSecondaryButtonStyle(height: 30))
                .help(pendingRenameCount > 0 ? "Adds tags and people, and renames files to their suggested names in your Name style. You see the counts and confirm first." : "Adds tags and people's names to the files. You confirm first.")
                .disabled(applyInFlight)
                .confirmationDialog(
                    applyConfirm?.title ?? "",
                    isPresented: Binding(get: { applyConfirm != nil }, set: { if !$0 { applyConfirm = nil } }),
                    titleVisibility: .visible,
                    presenting: applyConfirm
                ) { confirm in
                    Button(confirm.button) { runApply(confirm) }
                    Button("Cancel", role: .cancel) { }
                } message: { confirm in
                    Text(confirm.message)
                }
                if let applyStatus {
                    Text(applyStatus).font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                }
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
        }
    }

    private func secondaryApplyButton(_ title: String, systemImage: String,
                                      disabled: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: systemImage).frame(maxWidth: .infinity)
        }
        .buttonStyle(DSSecondaryButtonStyle(height: 30))
        .disabled(disabled)
    }

    enum ApplyScope { case tags, people, all }

    /// What one "Save to your files" press will change, counted before asking.
    struct ApplyConfirm {
        let scope: ApplyScope
        var renames: [CatalogWrites.NamedRename] = []
        var tagged = 0
        var people = 0

        private static func files(_ n: Int) -> String { "\(n.formatted()) file\(n == 1 ? "" : "s")" }

        var title: String {
            if !renames.isEmpty { return "Rename \(Self.files(renames.count)) and add tags?" }
            switch scope {
            case .tags: return "Add tags to \(Self.files(tagged))?"
            case .people: return "Add people's names to \(Self.files(people))?"
            case .all: return "Add tags and names to \(Self.files(max(tagged, people)))?"
            }
        }
        var button: String {
            if !renames.isEmpty { return "Rename \(Self.files(renames.count)) and add tags" }
            switch scope {
            case .tags: return "Add tags to \(Self.files(tagged))"
            case .people: return "Add names to \(Self.files(people))"
            case .all: return "Add tags and names"
            }
        }
        var message: String {
            var parts: [String] = []
            if !renames.isEmpty {
                parts.append("\(Self.files(renames.count)) get the suggested name in your Name style. They stay in their folders. To put the old names back, use Undo last rename at the top of Library.")
            }
            if tagged > 0 { parts.append("\(Self.files(tagged)) get FileID's tags as Finder tags.") }
            if people > 0 { parts.append("\(Self.files(people)) get the names of the people in them as Finder tags.") }
            parts.append("Your own Finder tags are kept. To take a tag off later, remove it in Finder.")
            return parts.joined(separator: " ")
        }
    }

    /// Counts what the press would change, then asks. Renames are worked out
    /// here, in the saved Name style, so the confirmed count is the real one.
    private func prepareApply(_ scope: ApplyScope) {
        guard !applyInFlight else { return }
        applyInFlight = true
        applyStatus = "Counting files…"
        let store = self.store
        Task { @MainActor in
            var confirm = ApplyConfirm(scope: scope)
            if scope != .people {
                confirm.tagged = await Task.detached(priority: .userInitiated) {
                    store.filesWithKeywordTags().filter { !$0.tags.isEmpty }.count
                }.value
            }
            if scope != .tags {
                confirm.people = await Task.detached(priority: .userInitiated) {
                    store.filesWithPersonTags().filter { !$0.names.isEmpty }.count
                }.value
            }
            if scope == .all {
                let ids = await Task.detached(priority: .userInitiated) {
                    store.filesWithProposedNames(limit: NameStyle.maxFiles).map(\.id)
                }.value
                switch await StyledNames.rows(engine: engine, fileIDs: ids, naming: StyledNames.savedNaming()) {
                case .rows(let rows):
                    confirm.renames = NameStyle.renames(rows)
                case .failed(let why):
                    applyInFlight = false
                    applyStatus = why
                    return
                }
            }
            applyInFlight = false
            guard confirm.renames.count + confirm.tagged + confirm.people > 0 else {
                applyStatus = "Nothing to add yet."
                return
            }
            applyStatus = nil
            applyConfirm = confirm
        }
    }

    /// Runs a confirmed apply off the main thread (DB read + FS writes) and
    /// reports a short summary. `.all` renames first, then writes keyword
    /// tags + people tags.
    private func runApply(_ confirm: ApplyConfirm) {
        guard !applyInFlight else { return }
        applyInFlight = true
        applyStatus = "Working…"
        let store = self.store
        let scope = confirm.scope
        Task { @MainActor in
            var summary: [String] = []
            if !confirm.renames.isEmpty {
                let result = await CatalogWrites.applyNames(confirm.renames, engine: engine, store: store)
                BulkRenameSheet.saveLastBatch(result.renamed)
                if !result.renamed.isEmpty { summary.append("\(result.renamed.count) renamed") }
                if result.failed > 0 { summary.append("\(result.failed) couldn't be renamed") }
            }
            if scope == .tags || scope == .all {
                let n = await Task.detached(priority: .userInitiated) {
                    Self.writeTags(store.filesWithKeywordTags())
                }.value
                if n > 0 { summary.append("\(n) tagged") }
            }
            if scope == .people || scope == .all {
                let n = await Task.detached(priority: .userInitiated) {
                    Self.writeTags(store.filesWithPersonTags().map { (url: $0.url, tags: $0.names) })
                }.value
                if n > 0 { summary.append("\(n) tagged with names") }
            }
            applyInFlight = false
            applyStatus = summary.isEmpty
                ? "Nothing to add yet."
                : "Done: " + summary.joined(separator: ", ") + "."
            refreshPendingRenameCount()
            refreshStatusCounts()
        }
    }

    /// Additively write each file's tags as Finder tags. Returns the number of
    /// files that ended up modified. Runs on a detached task (FS writes).
    nonisolated private static func writeTags(_ items: [(url: URL, tags: [String])]) -> Int {
        var changed = 0
        for item in items where !item.tags.isEmpty {
            if (try? TagWriter.addTags(item.tags, at: item.url)) != nil { changed += 1 }
        }
        return changed
    }

    private var smartNamesCard: some View {
        Panel {
            PanelHeader("Suggested names") {
                MetaText("\(pendingRenameCount) file\(pendingRenameCount == 1 ? "" : "s")", size: 11.5)
            }
            VStack(alignment: .leading, spacing: 10) {
                Text("FileID suggested clearer names for these files. Check them and choose which to use; nothing is renamed until you do.")
                    .font(Theme.Font.body(12))
                    .foregroundStyle(Theme.text3)
                HStack(spacing: 10) {
                    Button {
                        bulkRenameSheetOpen = true
                    } label: {
                        Label("Review suggested names…", systemImage: "wand.and.rays")
                    }
                    .buttonStyle(DSPrimaryButtonStyle(height: 30))
                    .disabled(pendingRenameCount == 0)
                    Button {
                        openNamePreview()
                    } label: {
                        Label("Preview names", systemImage: "textformat")
                    }
                    .buttonStyle(DSSecondaryButtonStyle(height: 30))
                    .help(ChatHandoff.shared.selectedFileIDs.isEmpty
                          ? "Name the files that have suggestions in the style below"
                          : "Name the files chosen in Library in the style below")
                }
                Divider().opacity(0.3)
                NameStyleControls(engine: engine)
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
        }
    }

    private func openNamePreview() {
        let selection = ChatHandoff.shared.selectedFileIDs
        Task {
            let fallback = selection.isEmpty
                ? await Task.detached(priority: .userInitiated) { [store] in store.filesWithProposedNames(limit: NameStyle.maxFiles).map(\.id) }.value
                : []
            namePreviewIDs = NameStyle.fileIDs(selection: selection, fallback: fallback)
            namePreviewOpen = true
        }
    }

    private var unavailableCard: some View {
        Panel(padding: 16) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(Theme.Font.body(20, .semibold))
                    .foregroundStyle(Theme.warning)
                VStack(alignment: .leading, spacing: 6) {
                    Text("Describe Files isn't available in this copy of FileID")
                        .font(Theme.Font.body(14, .semibold))
                    Text("Reinstall FileID to fix it.")
                        .font(Theme.Font.body(13.5))
                        .foregroundStyle(Theme.text3)
                        .fixedSize(horizontal: false, vertical: true)
                        .help(engine.deepAnalyzeUnavailableReason ?? "mlx.metallib was not compiled into this build.")
                }
                Spacer(minLength: 0)
            }
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 14) {
            MetaText("FileID's local AI looks at each file, writes a sentence about it and suggests a clearer name. Nothing is renamed until you approve it.", color: Theme.text2)
            Spacer(minLength: 0)
        }
    }

    private var statusCard: some View {
        let totals = pendingTotals
        let perFile = settings.activeKind.secondsPerImage
        let modelInstalled = ModelInstallStatus.isInstalled(kind: settings.activeKind)
        return Panel {
            PanelHeader("This library")
            VStack(alignment: .leading, spacing: 8) {
                if totals.total == 0 {
                    MetaText("No files to describe yet. Scan a folder first; its photos, videos and documents appear here.")
                }
                Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
                    GridRow {
                        Text("Can be described").foregroundStyle(Theme.text3)
                        Text("\(totals.total.formatted()) files").font(Theme.Font.body(13, .medium)).monospacedDigit()
                    }
                    GridRow {
                        Text("Not described yet").foregroundStyle(Theme.text3)
                        Text("\(totals.pending.formatted()) files").font(Theme.Font.body(13, .medium)).monospacedDigit()
                    }
                    if totals.pending > 0 {
                        GridRow {
                            Text("Time needed").foregroundStyle(Theme.text3)
                            Text(perFile > 0 ? "about \(DurationText.short(Double(totals.pending) * perFile))" : "Time not measured yet")
                                .font(Theme.Font.body(13, .medium)).monospacedDigit()
                        }
                    }
                    GridRow {
                        Text("AI model").foregroundStyle(Theme.text3)
                        HStack(spacing: 8) {
                            Text(settings.activeKind.displayName).font(Theme.Font.body(13, .medium))
                            MetaText(modelInstalled ? "Downloaded" : "Not downloaded yet · about \(DescriptionModelStore.downloadSizeText(settings.activeKind))",
                                     color: modelInstalled ? Theme.delight : Theme.text2, size: 11.5)
                            Button("Change…") { onSwitchTab(.settings) }
                                .buttonStyle(DSSecondaryButtonStyle(height: 22))
                                .help("Choose a different description model in Settings")
                        }
                    }
                }
                .font(Theme.Font.body(13))
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
        }
    }

    private var runBlocker: String? {
        if !engine.deepAnalyzeAvailable { return "Describe Files isn't available in this copy of FileID. Reinstall FileID to fix it." }
        if !settings.activeKind.fits(ramGB: settings.systemRAMGB) { return "\(settings.activeKind.displayName) needs more memory than this Mac has. Choose a smaller model in Settings." }
        if engine.deepAnalyzeInFlight { return "FileID is already describing files." }
        if pendingTotals.total == 0 { return "Scan a folder first." }
        if skipExisting && pendingTotals.pending == 0 { return "Every file is already described. Turn off \"Only files not described yet\" to describe them again." }
        return nil
    }

    private var runButton: some View {
        let count = skipExisting ? pendingTotals.pending : pendingTotals.total
        return Button {
            requestRun()
        } label: {
            Label(count > 0 ? "Describe \(count.formatted()) file\(count == 1 ? "" : "s")" : "Describe files", systemImage: "wand.and.stars")
        }
        .disabled(runBlocker != nil)
        .help(runBlocker ?? "Write a description and a suggested name for each file. You can stop at any time.")
        .confirmationDialog("This downloads about \(DescriptionModelStore.downloadSizeText(settings.activeKind)) once. Continue?",
                            isPresented: $confirmDownload, titleVisibility: .visible) {
            Button("Download and describe") { startRun() }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("FileID needs its description model before it can describe files. It downloads once from the internet (huggingface.co) and then works without a connection.")
        }
    }

    private func requestRun() {
        let kind = settings.activeKind
        if ModelInstallStatus.isInstalled(kind: kind) { startRun(); return }
        if let free = DescriptionModelStore.freeDiskBytes(), !kind.fits(freeDiskBytes: free) {
            runFailure = (ModelInstaller.notEnoughSpace(needed: kind.requiredFreeBytes, available: free), "")
            return
        }
        confirmDownload = true
    }

    private func startRun() {
        runFailure = nil
        guard ModelLicenseGate.ensureAccepted(for: settings.activeKind) else { return }
        engine.deepAnalyzeAll(modelKind: settings.activeKind.rawValue, skipExisting: skipExisting)
    }

    private func failureCard(_ failure: (text: String, detail: String)) -> some View {
        Panel(padding: 16) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(Theme.Font.body(16, .semibold))
                    .foregroundStyle(Theme.warning)
                VStack(alignment: .leading, spacing: 8) {
                    Text("Describing didn't start").font(Theme.Font.body(14, .semibold))
                    Text(failure.text)
                        .font(Theme.Font.body(13)).foregroundStyle(Theme.text2)
                        .fixedSize(horizontal: false, vertical: true)
                        .help(failure.detail)
                    HStack(spacing: 8) {
                        Button("Try again") { requestRun() }
                            .buttonStyle(DSSecondaryButtonStyle(height: 28))
                            .disabled(runBlocker != nil)
                        Button("Dismiss") { runFailure = nil }.buttonStyle(.dsGhost)
                    }
                }
                Spacer(minLength: 0)
            }
        }
    }

    private var actionsCard: some View {
        let blocker = runBlocker
        return Panel {
            PanelHeader("Describe files")
            VStack(alignment: .leading, spacing: 12) {
                if hasPeople, !hasNamedAnyone, !engine.deepAnalyzeInFlight {
                    namingRequiredBanner
                }
                Toggle("Only files not described yet", isOn: $skipExisting)
                    .toggleStyle(DSCheckboxToggleStyle())
                    .font(Theme.Font.body(13.5))
                    .help("Leave this on to skip files that already have a description from \(settings.activeKind.displayName)")
                HStack(spacing: 10) {
                    if pendingRenameCount > 0 {
                        runButton.buttonStyle(DSSecondaryButtonStyle(height: 30))
                    } else {
                        runButton.buttonStyle(DSPrimaryButtonStyle(height: 30))
                    }
                    if engine.deepAnalyzeInFlight {
                        Button("Stop") { engine.deepAnalyzeCancel() }
                            .buttonStyle(DSSecondaryButtonStyle(height: 30))
                            .help("Stop describing. Files already described keep their descriptions.")
                    }
                    Spacer()
                }
                if let blocker, !engine.deepAnalyzeInFlight {
                    MetaText(blocker, color: settings.activeKind.fits(ramGB: settings.systemRAMGB) ? Theme.text3 : Theme.warning)
                } else {
                    MetaText("You can keep using your Mac while this runs; it stays awake until the work is done.")
                }
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
            PanelFooter {
                MetaText("To describe one file, open it in Library and press Describe.")
                Spacer(minLength: 0)
            }
        }
    }

    /// Soft-block banner shown when no person has been named yet.
    /// Two paths out: the recommended one (name people, get good
    /// captions) and the escape hatch (skip and run with generic
    /// captions). The escape hatch lives RIGHT NEXT to the recommended
    /// path so the user knows it's an option they're explicitly
    /// choosing, not a hidden default.
    @ViewBuilder
    private var namingRequiredBanner: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "person.crop.circle.badge.exclamationmark")
                .font(Theme.Font.body(15, .semibold))
                .foregroundStyle(Theme.warning)
            VStack(alignment: .leading, spacing: 6) {
                Text("Tip: name people first").font(Theme.Font.body(13.5, .semibold))
                Text("With names, descriptions read \"Mia playing piano\" instead of \"a person playing piano\". You can also describe now and again later.")
                    .font(Theme.Font.body(12.5))
                    .foregroundStyle(Theme.text2)
                    .fixedSize(horizontal: false, vertical: true)
                Button { onSwitchTab(.people) } label: { Label("Name people", systemImage: "arrow.right") }
                    .buttonStyle(DSAccentOutlineButtonStyle(color: Theme.warning, height: 28))
                    .help("Open the People page and type names on the faces you know")
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(Theme.warning.opacity(0.06), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .overlay(alignment: .leading) { Rectangle().fill(Theme.warning).frame(width: 3) }
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
    }

    /// Shown the moment Deep Analyze is kicked off but before the
    /// first per-file progress event arrives. The Qwen / Gemma VLM
    /// container takes ~10s to load on first call; without this card
    /// the user sees an empty page after hitting Skip and assumes
    /// nothing happened. The subtitle is driven by the engine's
    /// `deepAnalyzeStarting` event so it advances "Queued" → "Loading
    /// <model>…" → "Finding files to analyze…" as the runner moves
    /// through each phase.
    @ViewBuilder
    private var startingCard: some View {
        let download = engine.modelDownloadProgress.flatMap { progress in
            progress.fraction < 0.99 && !ModelInstallStatus.isInstalled(kind: settings.activeKind) ? progress : nil
        }
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 12) {
                    ProgressView()
                        .controlSize(.regular)
                        .tint(Theme.gold)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(download.map { "Downloading the AI model… \(Int(($0.fraction * 100).rounded()))%" } ?? "Getting the AI ready…")
                            .font(Theme.Font.body(14, .semibold)).monospacedDigit()
                        Text(download == nil ? startingSubtitle : "About \(DescriptionModelStore.downloadSizeText(settings.activeKind)), downloaded once. Describing starts by itself when it finishes.")
                            .font(Theme.Font.body(12))
                            .foregroundStyle(Theme.text3)
                            .contentTransition(.opacity)
                            .animation(.easeInOut(duration: 0.18),
                                       value: engine.deepAnalyzeStarting?.message)
                    }
                    Spacer()
                    Button(download == nil ? "Stop" : "Cancel") {
                        engine.deepAnalyzeCancel()
                        engine.cancelPrewarm()
                    }
                    .buttonStyle(.dsSecondary)
                    .help(download == nil ? "Stop before describing starts" : "Stop the download. Nothing is described.")
                }
                if let download {
                    ProgressView(value: min(max(download.fraction, 0), 1)).tint(Theme.gold)
                        .help(download.message)
                } else {
                    ShimmerView(cornerRadius: 3)
                        .frame(height: 4)
                }
            }
        }
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    /// Human-readable subtitle for `startingCard`. Falls back to the
    /// generic "Loading…" copy when the engine hasn't emitted a phase
    /// label yet (e.g. older engine binary).
    private var startingSubtitle: String {
        engine.deepAnalyzeStarting?.message
            ?? "Loading the model on this Mac. The first description usually appears within 15 seconds."
    }

    @ViewBuilder
    private var progressCard: some View {
        if let p = engine.deepAnalyzeProgress {
            Panel(padding: 16) {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("Describing files…").font(Theme.Font.body(14, .semibold))
                        Spacer()
                        if let eta = p.etaSeconds {
                            Text("about \(DurationText.short(eta)) left")
                                .font(Theme.Font.body(12.5, .medium)).monospacedDigit().foregroundStyle(Theme.ai)
                        }
                    }
                    ProgressView(value: Double(p.processed),
                                  total: Double(max(p.total, 1)))
                        .tint(Theme.gold)
                    Text("\(p.processed.formatted()) of \(p.total.formatted()) files")
                        .font(Theme.Font.body(12.5)).monospacedDigit()
                    if let path = p.currentPath {
                        Text(URL(fileURLWithPath: path).lastPathComponent)
                            .font(Theme.Font.mono(11))
                            .foregroundStyle(Theme.text3)
                            .lineLimit(1).truncationMode(.middle)
                    }
                    // Live caption stream — engine emits the
                    // partial caption text at 4 Hz as the VLM generates.
                    // Empty/nil while pre-inference; fills word-by-word
                    // once tokens start flowing.
                    if let cap = p.currentCaption, !cap.isEmpty {
                        Text(cap)
                            .font(Theme.Font.body(13.5))
                            .foregroundStyle(.primary)
                            .lineLimit(3)
                            .multilineTextAlignment(.leading)
                            .padding(.top, 4)
                            .animation(.easeInOut(duration: 0.15), value: cap)
                    }
                }
            }
        }
    }

    private func completionCard(_ c: DeepAnalyzeComplete) -> some View {
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Image(systemName: c.cancelled ? "xmark.octagon" : "checkmark.seal.fill")
                        .foregroundStyle(c.cancelled ? Theme.danger : Theme.delight)
                    Text(c.cancelled ? "Stopped" : "Finished").font(Theme.Font.body(14, .semibold))
                    Spacer()
                }
                Text("\(c.processed.formatted()) file\(c.processed == 1 ? "" : "s") described" + (c.failed > 0 ? " · \(c.failed) couldn't be read" : "") + " · took \(DurationText.short(c.totalSeconds))")
                    .font(Theme.Font.body(12.5)).foregroundStyle(Theme.text3)
            }
        }
    }

    private func lastFileCard(_ d: DeepAnalyzeFileDone) -> some View {
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Latest description").font(Theme.Font.body(14, .semibold))
                Text(d.description).font(Theme.Font.body(13.5))
                if d.inheritedFromFileID != nil {
                    MetaText("Same as a similar photo", size: 11)
                }
                if let n = d.proposedName {
                    HStack {
                        Image(systemName: "wand.and.rays").foregroundStyle(Theme.gold)
                        Text("Suggested name: ")
                            .font(Theme.Font.body(12, .semibold))
                            .foregroundStyle(Theme.text3)
                        Text(n).font(Theme.Font.mono(11)).foregroundStyle(Theme.gold)
                    }
                }
            }
        }
    }
}

// MARK: - Per-file button (used in MediaPreviewSheet)

struct DeepAnalyzeButton: View {
    let engine: EngineClient
    let file: FileRow
    @State private var settings = DeepAnalyzeSettings.shared
    @State private var confirmDownload = false
    @State private var blocked: String?

    private func describe() {
        guard ModelLicenseGate.ensureAccepted(for: settings.activeKind) else { return }
        engine.deepAnalyzeFile(fileID: file.id, modelKind: settings.activeKind.rawValue)
    }

    var body: some View {
        let alreadyDone = file.isFullyAnalyzed(by: settings.activeKind.rawValue)
        let fits = settings.activeKind.fits(ramGB: settings.systemRAMGB)
        Button {
            guard fits else { return }
            let kind = settings.activeKind
            if ModelInstallStatus.isInstalled(kind: kind) { describe(); return }
            if let free = DescriptionModelStore.freeDiskBytes(), !kind.fits(freeDiskBytes: free) {
                blocked = ModelInstaller.notEnoughSpace(needed: kind.requiredFreeBytes, available: free)
                return
            }
            confirmDownload = true
        } label: {
            Label(
                alreadyDone ? "Describe again" : "Describe",
                systemImage: engine.deepAnalyzeInFlight ? "hourglass" : "wand.and.stars"
            )
            .font(.system(size: 12, weight: .semibold))
            .padding(.horizontal, 10).padding(.vertical, 5)
            .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(
                fits ? Theme.gold.opacity(alreadyDone ? 0.18 : 0.85) : Color.gray.opacity(0.4)
            ))
            .foregroundStyle(alreadyDone ? Theme.gold : .black)
        }
        .buttonStyle(.plain)
        .disabled(engine.deepAnalyzeInFlight || !fits)
        .confirmationDialog("This downloads about \(DescriptionModelStore.downloadSizeText(settings.activeKind)) once. Continue?",
                            isPresented: $confirmDownload, titleVisibility: .visible) {
            Button("Download and describe") { describe() }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("FileID needs its description model before it can describe this file. It downloads once from the internet (huggingface.co). You can watch the download on the Describe Files page.")
        }
        .alert("The description model can't be downloaded yet", isPresented: Binding(get: { blocked != nil }, set: { if !$0 { blocked = nil } })) {
            Button("OK", role: .cancel) { }
        } message: {
            Text(blocked ?? "")
        }
        .help(engine.deepAnalyzeInFlight ? "FileID is already describing files. Try again when it finishes." : fits
              ? "Write a description and a suggested name for this file (uses \(settings.activeKind.displayName))"
              : "\(settings.activeKind.displayName) needs more memory than this Mac has. Choose a smaller description model in Settings.")
    }
}
