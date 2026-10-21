// Restructure tab — three-tier "assistant" model (Anchor / Mixed / Junk).
// We respect the user's existing organization decisions:
//
//   Anchor folder (meaningful name + homogeneous content):
//     File stays in place. Nothing about its folder changes.
//   Mixed folder (meaningful name but a few outliers):
//     The matching files stay; outliers move out into their proper
//     anchor bucket via the standard People → Places → Documents →
//     Year heuristic.
//   Junk folder (generic name OR fully heterogeneous):
//     Every file dissolves — re-buckets via the heuristic.
//
// The plan + apply now route through the ENGINE butler
// (Restructure.proposeAll / Restructure.apply via IPC), not the app-side
// classifier. `requestPlan()` sends `planRestructure`; the engine's
// `restructurePlan` event is mapped onto the view's `Proposal`/`AssistantSummary`
// model in `applyPlan()` and drives the Sankey + TreeDiff + recommendation rows.
// Apply sends `applyRestructure` and reacts to `restructureApplyResult`.
import SwiftUI
import AppKit
import GRDB
import FileIDShared

struct RestructureView: View {
    let store: ReadStore
    let engine: EngineClient
    let selectedRoot: URL?

    @State private var libraryRoot: URL?
    @State private var proposals: [Proposal] = []
    @State private var summary: AssistantSummary = .empty
    @State private var groups: [Group] = []
    /// Per-outcome groups precomputed in `applyPlan()` so
    /// `inlineFileList(for:)` doesn't rebuild them on every render.
    @State private var inlineGroupsByOutcome: [RestructureOutcome: [Group]] = [:]
    @State private var inlineMatchedCountByOutcome: [RestructureOutcome: Int] = [:]
    @State private var selectedIDs: Set<Int64> = []
    @State private var loading = false
    @State private var status: String?
    @State private var statusIsError = false
    @State private var staysPutExpanded: Bool = false
    @State private var confirmApply: Bool = false
    /// Single-flight guard for the apply / convert paths. A real move is
    /// irreversible inside the app, so a double-click must never run apply
    /// twice — the bar buttons disable while this is set.
    @State private var applying = false
    @State private var cancelRequested = false
    /// Moves frozen at the instant the apply-confirmation dialog is
    /// presented. A background re-plan (e.g. Deep Analyze finishing mid-review)
    /// rewrites proposals/selectedIDs/engine.restructurePlan while the dialog
    /// stays open; the confirm action must apply exactly what the user reviewed,
    /// not the re-planned set (TOCTOU → unreviewed irreversible moves).
    @State private var pendingMoves: [RestructureMove] = []
    @State private var pendingPlanID: String?
    @State private var pendingApplyCount = 0
    @State private var storedPlanID: String?
    @State private var storedPlanMoveCount = 0
    /// Full-plan confidence tallies for the active stored plan — the auto-tier
    /// count is what actually applies (see `applyStoredPlan`'s Auto-only
    /// filter); review/ask/unknown stay put. Nil until a truncated plan lands.
    @State private var storedPlanConfidenceCounts: RestructureConfidenceCounts?

    @AppStorage("restructure.viewMode") private var viewModeRaw: String = ViewMode.cards.rawValue
    private var viewMode: ViewMode { ViewMode(rawValue: viewModeRaw) ?? .cards }
    enum ViewMode: String { case cards, tree }

    @State private var skippedOutcomes: Set<RestructureOutcome> = []
    @State private var expandedOutcome: RestructureOutcome?
    @State private var drillDown: DrillDownScope?
    /// Filtered + grouped proposals for the active drill-down scope.
    /// Rebuilt only when `drillDown` changes so a per-row checkbox tap
    /// inside the sheet doesn't re-filter + re-group the whole set.
    @State private var drillDownGroups: [Group] = []

    /// Shared hover state used by the Sankey, recommendation rows,
    /// tree, and staysPut disclosure to drive cross-highlight.
    @State private var hoverBus = RestructureHoverBus()
    @State private var captionedFraction: Double = 0
    @State private var totalAnalyzableFiles = 0
    @AppStorage("restructure.dismissedDeepAnalyzeHint") private var dismissedDeepAnalyzeHint = false
    /// (clip, text) embedding counts at plan time. nil = not yet measured. When both are
    /// 0 the scan ran without the CLIP/BGE models, so the plan is date/name-only — we tell
    /// the user to install the models + rescan for content-based grouping.
    @State private var embeddingCounts: (clip: Int, text: Int)?
    /// Lets the missing-models banner jump the user to Settings → AI Models.
    @AppStorage("activeTabRawValue") private var activeTabRaw: String = MainWindow.Tab.library.rawValue
    /// Per-file deselections captured at `requestPlan()` time and re-applied in
    /// `applyPlan()`, so an engine re-plan (e.g. after Deep Analyze finishes
    /// mid-review) doesn't silently re-check rows the user unchecked.
    @State private var priorDeselectedIDs: Set<Int64> = []

    /// Which subset of proposals the drill-down sheet renders.
    enum DrillDownScope: Identifiable, Hashable {
        case all
        case outcome(RestructureOutcome)
        case sourceFolder(String)
        case destBucket(String)
        /// Rollup of long-tail source folders that were collapsed into
        /// the Sankey's "+ N more folders" node. Tapping the rollup
        /// node should drill into every proposal whose source folder
        /// is in this list — not into a literal "+ N more folders"
        /// folder, which doesn't exist.
        case sourceFolders([String])
        case destBuckets([String])
        var id: String {
            switch self {
            case .all:                       return "all"
            case .outcome(let o):            return "outcome:\(o.rawValue)"
            case .sourceFolder(let f):       return "src:\(f)"
            case .destBucket(let b):         return "dst:\(b)"
            case .sourceFolders(let fs):     return "srcs:" + fs.sorted().joined(separator: "\u{1F}")
            case .destBuckets(let bs):       return "dsts:" + bs.sorted().joined(separator: "\u{1F}")
            }
        }
    }

    /// Why this file is moving. Used to group the diff preview by impact.
    enum ProposalKind: Sendable, Hashable {
        case dissolved          // file from a Junk folder
        case movedOutAsOutlier  // outlier from a Mixed folder
    }

    struct Proposal: Identifiable, Hashable {
        var id: Int64 { fileID }
        let fileID: Int64
        let oldPath: String
        let newPath: String
        let bucket: String        // destination bucket (e.g. "People/Marie Curie")
        let sourceFolder: String  // current parent folder (for "from X" display)
        // Precomputed display strings — avoids re-parsing the path on every row render.
        let filename: String
        let sourceName: String
        let kind: ProposalKind
        // Butler confidence band ("auto"/"review"/"ask", "" when unstamped) + the
        // engine's plain-language "why filed here". Surfaced per the deep-research
        // finding that showing confidence + reason is the #1 trust signal. (item D1)
        var confidence: String = ""
        var reason: String? = nil
        /// True for the top "auto" band — drives the "Select confident" helper.
        var isHighConfidence: Bool { confidence.lowercased() == "auto" }
    }

