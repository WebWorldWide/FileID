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
                if let progress = engine.modelDownloadProgress {
                    Divider().opacity(0.3)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(progress.message).font(Theme.Font.body(12, .semibold))
                        ProgressView(value: progress.fraction).tint(Theme.gold)
                    }
                }
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
        .help(fits ? "" : "This model needs \(String(format: "%.1f", kind.ramBudgetGB)) GB resident RAM. Choose a smaller model for this \(Int(ramGB)) GB Mac. FileID checks remaining memory again before loading.")
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
            BadgePill(label: "Needs \(Int(kind.ramBudgetGB)) GB RAM (you have \(Int(ramGB)))",
                       color: Theme.warning)
        } else if installed {
            BadgePill(label: "Downloaded", color: Theme.delight)
        } else {
            let gb = String(format: "%.1f GB", Double(kind.approxBytes) / 1_073_741_824)
            BadgePill(label: "\(gb) download", color: .secondary)
        }
    }

    @ViewBuilder
    private var statsLine: some View {
        Text(kind.secondsPerImage > 0 ? "Needs ≈ \(String(format: "%.1f", kind.ramBudgetGB)) GB memory · about \(String(format: "%.0f", kind.secondsPerImage)) s a photo · \(kind.licenseName)" : "≈ \(String(format: "%.1f", kind.ramBudgetGB)) GB RAM · Speed not measured · \(kind.licenseName)")
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

    // Item 4: lay-person explainer distinguishing automatic in-scan Tagging
    // from the opt-in Deep Analyze pass. Dismissible; stays hidden once read.
    @AppStorage("hideDeepAnalyzeExplainer") private var hideExplainer = false

    // Item 5: "Apply to your files" — write tags / people / smart names onto the
    // actual files. Single in-flight gate + a short result line.
    @State private var applyInFlight = false
    @State private var applyStatus: String? = nil
    // "Apply all" renames every pending file on disk in one tap with no review
    // list (unlike BulkRenameSheet). The rename is the irreversible part, so gate
    // it behind a confirmation that states the count. (HIGH #5)
    @State private var confirmApplyAll = false

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
                // Smart names produced by Deep Analyze stack up here for
                // bulk apply. Lives in this tab (not Library) because
                // smart names ARE Deep Analyze's output — putting the
                // bulk-rename trigger anywhere else split the workflow.
                if pendingRenameCount > 0 {
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
        .onChange(of: engine.deepAnalyzeComplete?.processed ?? -1) { _, _ in
            refreshPendingRenameCount()
            refreshStatusCounts()
        }
        .onChange(of: store.version) { _, _ in throttledRefresh() }
        .onChange(of: settings.activeKind.rawValue) { _, _ in
            refreshStatusCounts()   // pending is keyed by active model
        }
        .sheet(isPresented: $bulkRenameSheetOpen, onDismiss: refreshPendingRenameCount) {
            BulkRenameSheet(store: store)
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
    /// don't fire 1000× — including while this tab is off-screen. Leading
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
                    secondaryApplyButton("Add tags to files", systemImage: "tag",
                                         disabled: applyInFlight) { runApply(.tags) }
                    secondaryApplyButton("Add people's names to files", systemImage: "person.crop.square",
                                         disabled: applyInFlight || !hasNamedAnyone) { runApply(.people) }
                        .help(hasNamedAnyone ? "Tag each photo with the names of the people in it" : "Name someone on the People page first")
                }
                Button {
                    // Rename-on-disk is the destructive step; confirm first when
                    // there are files to rename. A tag-only apply (no pending
                    // renames) is additive/reversible, so let it run straight away.
                    if pendingRenameCount > 0 {
                        confirmApplyAll = true
                    } else {
                        runApply(.all)
                    }
                } label: {
                    Label(pendingRenameCount > 0 ? "Add tags and rename \(pendingRenameCount) file\(pendingRenameCount == 1 ? "" : "s")" : "Add tags and names", systemImage: "checkmark.circle")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(DSSecondaryButtonStyle(height: 30))
                .help(pendingRenameCount > 0 ? "Adds tags and people, and renames files to their suggested names. You confirm first." : "Adds tags and people's names to the files")
                .disabled(applyInFlight)
                .confirmationDialog(
                    "Rename \(pendingRenameCount) file\(pendingRenameCount == 1 ? "" : "s") to the suggested names?",
                    isPresented: $confirmApplyAll,
                    titleVisibility: .visible
                ) {
                    Button("Rename \(pendingRenameCount) file\(pendingRenameCount == 1 ? "" : "s")", role: .destructive) {
                        runApply(.all)
                    }
                    Button("Cancel", role: .cancel) { }
                } message: {
                    Text("The files keep their folders; only their names change. Tags and people are added too. To put the old names back, use Undo last rename at the top of Library.")
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

    /// Runs the requested apply off the main thread (DB read + FS writes) and
    /// reports a short summary. `.all` renames to smart names, then writes
    /// keyword tags + people tags.
    private func runApply(_ scope: ApplyScope) {
        guard !applyInFlight else { return }
        applyInFlight = true
        applyStatus = "Applying…"
        let store = self.store
        Task { @MainActor in
            var summary: [String] = []
            if scope == .all {
                let renamed: [ReadStore.RenameOutcome] = await Task.detached(priority: .userInitiated) {
                    let pending = store.filesWithProposedNames()
                    guard !pending.isEmpty else { return [] }
                    let result = store.applyProposedNamesBulk(pending)
                    // Journal THIS batch so Library's "Undo last rename" reverts it,
                    // not some older batch — matching every other rename call site
                    // (BulkRenameSheet, per-file rename). saveLastBatch is
                    // nonisolated, so it's safe to call from this detached task.
                    BulkRenameSheet.saveLastBatch(result.renamed)
                    return result.renamed
                }.value
                if !renamed.isEmpty { summary.append("\(renamed.count) renamed") }
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
                Button {
                    bulkRenameSheetOpen = true
                } label: {
                    Label("Review suggested names…", systemImage: "wand.and.rays")
                }
                .buttonStyle(DSPrimaryButtonStyle(height: 30))
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
        }
    }

    private var unavailableCard: some View {
        Panel(padding: 16) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(Theme.Font.body(20, .semibold))
                    .foregroundStyle(Theme.warning)
                VStack(alignment: .leading, spacing: 6) {
                    Text("Describing isn't available in this build")
                        .font(Theme.Font.body(14, .semibold))
                    Text(engine.deepAnalyzeUnavailableReason ??
                         "mlx.metallib was not compiled. Run ./run.sh — it will fail with install instructions if cmake or the Metal Toolchain is missing.")
                        .font(Theme.Font.body(13.5))
                        .foregroundStyle(Theme.text3)
                        .fixedSize(horizontal: false, vertical: true)
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
        let pendingMins = Double(totals.pending) * settings.activeKind.secondsPerImage / 60.0
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
                            Text("about \(DurationText.short(pendingMins * 60))")
                                .font(Theme.Font.body(13, .medium)).monospacedDigit()
                        }
                    }
                    GridRow {
                        Text("AI model").foregroundStyle(Theme.text3)
                        Text(settings.activeKind.displayName).font(Theme.Font.body(13, .medium))
                            .help("Change the description model in Settings")
                    }
                }
                .font(Theme.Font.body(13))
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
        }
    }

    private var runBlocker: String? {
        if !engine.deepAnalyzeAvailable { return "Describing isn't available in this build." }
        if !settings.activeKind.fits(ramGB: settings.systemRAMGB) { return "\(settings.activeKind.displayName) needs more memory than this Mac has. Choose a smaller model in Settings." }
        if engine.deepAnalyzeInFlight { return "FileID is already describing files." }
        if pendingTotals.total == 0 { return "Scan a folder first." }
        if skipExisting && pendingTotals.pending == 0 { return "Every file is already described. Turn off \"Only files not described yet\" to describe them again." }
        return nil
    }

    private var runButton: some View {
        let count = skipExisting ? pendingTotals.pending : pendingTotals.total
        return Button {
            guard ModelLicenseGate.ensureAccepted(for: settings.activeKind) else { return }
            engine.deepAnalyzeAll(modelKind: settings.activeKind.rawValue, skipExisting: skipExisting)
        } label: {
            Label(count > 0 ? "Describe \(count.formatted()) file\(count == 1 ? "" : "s")" : "Describe files", systemImage: "wand.and.stars")
        }
        .disabled(runBlocker != nil)
        .help(runBlocker ?? "Write a description and a suggested name for each file. You can stop at any time.")
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
        Panel(padding: 16) {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 12) {
                    ProgressView()
                        .controlSize(.regular)
                        .tint(Theme.gold)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Getting the AI ready…").font(Theme.Font.body(14, .semibold))
                        Text(startingSubtitle)
                            .font(Theme.Font.body(12))
                            .foregroundStyle(Theme.text3)
                            .contentTransition(.opacity)
                            .animation(.easeInOut(duration: 0.18),
                                       value: engine.deepAnalyzeStarting?.message)
                    }
                    Spacer()
                    Button("Stop") {
                        engine.deepAnalyzeCancel()
                    }
                    .buttonStyle(.dsSecondary)
                }
                ShimmerView(cornerRadius: 3)
                    .frame(height: 4)
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

    var body: some View {
        let alreadyDone = file.isFullyAnalyzed(by: settings.activeKind.rawValue)
        let fits = settings.activeKind.fits(ramGB: settings.systemRAMGB)
        Button {
            guard fits else { return }
            guard ModelLicenseGate.ensureAccepted(for: settings.activeKind) else { return }
            engine.deepAnalyzeFile(fileID: file.id, modelKind: settings.activeKind.rawValue)
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
        .help(engine.deepAnalyzeInFlight ? "FileID is already describing files. Try again when it finishes." : fits
              ? "Write a description and a suggested name for this file (uses \(settings.activeKind.displayName))"
              : "\(settings.activeKind.displayName) needs more memory than this Mac has. Choose a smaller description model in Settings.")
    }
}