    struct Group: Identifiable {
        var id: String { bucket }
        let bucket: String
        let proposals: [Proposal]
    }

    /// Human-readable summary of what the assistant just did to the library.
    struct AssistantSummary: Sendable {
        var anchorFolders: Int          // kept intact
        var mixedFolders: Int           // kept, with outliers extracted
        var junkFolders: Int            // dissolved
        var staysPutFiles: Int          // unchanged
        var movedOutFiles: Int          // outliers
        var dissolvedFiles: Int         // junk-folder contents
        /// Per-anchor-folder breakdown: (folder display name, file count
        /// staying inside it). Sorted alphabetically. Used by the
        /// "Staying put" disclosure to show which folders are anchored.
        var staysPutBreakdown: [(folder: String, count: Int)]

        static let empty = AssistantSummary(
            anchorFolders: 0, mixedFolders: 0, junkFolders: 0,
            staysPutFiles: 0, movedOutFiles: 0, dissolvedFiles: 0,
            staysPutBreakdown: []
        )

        var hasContent: Bool {
            anchorFolders + mixedFolders + junkFolders > 0
        }
    }

    var body: some View {
        // The step cells stay pinned above the scrolling plan so the Move
        // action is visible the moment a plan lands. (owner request)
        VStack(alignment: .leading, spacing: 0) {
            if libraryRoot != nil {
                stepCells
                    .padding(.horizontal, Theme.Metrics.pageInsetX)
                    .padding(.top, 16).padding(.bottom, 12)
                    .transition(.opacity)
                    // The engine butler performs real on-disk moves (macOS has
                    // no shortcut-preview mode), so the confirmation states the
                    // real behavior before anything moves.
                    .confirmationDialog(
                        "Move \(pendingApplyCount) file\(pendingApplyCount == 1 ? "" : "s")?",
                        isPresented: $confirmApply,
                        titleVisibility: .visible
                    ) {
                        Button("Move files", role: .destructive) {
                            applyViaEngine()
                        }
                        Button("Cancel", role: .cancel) { }
                    } message: {
                        Text("The files move into the new folders. Nothing is deleted, and Undo move puts every file back where it was.")
                    }
                Hairline()
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    header
                    if storedPlanMoveCount > 0 {
                        largePlanCard
                    } else if libraryRoot == nil
                        || (!summary.hasContent && proposals.isEmpty) {
                        emptyState
                    } else {
                        if !proposals.isEmpty || summary.hasContent {
                            RestructureStatHero(summary: summary, hoverBus: hoverBus)
                            if shouldShowMissingContentModels {
                                missingContentModelsBanner
                            } else if shouldShowDeepAnalyzeHint {
                                deepAnalyzeHintBanner
                            }
                        }
                        if !proposals.isEmpty {
                            mapPanel
                            recommendationsPanel
                        } else if summary.hasContent {
                            nothingToMoveCard
                        }
                        if summary.staysPutFiles > 0 {
                            staysPutSection
                        }
                    }
                    if let s = status {
                        statusBanner(s, isError: statusIsError)
                    }
                }
                .padding(.horizontal, Theme.Metrics.pageInsetX)
                .padding(.top, Theme.Metrics.pageInsetY)
                .padding(.bottom, 24)
            }
            .focusSection()
        }
        .animation(Theme.springStandard, value: applyBarVisible)
        .animation(Theme.springStandard, value: engine.canUndoRestructure)
        .sheet(item: $drillDown) { scope in
            drillDownSheet(scope)
        }
        .task(id: selectedRoot?.standardizedFileURL.path) {
            guard libraryRoot?.standardizedFileURL.path
                    != selectedRoot?.standardizedFileURL.path else { return }
            libraryRoot = selectedRoot
            proposals = []
            groups = []
            selectedIDs = []
            summary = .empty
            if selectedRoot != nil { requestPlan() }
        }
        // Re-request a plan only on Deep Analyze terminal events
        // (`.deepAnalyzeComplete`). The previous per-file 3-s throttle
        // still triggered up to 50K full re-plans during a long batch —
        // `proposeAll` walks every file in the library, so even at 3-s
        // spacing it caused UI stutters and slowed the engine. Terminal-only
        // is the right granularity: Deep Analyze is a single batch run, and
        // proposals don't need to track mid-flight progress.
        .onChange(of: engine.deepAnalyzeComplete?.processed ?? -1) { _, _ in
            guard libraryRoot != nil else { return }
            store.notifyChanged()
            requestPlan()
        }
        // Engine-authoritative plan arrived — map it onto the
        // view's proposal model + summary and rebuild the render caches.
        .onChange(of: engine.restructurePlanSignal) { _, _ in
            applyPlan()
        }
        // Apply finished engine-side — surface the result, release the
        // single-flight guard, and re-request a fresh plan (moved files now
        // carry new path_text).
        .onChange(of: engine.restructureApplyResultSignal) { _, _ in
            guard libraryRoot != nil, let result = engine.restructureApplyResult else { return }
            applying = false
            if let priv = result.privilegeError, !priv.isEmpty {
                status = priv
                statusIsError = true
            } else if result.cancelled {
                status = "Stopped — \(result.applied) file\(result.applied == 1 ? "" : "s") already moved, still undoable."
                statusIsError = false
            } else {
                status = "\(result.applied) moved · \(result.failed) failed"
                statusIsError = false
            }
            store.notifyChanged()
            requestPlan()
        }
        // The engine died before a terminal plan/apply event could clear the
        // local in-flight UI — release it so the tab doesn't wedge.
        .onChange(of: engine.engineResetSignal) { _, _ in
            if applying {
                applying = false
                status = "FileID restarted in the middle of the move. Files already moved can be put back with Undo move; try again for the rest."
                statusIsError = true
            }
            loading = false
        }
        // A plan computation that failed engine-side ends the "Computing…"
        // spinner (it would otherwise spin forever waiting for a plan event).
        // Keyed on `lastErrorSignal` (bumps on every error write) not the
        // message, so a retry that fails with the identical message still
        // clears the spinner. An oversized plan frame dropped by the app's
        // inbound IPC cap (`ipc_frame_too_large`) is a terminal plan failure
        // here too — otherwise the tab wedges on a very large library's plan.
        .onChange(of: engine.lastErrorSignal) { _, _ in
            guard let kind = engine.lastError?.kind else { return }
            // An apply/undo the engine rejected with a bare error (db_unavailable:
            // the engine stays alive, so NO restructureApplyResult and NO reset
            // event arrives) would wedge `applying` true forever — the apply bar +
            // Undo button stay disabled and re-entry-guarded. Release it here,
            // mirroring the engineResetSignal handler above.
            if applying, kind == "db_unavailable" {
                applying = false
                status = "FileID is still getting ready. Try again in a moment."
                statusIsError = true
            }
            guard loading else { return }
            switch kind {
            case "plan_restructure_failed", "db_unavailable":
                loading = false
                status = "FileID couldn't work out a plan. Try again."
                statusIsError = true
            case "ipc_frame_too_large":
                loading = false
                status = "This folder has too many files to show a plan. Try a smaller folder."
                statusIsError = true
            default:
                break
            }
        }
        // Group the drill-down's proposals once per scope, not on every
        // checkbox tap inside the sheet.
        .onChange(of: drillDown) { _, newScope in
            rebuildDrillDownGroups(for: newScope)
        }
    }

    // MARK: - Step cells

    private var applyBarVisible: Bool {
        libraryRoot != nil && (storedPlanMoveCount > 0 || !proposals.isEmpty)
    }

    private var planReady: Bool { !loading && (storedPlanMoveCount > 0 || !proposals.isEmpty || summary.hasContent) }
    private var plannedMoveCount: Int { storedPlanMoveCount > 0 ? storedPlanMoveCount : proposals.count }
    private var applyCount: Int {
        storedPlanMoveCount > 0 ? (storedPlanConfidenceCounts?.auto ?? storedPlanMoveCount) : selectedIDs.count
    }

    private func beginApply() {
        guard !applying else { return }
        if storedPlanMoveCount > 0 {
            pendingMoves = []
            pendingPlanID = storedPlanID
            // A stored plan only applies its Auto tier, so the confirmation
            // promises exactly that count.
            pendingApplyCount = storedPlanConfidenceCounts?.auto ?? storedPlanMoveCount
        } else {
            pendingMoves = eligibleMoves()
            pendingPlanID = nil
            pendingApplyCount = pendingMoves.count
        }
        guard pendingApplyCount > 0 else { return }
        confirmApply = true
    }

    private func undoLastMove() {
        guard let root = libraryRoot, !applying else { return }
        applying = true
        status = "Putting the moved files back…"
        statusIsError = false
        // A failed send never gets a result event, so release the guard here.
        if !engine.undoRestructure(libraryRoot: root.path) {
            applying = false
            status = "FileID is still getting ready. Try again in a moment."
            statusIsError = true
        }
    }

    private var stepCells: some View {
        let moved = engine.canUndoRestructure
        let previewState: RestructureStepState = loading ? .current : (planReady || moved ? .done : .current)
        let reviewState: RestructureStepState = !planReady ? .upcoming : (applyCount > 0 || moved ? .done : .current)
        let moveState: RestructureStepState = applying ? .current : (moved ? .done : (planReady && applyCount > 0 ? .current : .upcoming))
        return HStack(spacing: 0) {
            RestructureStepCell(number: 1, title: "Preview", state: previewState,
                                detail: loading ? "Reading every folder and choosing a home for each file…"
                                    : (planReady ? "\(plannedMoveCount.formatted()) file\(plannedMoveCount == 1 ? "" : "s") to move · \(sankeyHeaderStat)" : "FileID suggests a tidier layout for this folder. Nothing moves yet.")) {
                if loading {
                    IndeterminateBar().frame(maxWidth: 160)
                } else {
                    Button(planReady ? "Suggest again" : "Suggest a layout") { requestPlan() }
                        .buttonStyle(planReady ? AnyButtonStyle(DSGhostButtonStyle()) : AnyButtonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.hitTarget)))
                        .disabled(applying)
                        .help(applying ? "Wait for the move to finish" : "Work out a new plan from the files as they are now")
                }
            }
            VHairline()
            RestructureStepCell(number: 2, title: "Review", state: reviewState,
                                detail: !planReady ? "Check each group of moves and skip any you don't want."
                                    : (storedPlanMoveCount > 0
                                        ? "Large plan: only the \(applyCount.formatted()) confident moves apply. The rest stay put."
                                        : "\(applyCount.formatted()) of \(plannedMoveCount.formatted()) moves selected")) {
                if planReady && !proposals.isEmpty {
                    Button("Review all moves") { drillDown = .all }
                        .buttonStyle(.dsGhost)
                        .disabled(applying)
                }
            }
            VHairline()
            RestructureStepCell(number: 3, title: "Move", state: moveState,
                                detail: applying ? (cancelRequested ? "Stopping after the current file…" : "Moving files on disk…")
                                    : (moved ? "Files were moved. Undo move puts them back." : "Moves the chosen files into their new folders.")) {
                if applying {
                    IndeterminateBar().frame(maxWidth: 120)
                    Button(cancelRequested ? "Stopping…" : "Cancel") {
                        guard !cancelRequested else { return }
                        cancelRequested = true
                        engine.cancelRestructure()
                    }
                    .buttonStyle(DSDangerButtonStyle(height: Theme.Metrics.hitTarget))
                    .disabled(cancelRequested)
                    .help("Stops after the file being moved. Files already moved stay moved and can be put back.")
                } else {
                    if planReady && applyCount > 0 {
                        Button("Move \(applyCount.formatted()) file\(applyCount == 1 ? "" : "s")") { beginApply() }
                            .buttonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.hitTarget))
                            .help("Move the chosen files into their new folders. You confirm first, and you can undo.")
                    }
                    if moved {
                        Button { undoLastMove() } label: { Label("Undo move", systemImage: "arrow.uturn.backward") }
                            .buttonStyle(DSAccentOutlineButtonStyle(height: Theme.Metrics.hitTarget))
                            .help("Put every file from the last move back where it was")
                    }
                }
            }
        }
        .fixedSize(horizontal: false, vertical: true)
        .background(Theme.panel, in: RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous).stroke(Theme.hairlineStrong.opacity(0.8), lineWidth: 1))
        .focusSection()
        .onChange(of: applying) { _, now in if !now { cancelRequested = false } }
    }

    // MARK: - Notices

    private func notice<Actions: View>(tone: Color, title: String, body: String, @ViewBuilder actions: () -> Actions) -> some View {
        HStack(alignment: .center, spacing: 14) {
            Rectangle().fill(tone).frame(width: 3).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(Theme.Font.body(13.5, .semibold)).foregroundStyle(Theme.text1)
                Text(body).font(Theme.Font.body(12.5)).foregroundStyle(Theme.text2)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.vertical, 12)
            Spacer(minLength: 8)
            actions().padding(.trailing, 14)
        }
        .background(tone.opacity(0.05), in: RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .background(Theme.panel, in: RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous).stroke(Theme.hairlineStrong.opacity(0.8), lineWidth: 1))
        .accessibilityElement(children: .contain)
    }

    private var shouldShowDeepAnalyzeHint: Bool {
        guard !dismissedDeepAnalyzeHint else { return false }
        guard engine.deepAnalyzeAvailable else { return false }
        guard !engine.deepAnalyzeInFlight else { return false }
        guard totalAnalyzableFiles > 0 else { return false }
        return captionedFraction < 0.4
    }

    /// The scan produced no content embeddings (CLIP + BGE absent), so the
    /// plan can only sort by date and name: the usual cause of a plan that
    /// files everything into year folders.
    private var shouldShowMissingContentModels: Bool {
        guard let c = embeddingCounts else { return false }
        guard totalAnalyzableFiles > 0 else { return false }
        return c.clip == 0 && c.text == 0
    }

    private var clipModelInstalled: Bool {
        if case .installed = CLIPModelInstaller.shared.status { return true }
        return false
    }

    private var missingContentModelsBanner: some View {
        notice(tone: Theme.warning,
               title: clipModelInstalled ? "Rescan so FileID can see what's in your photos" : "Sorting by date only",
               body: clipModelInstalled
                ? "This folder was scanned before the Search by description model was downloaded. A rescan looks at the photos again; files already up to date are skipped."
                : "Without the Search by description and Documents models, FileID can't see what files contain, so loose files go into year and month folders. Download them in Settings, then rescan.") {
            Button {
                if clipModelInstalled, let root = libraryRoot {
                    engine.startScan(rootURL: root)
                } else {
                    activeTabRaw = MainWindow.Tab.settings.rawValue
                }
            } label: {
                Label(clipModelInstalled ? "Rescan" : "Open Settings", systemImage: clipModelInstalled ? "arrow.counterclockwise" : "gearshape")
            }
            .buttonStyle(DSAccentOutlineButtonStyle(color: Theme.warning, height: Theme.Metrics.hitTarget))
        }
    }

    @ViewBuilder
    private var deepAnalyzeHintBanner: some View {
        let captioned = Int((captionedFraction * Double(totalAnalyzableFiles)).rounded())
        let pct = Int((captionedFraction * 100).rounded())
        notice(tone: Theme.ai,
               title: "Better suggestions with descriptions",
               body: captioned == 0
                ? "Right now files are sorted by folder name and people. Describing files lets FileID see what each one shows, so receipts go to Documents and screenshots to Photos."
                : "\(captioned.formatted()) of \(totalAnalyzableFiles.formatted()) files are described (\(pct)%). Describing the rest gives the plan more to work with.") {
            HStack(spacing: 6) {
                Button("Not now") { dismissedDeepAnalyzeHint = true }
                    .buttonStyle(.dsGhost)
                Button {
                    engine.deepAnalyzeAll(modelKind: DeepAnalyzeSettings.shared.activeKind.rawValue, skipExisting: true)
                } label: { Label("Describe files", systemImage: "sparkles") }
                    .buttonStyle(DSAccentOutlineButtonStyle(color: Theme.ai, height: Theme.Metrics.hitTarget))
            }
        }
    }

    // MARK: - Map and recommendations

    private var viewModeBinding: Binding<ViewMode> {
        Binding(get: { viewMode }, set: { viewModeRaw = $0.rawValue })
    }

    private var mapPanel: some View {
        Panel {
            PanelHeader(viewMode == .cards ? "Folder map" : "Now → After") {
                MetaText(sankeyHeaderStat, size: 12).monospacedDigit()
                SegmentedBar(selection: viewModeBinding, options: [(.cards, "Map"), (.tree, "Tree")])
                    .help("Map traces where files go; Tree shows the folders now and after side by side")
            }
            VStack(alignment: .leading, spacing: 8) {
                MetaText(viewMode == .cards
                         ? "Hover a folder or ribbon to trace its files. Click to see the exact list."
                         : "Click any folder to see its files.", size: 12)
                if viewMode == .cards {
                    SankeyFlowView(
                        proposals: proposals,
                        // `folder` is the full source path, so same-named
                        // folders stay distinct.
                        onTapSource: { folder in drillDown = .sourceFolder(folder) },
                        onTapDestination: { bucket in drillDown = .destBucket(bucket) },
                        onTapSourceRollup: { folders in drillDown = .sourceFolders(folders) },
                        onTapDestRollup: { buckets in drillDown = .destBuckets(buckets) },
                        hoverBus: hoverBus
                    )
                } else {
                    TreeDiffView(
                        proposals: proposals,
                        summary: summary,
                        onTapSource: { folder in drillDown = .sourceFolder(folder) },
                        onTapDestination: { bucket in drillDown = .destBucket(bucket) },
                        hoverBus: hoverBus
                    )
                }
            }
            .padding(14)
        }
    }

    private var recommendationsPanel: some View {
        Panel {
            PanelHeader("Suggestions") {
                MetaText("Skip a group to leave it out of the move", size: 12)
            }
            VStack(spacing: 0) {
                if summary.anchorFolders > 0 {
                    RestructureRecommendationRow(
                        outcome: .keep,
                        headline: "Keep \(summary.anchorFolders) folder\(summary.anchorFolders == 1 ? "" : "s") as they are",
                        bodyText: "Clear names and matching contents. \(summary.staysPutFiles.formatted()) file\(summary.staysPutFiles == 1 ? "" : "s") stay exactly where they are.",
                        fileCount: summary.staysPutFiles,
                        folderCount: summary.anchorFolders,
                        isApproved: true,
                        isInformational: true,
                        hoverBus: hoverBus,
                        onHover: { hovering in hoverBus.set(hovering ? .outcome(.keep) : nil) },
                        expandedContent: { EmptyView() }
                    )
                }
                if summary.mixedFolders > 0 {
                    if summary.anchorFolders > 0 { Hairline(color: Theme.hairlineSoft) }
                    RestructureRecommendationRow(
                        outcome: .tidy,
                        headline: "Tidy \(summary.mixedFolders) folder\(summary.mixedFolders == 1 ? "" : "s"): move \(summary.movedOutFiles.formatted()) misplaced file\(summary.movedOutFiles == 1 ? "" : "s")",
                        bodyText: "Mostly organized folders with a few files that don't fit. The folder stays; the misplaced files move to where they belong.",
                        fileCount: summary.movedOutFiles,
                        folderCount: summary.mixedFolders,
                        isApproved: !skippedOutcomes.contains(.tidy),
                        isExpanded: expandedOutcome == .tidy,
                        hoverBus: hoverBus,
                        onToggleApproval: { toggleSkip(.tidy) },
                        onToggleExpand: { toggleExpand(.tidy) },
                        onHover: { hovering in hoverBus.set(hovering ? .outcome(.tidy) : nil) },
                        expandedContent: { inlineFileList(for: .outcome(.tidy)) }
                    )
                }
                if summary.junkFolders > 0 {
                    if summary.anchorFolders > 0 || summary.mixedFolders > 0 { Hairline(color: Theme.hairlineSoft) }
                    RestructureRecommendationRow(
                        outcome: .reorganize,
                        headline: "Reorganize \(summary.junkFolders) folder\(summary.junkFolders == 1 ? "" : "s"): sort \(summary.dissolvedFiles.formatted()) file\(summary.dissolvedFiles == 1 ? "" : "s")",
                        bodyText: "Folders with generic names like \"Untitled\" or \"Camera Roll\". Their files are sorted into People, Places, Documents, events or Photos by year.",
                        fileCount: summary.dissolvedFiles,
                        folderCount: summary.junkFolders,
                        isApproved: !skippedOutcomes.contains(.reorganize),
                        isExpanded: expandedOutcome == .reorganize,
                        hoverBus: hoverBus,
                        onToggleApproval: { toggleSkip(.reorganize) },
                        onToggleExpand: { toggleExpand(.reorganize) },
                        onHover: { hovering in hoverBus.set(hovering ? .outcome(.reorganize) : nil) },
                        expandedContent: { inlineFileList(for: .outcome(.reorganize)) }
                    )
                }
            }
        }
    }

    private func statusBanner(_ message: String, isError: Bool = false) -> some View {
        notice(tone: isError ? Theme.warning : Theme.delight, title: isError ? "Something went wrong" : "Done", body: message) { EmptyView() }
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("FileID keeps your well-named folders and suggests a tidier home for everything else. Nothing moves until you press Move.")
                .font(Theme.Font.body(13.5)).foregroundStyle(Theme.text2)
                .fixedSize(horizontal: false, vertical: true)
            if let root = libraryRoot {
                Text(root.path)
                    .font(Theme.Font.mono(11)).foregroundStyle(Theme.text3)
                    .lineLimit(1).truncationMode(.head)
                    .help(root.path)
                    .accessibilityLabel("Library folder \(root.path)")
            }
        }
    }

    private var nothingToMoveCard: some View {
        notice(tone: Theme.delight, title: "Nothing to move",
               body: "This library is already organized: every folder has a clear name and matching contents.") { EmptyView() }
    }

    private var largePlanCard: some View {
        let body: String
        if let counts = storedPlanConfidenceCounts {
            let heldBack = counts.review + counts.ask + counts.unknown
            body = "All \(storedPlanMoveCount.formatted()) moves are stored as one undoable run. Move applies the \(counts.auto.formatted()) confident move\(counts.auto == 1 ? "" : "s"); \(heldBack.formatted()) that need review stay put."
        } else {
            body = "All \(storedPlanMoveCount.formatted()) moves are stored as one undoable run. Move applies only the confident moves."
        }
        return notice(tone: Theme.gold, title: "Large plan ready", body: body) { EmptyView() }
    }

    /// One row inside "Staying put". Hovering it lights the matching node in
    /// the map. A standalone view so its hover-bus read only re-renders the
    /// row, not the whole page.
    private struct StaysPutRow: View {
        let entry: (folder: String, count: Int)
        let hoverBus: RestructureHoverBus
        var body: some View {
            let isHovered = hoverBus.touchesSource(entry.folder)
            HStack(spacing: 10) {
                Circle().fill(Theme.delight.opacity(isHovered ? 1 : 0.7)).frame(width: 7, height: 7)
                    .accessibilityHidden(true)
                Text(entry.folder).font(Theme.Font.mono(12)).foregroundStyle(isHovered ? Theme.text1 : Theme.text2)
                    .lineLimit(1).truncationMode(.middle)
                Spacer()
                Text("\(entry.count.formatted()) file\(entry.count == 1 ? "" : "s")").font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3)
            }
            .padding(.horizontal, 14).padding(.vertical, 6)
            .background(Theme.delight.opacity(isHovered ? 0.08 : 0), ignoresSafeAreaEdges: [])
            .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
            .contentShape(Rectangle())
            .onHover { hovering in hoverBus.set(hovering ? .sourceFolder(entry.folder) : nil) }
            .accessibilityElement(children: .combine)
        }
    }

    private var staysPutSection: some View {
        Panel {
            Button { withAnimation(Theme.springStandard) { staysPutExpanded.toggle() } } label: {
                HStack(spacing: 10) {
                    Image(systemName: "chevron.right").font(.system(size: 9, weight: .bold)).foregroundStyle(Theme.text3)
                        .rotationEffect(.degrees(staysPutExpanded ? 90 : 0))
                    SectionLabel("Staying put")
                    MetaText("\(summary.anchorFolders) folder\(summary.anchorFolders == 1 ? "" : "s") · \(summary.staysPutFiles.formatted()) file\(summary.staysPutFiles == 1 ? "" : "s")", size: 12)
                    Spacer()
                }
                .padding(.horizontal, 14)
                .frame(height: 38)
                .contentShape(Rectangle())
            }
            .buttonStyle(DSRowButtonStyle(radius: 0))
            .accessibilityLabel("Staying put, \(summary.anchorFolders) folders")
            .accessibilityValue(staysPutExpanded ? "Expanded" : "Collapsed")
            if staysPutExpanded {
                Hairline()
                MetaText("Folders with a meaningful name and matching contents. Their files stay exactly where they are.")
                    .padding(.horizontal, 14).padding(.vertical, 8)
                ForEach(Array(summary.staysPutBreakdown.enumerated()), id: \.offset) { _, entry in
                    StaysPutRow(entry: entry, hoverBus: hoverBus)
                }
            }
        }
    }

    @ViewBuilder
    private var emptyState: some View {
        if libraryRoot == nil {
            EmptyStateView(
                icon: "rectangle.3.offgrid",
                title: "Choose a folder first",
                message: "Choose and scan a folder. FileID then suggests a tidier layout inside that same folder, and nothing moves until you check it and press Move."
            )
        } else if loading {
            Panel {
                VStack(spacing: 10) {
                    IndeterminateBar().frame(width: 220)
                    Text("Planning…").font(Theme.Font.display(28)).foregroundStyle(Theme.text1)
                    Text("Looking at every folder, deciding which to keep, and choosing a home for every other file.")
                        .font(Theme.Font.body(12.5)).foregroundStyle(Theme.text3)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 380)
                }
                .padding(32)
                .frame(maxWidth: .infinity, minHeight: 220)
            }
            .accessibilityElement(children: .combine)
        } else {
            EmptyStateView(
                icon: "rectangle.3.offgrid",
                title: "Scan this folder first",
                message: "Once the scan finishes, FileID suggests a tidier layout you can check before anything moves."
            )
        }
    }

    // MARK: - File list

    /// A destination bucket and its file list. Inline lists cap at 50 rows;
    /// the drill-down sheet passes `unlimited` so every file is reachable.
    @ViewBuilder
    private func bucketSection(_ g: Group, unlimited: Bool = false) -> some View {
        let allSelected = g.proposals.allSatisfy { selectedIDs.contains($0.fileID) }
        let cap = 50
        let visible: ArraySlice<Proposal> = unlimited ? ArraySlice(g.proposals) : g.proposals.prefix(cap)
        let hiddenCount = unlimited ? 0 : max(0, g.proposals.count - cap)
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: 10) {
                Image(systemName: bucketIconName(g.bucket))
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.gold)
                    .frame(width: 20, alignment: .center)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 1) {
                    Text(g.bucket).font(Theme.Font.mono(12.5, .medium)).foregroundStyle(Theme.text1)
                    Text("\(g.proposals.count.formatted()) file\(g.proposals.count == 1 ? "" : "s") moving in")
                        .font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3)
                }
                Spacer()
                Button(allSelected ? "Skip all" : "Select all") {
                    if allSelected {
                        for p in g.proposals { selectedIDs.remove(p.fileID) }
                    } else {
                        for p in g.proposals { selectedIDs.insert(p.fileID) }
                    }
                }
                .buttonStyle(.dsGhost)
                .accessibilityLabel(allSelected ? "Skip all files going to \(g.bucket)" : "Select all files going to \(g.bucket)")
            }
            .padding(.bottom, 6)
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(visible)) { p in
                    proposalRow(p)
                }
                if hiddenCount > 0 {
                    MetaText("+ \(hiddenCount.formatted()) more file\(hiddenCount == 1 ? "" : "s")", size: 11.5)
                        .padding(.leading, 8).padding(.top, 4)
                }
            }
            .padding(.leading, 26)
            .overlay(alignment: .leading) {
                Rectangle().fill(Theme.hairlineStrong).frame(width: 1).padding(.leading, 10)
            }
        }
    }

    private func proposalRow(_ p: Proposal) -> some View {
        let on = selectedIDs.contains(p.fileID)
        let filename = p.filename
        let sourceName = p.sourceName
        return Button {
            if on { selectedIDs.remove(p.fileID) } else { selectedIDs.insert(p.fileID) }
        } label: {
            HStack(spacing: 10) {
                DSCheck(on: on)
                Image(systemName: FileTypes.symbolName(forExtension: (filename as NSString).pathExtension))
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.text3)
                    .frame(width: 14, alignment: .center)
                VStack(alignment: .leading, spacing: 1) {
                    Text(filename)
                        .font(Theme.Font.mono(12)).foregroundStyle(on ? Theme.text1 : Theme.text3)
                        .lineLimit(1).truncationMode(.head)
                    Text("from \(sourceName.isEmpty ? "root" : sourceName)")
                        .font(Theme.Font.mono(11)).foregroundStyle(Theme.text3)
                        .lineLimit(1).truncationMode(.head)
                    if let reason = p.reason, !reason.isEmpty {
                        Text(reason)
                            .font(Theme.Font.body(11.5)).foregroundStyle(Theme.text2)
                            .lineLimit(1).truncationMode(.tail)
                    }
                }
                Spacer()
                confidenceBadge(p.confidence)
            }
            .padding(.horizontal, 8).padding(.vertical, 4)
            .frame(minHeight: Theme.Metrics.hitTarget)
        }
        .buttonStyle(DSRowButtonStyle(radius: Theme.Radius.s))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Move \(filename) from \(p.sourceFolder) to \(p.bucket)")
        .accessibilityAddTraits(on ? .isSelected : [])
        .accessibilityHint(on ? "Included in the move. Press to skip it." : "Skipped. Press to include it in the move.")
    }

    /// Item D1: at-a-glance confidence badge for a proposed move. The research's
    /// #1 UX pattern — tier auto/review/ask so the user trusts the sure ones and
    /// scrutinizes the unsure. Nothing shown when the engine didn't stamp a band
    /// (rule-cascade moves), so it never adds noise.
    @ViewBuilder
    private func confidenceBadge(_ confidence: String) -> some View {
        switch confidence.lowercased() {
        case "auto":
            Image(systemName: "checkmark.seal.fill")
                .foregroundStyle(Theme.gold)
                .font(Theme.Font.body(12))
                .help("FileID is sure about this one")
                .accessibilityLabel("High confidence")
        case "review":
            Image(systemName: "questionmark.circle")
                .foregroundStyle(Theme.warning)
                .font(Theme.Font.body(12))
                .help("Fairly sure; worth a look")
                .accessibilityLabel("Medium confidence — review")
        case "ask":
            Image(systemName: "hand.raised")
                .foregroundStyle(Theme.text3)
                .font(Theme.Font.body(12))
                .help("Not sure; check before moving")
                .accessibilityLabel("Low confidence — ask")
        default:
            EmptyView()
        }
    }

    // MARK: - Engine-routed plan + apply

    /// Ask the engine butler for a fresh restructure plan. The reply arrives
    /// asynchronously on `engine.restructurePlan`; `applyPlan()` (driven by
    /// `engine.restructurePlanSignal`) maps it onto the view's proposal model.
    private func requestPlan() {
        guard let root = libraryRoot else { return }
        // Capture per-file deselections BEFORE the new plan lands so a re-plan
        // (e.g. after Deep Analyze finishes mid-review) doesn't re-check rows
        // the user unchecked. Re-applied in applyPlan().
        priorDeselectedIDs = Set(proposals.map(\.fileID)).subtracting(selectedIDs)
        storedPlanID = nil
        storedPlanMoveCount = 0
        storedPlanConfidenceCounts = nil
        loading = true
        status = nil
        if !engine.planRestructure(libraryRoot: root.path) {
            loading = false
            status = "FileID is still getting ready. Open this page again in a moment to see your plan."
            statusIsError = true
        }
    }

    /// Map the engine's authoritative plan onto the view's `Proposal`/summary
    /// model, then rebuild the grouping/selection/inline caches the Sankey,
    /// TreeDiff, recommendation rows, and drill-down render from.
    private func applyPlan() {
        guard let plan = engine.restructurePlan, let root = libraryRoot else { return }
        // Ignore a plan computed for a different destination root (the user may
        // have switched folders while a plan was in flight).
        guard Self.pathsMatch(plan.libraryRoot, root.path) else { return }

        if plan.truncated {
            guard let planID = plan.planID,
                  let totalMoves = plan.totalMoves, totalMoves > 0 else {
                loading = false
                status = "The saved plan couldn't be opened. Press Suggest a layout to make a new one."
                statusIsError = true
                return
            }
            storedPlanID = planID
            storedPlanMoveCount = totalMoves
            storedPlanConfidenceCounts = plan.confidenceCounts
            proposals = []
            groups = []
            selectedIDs = []
            summary = .empty
            inlineGroupsByOutcome = [:]
            inlineMatchedCountByOutcome = [:]
            loading = false
            status = nil
            statusIsError = false
            return
        }
        storedPlanID = nil
        storedPlanMoveCount = 0
        storedPlanConfidenceCounts = nil

        let mapped = Self.mapProposals(from: plan)
        proposals = mapped
        summary = Self.makeSummary(from: plan, proposals: mapped, libraryFiles: store.totalFiles)

        let by = Dictionary(grouping: mapped, by: { $0.bucket })
        groups = by.map { Group(bucket: $0.key, proposals: $0.value) }
            .sorted { a, b in a.proposals.count != b.proposals.count ? a.proposals.count > b.proposals.count : a.bucket < b.bucket }

        selectedIDs = Set(mapped.map(\.fileID))
        // Ask-confidence moves start deselected — "No clear signal, the decision
        // is yours." The user must explicitly check them before applying.
        // (RESTRUCTURE.md §6 — confidence-tier autonomy)
        for p in mapped where p.confidence.lowercased() == "ask" {
            selectedIDs.remove(p.fileID)
        }
        // Re-apply per-file deselections that carried into the fresh set.
        selectedIDs.subtract(priorDeselectedIDs)
        // Preserve the user's "Skip these" choices: re-exclude any persisted
        // skipped outcome's proposals (otherwise skipped rows render dimmed but
        // get re-applied).
        for outcome in skippedOutcomes {
            for p in mapped where outcomeFor(p) == outcome {
                selectedIDs.remove(p.fileID)
            }
        }

        // Per-outcome groupings the recommendation row's expand-in-place file
        // list reads from. Built here so the render path returns cached values
        // without re-running filter/groupBy.
        let totalCap = 30
        let bucketCap = 4
        var byOutcome: [RestructureOutcome: [Group]] = [:]
        var matchedCount: [RestructureOutcome: Int] = [:]
        for outcome in [RestructureOutcome.tidy, .reorganize] {
            let matched = mapped.filter { outcomeFor($0) == outcome }
            matchedCount[outcome] = matched.count
            let buckets = Dictionary(grouping: matched, by: \.bucket)
            let order = buckets.keys.sorted {
                (buckets[$0]?.count ?? 0) > (buckets[$1]?.count ?? 0)
            }
            byOutcome[outcome] = order.prefix(bucketCap).map { bucket in
                Group(
                    bucket: bucket,
                    proposals: Array((buckets[bucket] ?? []).prefix(totalCap))
                )
            }
        }
        inlineGroupsByOutcome = byOutcome
        inlineMatchedCountByOutcome = matchedCount

        let (total, captioned) = store.filesAnalysisStats()
        totalAnalyzableFiles = total
        captionedFraction = total > 0 ? Double(captioned) / Double(total) : 0
        embeddingCounts = store.contentEmbeddingCounts()

        loading = false
    }

    /// Apply the selected, non-skipped moves through the engine butler.
    /// Single-flight: `applying` gates the apply-bar buttons and is
    /// cleared when `restructureApplyResult` arrives (or the engine resets).
    /// Selected ∧ not-skipped moves from the CURRENT
    /// plan/selection. Snapshotted into `pendingMoves` at dialog-present time so a
    /// background re-plan can't change the set the confirm action applies.
    /// Belt-and-suspenders: filter the engine plan's own moves so a skipped
    /// outcome never moves.
    private func eligibleMoves() -> [RestructureMove] {
        guard let plan = engine.restructurePlan else { return [] }
        let eligible = Set(
            proposals
                .filter { selectedIDs.contains($0.fileID) && !skippedOutcomes.contains(outcomeFor($0)) }
                .map(\.fileID))
        return plan.moves.filter { eligible.contains($0.fileID) }
    }

    private func applyViaEngine() {
        guard !applying else { return }
        guard let root = libraryRoot else {
            status = "Choose a folder first."
            statusIsError = true
            return
        }
        // Apply exactly the set frozen when the dialog was presented, not
        // a set a mid-dialog re-plan may have rewritten.
        let moves = pendingMoves
        guard pendingPlanID != nil || !moves.isEmpty else {
            status = "Nothing selected to apply."
            statusIsError = true
            return
        }
        applying = true
        status = nil
        if !engine.applyRestructure(
            libraryRoot: root.path, moves: moves, planID: pendingPlanID)
        {
            applying = false
            status = "FileID is still getting ready. Try again in a moment."
            statusIsError = true
        }
    }

    // MARK: - Engine plan → view-model mapping

    /// Build the view's proposal model from the engine plan. The display
    /// `bucket` is the destination's parent directory relative to the library
    /// root (e.g. "Photos/2023/January", "People/Marie Curie/2023") so the
    /// Sankey/TreeDiff bucket icons + grouping match the prior app-side output;
    /// `kind` comes from the engine's per-move folder tier when present.
    static func mapProposals(from plan: RestructurePlan) -> [Proposal] {
        plan.moves.map { m in
            Proposal(
                fileID: m.fileID,
                oldPath: m.source,
                newPath: m.destination,
                bucket: bucketLabel(destination: m.destination,
                                    root: plan.libraryRoot, fallback: m.category),
                sourceFolder: (m.source as NSString).deletingLastPathComponent,
                filename: URL(fileURLWithPath: m.source).lastPathComponent,
                sourceName: ((m.source as NSString).deletingLastPathComponent as NSString).lastPathComponent,
                kind: kind(forTier: m.tier),
                confidence: m.confidence,
                reason: m.reason)
        }
    }

    /// Derive the at-a-glance summary from the plan. Folder tiers come from the
    /// engine's `folderClassifications` when present; otherwise (current macOS
    /// engine) every received move is a relocation — the engine strips
    /// anchor-folder moves before sending — so we count distinct source folders
    /// per outcome. That is aggregation of the engine's moves, NOT a re-run of
    /// the retired app-side classifier.
    static func makeSummary(from plan: RestructurePlan, proposals: [Proposal], libraryFiles: Int = 0) -> AssistantSummary {
        let movedOut = proposals.filter { $0.kind == .movedOutAsOutlier }
        let dissolved = proposals.filter { $0.kind == .dissolved }
        let anchorFolders: Int
        let mixedFolders: Int
        let junkFolders: Int
        if let fc = plan.folderClassifications {
            anchorFolders = fc.anchorFolders
            mixedFolders = fc.mixedFolders
            junkFolders = fc.junkFolders
        } else {
            anchorFolders = 0
            mixedFolders = Set(movedOut.map(\.sourceFolder)).count
            junkFolders = Set(dissolved.map(\.sourceFolder)).count
        }
        return AssistantSummary(
            anchorFolders: anchorFolders,
            mixedFolders: mixedFolders,
            junkFolders: junkFolders,
            // The plan carries only moves, so everything else in the library stays.
            staysPutFiles: max(0, libraryFiles - proposals.count),
            movedOutFiles: movedOut.count,
            dissolvedFiles: dissolved.count,
            staysPutBreakdown: [])
    }

    /// "Mixed" folders contribute outliers (Tidy); "Junk"/unknown/absent
    /// dissolve (Reorganize). The current macOS engine sends `nil` → Reorganize.
    static func kind(forTier tier: String?) -> ProposalKind {
        tier?.lowercased() == "mixed" ? .movedOutAsOutlier : .dissolved
    }

    /// Destination's parent directory relative to the library root, or the
    /// engine's category label when the destination isn't under the root.
    static func bucketLabel(destination: String, root: String, fallback: String) -> String {
        let parent = (destination as NSString).deletingLastPathComponent
        let rootNorm = root.hasSuffix("/") ? String(root.dropLast()) : root
        if parent == rootNorm { return fallback }
        let prefix = rootNorm + "/"
        if parent.hasPrefix(prefix) {
            let rel = String(parent.dropFirst(prefix.count))
            return rel.isEmpty ? fallback : rel
        }
        return fallback
    }

    static func pathsMatch(_ a: String, _ b: String) -> Bool {
        if a == b { return true }
        return URL(fileURLWithPath: a).standardizedFileURL.path
            == URL(fileURLWithPath: b).standardizedFileURL.path
    }

    // MARK: - View-mode toggle + Sankey + recommendations

    private var sankeyHeaderStat: String {
        let srcCount = Set(proposals.map(\.sourceFolder)).count
        let dstCount = Set(proposals.map(\.bucket)).count
        return "\(srcCount) folder\(srcCount == 1 ? "" : "s") → \(dstCount) folder\(dstCount == 1 ? "" : "s")"
    }

    private func toggleExpand(_ outcome: RestructureOutcome) {
        withAnimation(Theme.springStandard) {
            if expandedOutcome == outcome {
                expandedOutcome = nil
            } else {
                expandedOutcome = outcome
            }
        }
    }

    /// Inline file-list view for a recommendation card. Reads from
    /// the precomputed `inlineGroupsByOutcome` cache populated in
    /// `applyPlan()` — no `proposals.filter` or `Dictionary(grouping:)`
    /// runs on the render path. The cache covers `.outcome(...)`
    /// scopes, which are the only scopes inline lists ever receive.
    @ViewBuilder
    private func inlineFileList(for scope: DrillDownScope) -> some View {
        if case .outcome(let outcome) = scope,
           let cachedGroups = inlineGroupsByOutcome[outcome] {
            let matchedCount = inlineMatchedCountByOutcome[outcome] ?? 0
            let totalCap = 30
            VStack(alignment: .leading, spacing: 10) {
                ForEach(Array(cachedGroups.enumerated()), id: \.element.id) { idx, g in
                    if idx > 0 { Hairline(color: Theme.hairlineSoft) }
                    bucketSection(g)
                }
                if matchedCount > totalCap {
                    HStack {
                        Spacer()
                        Button { drillDown = scope } label: {
                            Label("See all \(matchedCount.formatted()) files", systemImage: "arrow.up.right.square")
                        }
                        .buttonStyle(.dsGhost)
                    }
                }
            }
        } else {
            EmptyView()
        }
    }

    private func toggleSkip(_ outcome: RestructureOutcome) {
        if skippedOutcomes.contains(outcome) {
            skippedOutcomes.remove(outcome)
            // Re-include this outcome's proposals in the selection.
            for p in proposals where outcomeFor(p) == outcome {
                selectedIDs.insert(p.fileID)
            }
        } else {
            skippedOutcomes.insert(outcome)
            // Drop this outcome's proposals from the selection so the
            // Apply button count + the actual apply pass match.
            for p in proposals where outcomeFor(p) == outcome {
                selectedIDs.remove(p.fileID)
            }
        }
    }

    private func outcomeFor(_ p: Proposal) -> RestructureOutcome {
        switch p.kind {
        case .dissolved:        return .reorganize
        case .movedOutAsOutlier: return .tidy
        }
    }

    // MARK: - Drill-down sheet

    @ViewBuilder
    private func drillDownSheet(_ scope: DrillDownScope) -> some View {
        // Read the grouping cached in `drillDownGroups` (rebuilt only when
        // the scope changes) so a per-row checkbox tap doesn't re-filter +
        // re-group the whole proposal set on every render.
        let groups = drillDownGroups
        let matchedCount = groups.reduce(0) { $0 + $1.proposals.count }
        let title = drillDownTitle(scope, count: matchedCount)
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    SectionLabel("Organize Folders / Review")
                    Text(title).font(Theme.Font.display(28)).foregroundStyle(Theme.text1).lineLimit(1)
                    MetaText("\(matchedCount.formatted()) file\(matchedCount == 1 ? "" : "s"). Click a row, or press Space on it, to include or skip it.")
                }
                Spacer()
                Button("Done") { drillDown = nil }
                    .buttonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.controlLarge))
                    .keyboardShortcut(.defaultAction)
            }
            .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.top, 20).padding(.bottom, 14)
            Hairline()
            if groups.isEmpty {
                DashedNotice(title: "Nothing to show")
                    .padding(Theme.Metrics.pageInsetX)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(Array(groups.enumerated()), id: \.element.id) { idx, g in
                            if idx > 0 { Hairline(color: Theme.hairlineSoft) }
                            bucketSection(g, unlimited: true)
                        }
                    }
                    .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, 16)
                }
            }
        }
        .frame(minWidth: 600, minHeight: 480)
        .background(LavaLampBackground())
        .preferredColorScheme(.dark)
    }

    /// Recompute the filtered + bucketed proposals for a drill-down scope.
    /// Called only when the scope changes (sheet open/close/switch), so the
    /// O(N) filter + groupBy doesn't run on every checkbox tap.
    private func rebuildDrillDownGroups(for scope: DrillDownScope?) {
        guard let scope else { drillDownGroups = []; return }
        let matched = proposals.filter { matches(scope, $0) }
        let byBucket = Dictionary(grouping: matched, by: { $0.bucket })
        drillDownGroups = byBucket.keys
            .sorted { (byBucket[$0]?.count ?? 0) > (byBucket[$1]?.count ?? 0) }
            .map { Group(bucket: $0, proposals: byBucket[$0] ?? []) }
    }

    private func matches(_ scope: DrillDownScope, _ p: Proposal) -> Bool {
        switch scope {
        case .all:                       return true
        case .outcome(let o):            return outcomeFor(p) == o
        case .sourceFolder(let f):       return p.sourceFolder == f
        case .destBucket(let b):         return p.bucket == b
        case .sourceFolders(let fs):     return fs.contains(p.sourceFolder)
        case .destBuckets(let bs):       return bs.contains(p.bucket)
        }
    }

    private func drillDownTitle(_ scope: DrillDownScope, count: Int) -> String {
        switch scope {
        case .all:
            return "All proposed moves"
        case .outcome(.tidy):
            return "Tidying — files moving out of mixed folders"
        case .outcome(.reorganize):
            return "Reorganizing — files leaving generic folders"
        case .outcome(.keep):
            return "Folders staying put"
        case .sourceFolder(let f):
            return "From \((f as NSString).lastPathComponent)"
        case .destBucket(let b):
            return "Going to \(b)"
        case .sourceFolders(let fs):
            return "From \(fs.count) smaller folder\(fs.count == 1 ? "" : "s")"
        case .destBuckets(let bs):
            return "Going to \(bs.count) smaller folder\(bs.count == 1 ? "" : "s")"
        }
    }
}
