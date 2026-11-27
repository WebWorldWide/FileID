// Restructure tab. Folders are classed Anchor (kept), Mixed (outliers move out) or Junk (dissolved); see shared/docs/RESTRUCTURE.md.
// Plans and applies go through the engine butler (`planRestructure` / `applyRestructure`).
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
    @State private var moveOutcome: MoveOutcome?
    @State private var undoing = false
    @State private var confirmApply: Bool = false
    @State private var confirmLargeApply = false
    @State private var storedPlanFolders: [RestructureCategoryCount] = []
    @State private var planBuild: Task<Void, Never>?
    @State private var planVersion = 0
    /// `store.version` when the plan on screen was asked for.
    @State private var planStoreVersion = 0
    @State private var planDelivered = false
    @State private var sankeyHeaderStat = ""
    /// Single-flight guard for apply: a double-click must never run it twice, so the bar buttons disable while set.
    @State private var applying = false
    @State private var cancelRequested = false
    /// Moves frozen when the confirmation dialog opens. A background re-plan can rewrite the
    /// proposals while it is open; confirming must apply exactly what the user reviewed.
    @State private var pendingMoves: [RestructureMove] = []
    @State private var pendingPlanID: String?
    @State private var pendingApplyCount = 0
    @State private var storedPlanID: String?
    @State private var storedPlanMoveCount = 0
    /// Full-plan confidence tallies for the stored plan. Only the auto tier applies (see
    /// `applyStoredPlan`); nil until a truncated plan lands.
    @State private var storedPlanConfidenceCounts: RestructureConfidenceCounts?

    @AppStorage("restructure.viewMode") private var viewModeRaw: String = ViewMode.cards.rawValue
    private var viewMode: ViewMode { ViewMode(rawValue: viewModeRaw) ?? .cards }
    enum ViewMode: String { case cards, tree }

    @State private var skippedOutcomes: Set<RestructureOutcome> = []
    @State private var expandedOutcome: RestructureOutcome?
    @State private var drillDown: DrillDownScope?
    /// Filtered and grouped proposals for the drill-down scope, rebuilt only when the scope changes.
    @State private var drillDownGroups: [Group] = []

    /// Hover state shared by the Sankey, recommendation rows and tree.
    @State private var hoverBus = RestructureHoverBus()
    @State private var captionedFraction: Double = 0
    @State private var totalAnalyzableFiles = 0
    @AppStorage("restructure.dismissedDeepAnalyzeHint") private var dismissedDeepAnalyzeHint = false
    /// (clip, text) embedding counts at plan time; nil until measured. Both 0 means the plan
    /// is date/name-only, and the user is told to install the models and rescan.
    @State private var embeddingCounts: (clip: Int, text: Int)?
    /// Lets the missing-models banner jump the user to Settings → AI Models.
    @AppStorage("activeTabRawValue") private var activeTabRaw: String = MainWindow.Tab.library.rawValue
    /// Deselections captured at `requestPlan()` and re-applied in `applyPlan()`, so a re-plan does not re-check rows the user unchecked.
    @State private var priorDeselectedIDs: Set<Int64> = []

    /// Which subset of proposals the drill-down sheet renders.
    enum DrillDownScope: Identifiable, Hashable {
        case all
        case outcome(RestructureOutcome)
        case sourceFolder(String)
        case destBucket(String)
        /// The long-tail source folders collapsed into the Sankey's "+ N more folders" node.
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

    struct MoveOutcome {
        let title: String
        let detail: String
        let warning: Bool
    }

    struct PlanModel: Sendable {
        let proposals: [Proposal]
        let summary: AssistantSummary
        let groups: [Group]
        let selectedIDs: Set<Int64>
        let inlineGroups: [RestructureOutcome: [Group]]
        let inlineMatched: [RestructureOutcome: Int]
        let headerStat: String
        let analyzable: Int
        let captioned: Int
        let embeddings: (clip: Int, text: Int)
    }

    struct Proposal: Identifiable, Hashable, Sendable {
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
        // Butler confidence band ("auto"/"review"/"ask", "" when unstamped) and the engine's "why filed here": the main trust signal.
        var confidence: String = ""
        var reason: String? = nil
    }

    struct Group: Identifiable, Sendable {
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

        static let empty = AssistantSummary(
            anchorFolders: 0, mixedFolders: 0, junkFolders: 0,
            staysPutFiles: 0, movedOutFiles: 0, dissolvedFiles: 0
        )

        var hasContent: Bool {
            anchorFolders + mixedFolders + junkFolders > 0
        }
    }

    var body: some View {
        // Step cells stay pinned above the scrolling plan so Move is visible as soon as a plan lands.
        VStack(alignment: .leading, spacing: 0) {
            if libraryRoot != nil {
                stepCells
                    .padding(.horizontal, Theme.Metrics.pageInsetX)
                    .padding(.top, 16).padding(.bottom, 12)
                    .transition(.opacity)
                    // The butler performs real on-disk moves, so the dialog states that before anything moves.
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
                    .confirmationDialog(
                        "Move \(pendingApplyCount.formatted()) file\(pendingApplyCount == 1 ? "" : "s") without checking each one?",
                        isPresented: $confirmLargeApply,
                        titleVisibility: .visible
                    ) {
                        Button("Move without checking each file", role: .destructive) {
                            applyViaEngine()
                        }
                        Button("Cancel", role: .cancel) { }
                    } message: {
                        Text(largeApplyMessage)
                    }
                Hairline()
            }
            ScrollView(.vertical) {
                VStack(alignment: .leading, spacing: 16) {
                    header
                    if let outcome = moveOutcome {
                        moveOutcomeBanner(outcome)
                    }
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
            planBuild?.cancel()
            proposals = []
            groups = []
            selectedIDs = []
            summary = .empty
            sankeyHeaderStat = ""
            planVersion &+= 1
            moveOutcome = nil
            if selectedRoot != nil { requestPlan() }
        }
        // Re-plan only on .deepAnalyzeComplete: proposeAll walks the whole library, so even a 3-s per-file throttle caused up to 50K re-plans in a long batch.
        .onChangeWhileActive(of: engine.deepAnalyzeComplete?.processed ?? -1) {
            guard libraryRoot != nil else { return }
            store.notifyChanged()
            requestPlan()
        }
        .onChange(of: engine.restructurePlanSignal) { _, _ in
            applyPlan()
        }
        // Coming back plans again only when files changed or the last plan never arrived.
        .onPageVisibility(shown: {
            guard libraryRoot != nil, !loading, !applying, !confirmApply, !confirmLargeApply else { return }
            if Self.needsReplan(storeVersion: store.version, planStoreVersion: planStoreVersion, planDelivered: planDelivered) { requestPlan() }
        })
        // Apply finished: show the result, release the single-flight guard, and re-plan (moved files have new paths).
        .onChange(of: engine.restructureApplyResultSignal) { _, _ in
            guard libraryRoot != nil, let result = engine.restructureApplyResult else { return }
            applying = false
            let wasUndo = undoing
            undoing = false
            store.notifyChanged()
            requestPlan()
            // requestPlan clears `status`, so the result is shown in its own banner.
            let n = result.applied
            let files = "\(n.formatted()) file\(n == 1 ? "" : "s")"
            let failed = result.failed
            if let priv = result.privilegeError, !priv.isEmpty {
                moveOutcome = MoveOutcome(title: "The move couldn’t start", detail: priv, warning: true)
            } else if wasUndo {
                moveOutcome = failed > 0 || result.cancelled
                    ? MoveOutcome(title: "Put back \(files), \(failed.formatted()) couldn’t be put back",
                                  detail: "The rest are still in their new folders. Press Undo move to try again.", warning: true)
                    : MoveOutcome(title: "Put back \(files)", detail: "Every file is back where it was.", warning: false)
            } else if result.cancelled {
                moveOutcome = MoveOutcome(title: "Stopped after moving \(files)",
                                          detail: "The rest stayed where they were. Undo move puts the moved files back.", warning: false)
            } else if failed > 0 {
                moveOutcome = MoveOutcome(
                    title: "Moved \(files), \(failed.formatted()) couldn’t be moved",
                    detail: "The \(failed.formatted()) file\(failed == 1 ? "" : "s") that didn’t move \(failed == 1 ? "is" : "are") still where \(failed == 1 ? "it was" : "they were"), usually because a file changed, went missing, or its drive was disconnected. FileID doesn’t get a list of which ones; any that still need a new home show up in the plan again.",
                    warning: true)
            } else {
                moveOutcome = MoveOutcome(title: "Moved \(files)", detail: "Undo move puts every file back where it was.", warning: false)
            }
        }
        // The engine died before a terminal event arrived; release the in-flight UI so the tab does not wedge.
        .onChange(of: engine.engineResetSignal) { _, _ in
            if applying {
                applying = false
                undoing = false
                status = "FileID restarted in the middle of the move. Files already moved can be put back with Undo move; try again for the rest."
                statusIsError = true
            }
            loading = false
        }
        // A failed plan ends the "Computing…" spinner. Keyed on `lastErrorSignal`, not the message,
        // so a retry failing with the same message still clears it. A plan frame dropped by the
        // inbound IPC cap (`ipc_frame_too_large`) is also a terminal failure.
        .onChange(of: engine.lastErrorSignal) { _, _ in
            guard let kind = engine.lastError?.kind else { return }
            // An apply or undo rejected with a bare error (e.g. db_unavailable) gets no apply result
            // and no reset event, so `applying` would stay set forever; release it here.
            if applying, EngineClient.endsRestructureWithoutResult(kind) {
                applying = false
                undoing = false
                status = kind == "db_unavailable"
                    ? "FileID is still getting ready. Try again in a moment."
                    : "FileID couldn’t finish that. Nothing else moved. Try again in a moment."
                statusIsError = true
            }
            guard loading else { return }
            switch kind {
            case "plan_restructure_failed", "db_unavailable":
                loading = false
                status = "FileID couldn’t work out a plan. Try again."
                statusIsError = true
            case "plan_restructure_root":
                loading = false
                status = "This folder isn’t available. Reconnect it, then try again."
                statusIsError = true
            case "plan_restructure_cancelled":
                loading = false
                status = "Stopped working out the plan."
                statusIsError = false
            case let other where other.hasPrefix("plan_restructure_"):
                loading = false
                status = "FileID couldn’t work out a plan. Try again."
                statusIsError = true
            case "ipc_frame_too_large":
                loading = false
                status = "This folder has too many files to show a plan. Try a smaller folder."
                statusIsError = true
            default:
                break
            }
        }
        // Grouped once per scope, not on every checkbox tap in the sheet.
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
        guard !applying, readOnlyReason == nil else { return }
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
        if storedPlanMoveCount > 0 {
            guard !storedPlanFolders.isEmpty else { return }
            confirmLargeApply = true
        } else {
            confirmApply = true
        }
    }

    static func readOnlyMoveNote(reason: String, moveCount: Int) -> String {
        "\(reason) The \(Plural.noun(moveCount, "move", "moves")) in this plan can’t run. You can look at the plan, but Move is off."
    }

    private var readOnlyReason: String? {
        guard let root = libraryRoot else { return nil }
        return engine.readOnlyGate.reasonIfAll([root.path])
    }

    private var largeApplyBlocked: Bool { storedPlanMoveCount > 0 && storedPlanFolders.isEmpty }

    private var largeApplyMessage: String {
        let shown = storedPlanFolders.prefix(8)
            .map { "\($0.category) (\($0.count.formatted()))" }
            .joined(separator: ", ")
        let more = storedPlanFolders.count - min(8, storedPlanFolders.count)
        let tail = more > 0 ? ", and \(more.formatted()) more folder\(more == 1 ? "" : "s")" : ""
        return "This plan is too large to list file by file. It sorts files into: \(shown)\(tail). Only the moves FileID is sure about happen now; the rest stay put. Nothing is deleted, and Undo move puts every file back."
    }

    /// An Organize waits to be undone: this session's own apply, or one a relaunch found in the engine's journal for this folder.
    private var undoAvailable: Bool {
        guard engine.canUndoRestructure else { return false }
        guard let pending = engine.restructureUndoRoot, let root = libraryRoot else { return true }
        return Self.sameFolder(pending, root.path)
    }

    nonisolated static func sameFolder(_ left: String, _ right: String) -> Bool {
        URL(fileURLWithPath: left).standardizedFileURL.resolvingSymlinksInPath().path
            == URL(fileURLWithPath: right).standardizedFileURL.resolvingSymlinksInPath().path
    }

    private func undoLastMove() {
        guard let root = libraryRoot, !applying else { return }
        applying = true
        undoing = true
        moveOutcome = nil
        status = "Putting the moved files back…"
        statusIsError = false
        // A failed send never gets a result event, so release the guard here.
        if !engine.undoRestructure(libraryRoot: root.path) {
            applying = false
            undoing = false
            status = HelperGate.unsentNote(engine)
            statusIsError = true
        }
    }

    private var stepCells: some View {
        let moved = undoAvailable
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
                        .padding(.leading, planReady ? -6 : 0)
                }
            }
            VHairline()
            RestructureStepCell(number: 2, title: "Review", state: reviewState,
                                detail: !planReady ? "Check each group of moves and skip any you don’t want."
                                    : (storedPlanMoveCount > 0
                                        ? "Too many moves to list one by one. Only the \(applyCount.formatted()) FileID is sure about will move; the rest stay put."
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
                            .disabled(largeApplyBlocked || readOnlyReason != nil)
                            .help(readOnlyReason ?? (largeApplyBlocked
                                  ? "This plan is too large to show, so it can’t be checked. Choose a smaller folder."
                                  : "Move the chosen files into their new folders. You confirm first, and you can undo."))
                            .accessibilityHint(readOnlyReason ?? "")
                        if largeApplyBlocked {
                            MetaText("Too large to check. Choose a smaller folder.", color: Theme.warning, size: 11.5)
                        }
                        if let readOnlyReason {
                            MetaText(Self.readOnlyMoveNote(reason: readOnlyReason, moveCount: applyCount), size: 11.5)
                        }
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
               title: clipModelInstalled ? "Rescan so FileID can see what’s in your photos" : "Sorting by date only",
               body: clipModelInstalled
                ? "This folder was scanned before the Search by description model was downloaded. A rescan looks at the photos again; files already up to date are skipped."
                : "Without the Search by description and Documents models, FileID can’t see what files contain, so loose files go into year and month folders. Download them in Settings, then rescan.") {
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
                : "\(captioned.formatted()) of \(Plural.noun(totalAnalyzableFiles, "file", "files")) are described (\(pct)%). Describing the rest gives the plan more to work with.") {
            HStack(spacing: 6) {
                Button("Not now") { dismissedDeepAnalyzeHint = true }
                    .buttonStyle(.dsGhost)
                Button {
                    let kind = DeepAnalyzeSettings.shared.activeKind
                    guard ModelLicenseGate.ensureAccepted(for: kind) else { return }
                    engine.deepAnalyzeAll(modelKind: kind.rawValue, skipExisting: true)
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
                        planVersion: planVersion,
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
                        bodyText: "Mostly organized folders with a few files that don’t fit. The folder stays; the misplaced files move to where they belong.",
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
                        bodyText: "Folders with generic names like “Untitled” or “Camera Roll”. Their files are sorted into People, Places, Documents, events or Photos by year.",
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
        notice(tone: isError ? Theme.warning : Theme.delight, title: isError ? "Something went wrong" : "Organize Folders", body: message) { EmptyView() }
    }

    private func moveOutcomeBanner(_ outcome: MoveOutcome) -> some View {
        notice(tone: outcome.warning ? Theme.warning : Theme.delight, title: outcome.title, body: outcome.detail) {
            Button("Dismiss") { moveOutcome = nil }
                .buttonStyle(.dsGhost)
        }
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
            if undoAvailable && proposals.isEmpty && !applying && moveOutcome == nil {
                Text("Your last Organize can still be undone.")
                    .font(Theme.Font.body(13)).foregroundStyle(Theme.text2)
            }
        }
    }

    nonisolated static func nothingToMoveBody(rootName: String?) -> String {
        let inside = rootName.map { "inside “\($0)”" } ?? "in it"
        return "Your folders \(inside) are already well organized. FileID doesn’t rename the folder you chose."
    }

    private var nothingToMoveCard: some View {
        notice(tone: Theme.delight, title: "Nothing to move",
               body: Self.nothingToMoveBody(rootName: libraryRoot?.lastPathComponent)) { EmptyView() }
    }

    @ViewBuilder
    private var largePlanCard: some View {
        let sure = storedPlanConfidenceCounts?.auto ?? storedPlanMoveCount
        let heldBack = max(0, storedPlanMoveCount - sure)
        let scope = "FileID found \(Plural.noun(storedPlanMoveCount, "file", "files")) to move, too many to list one by one."
        if largeApplyBlocked {
            notice(tone: Theme.warning, title: "This plan is too large to check",
                   body: "\(scope) Choose a smaller folder, such as one year or one project, so you can see every move before it happens.") { EmptyView() }
        } else {
            notice(tone: Theme.gold, title: "A large plan",
                   body: "\(scope) Move takes \(Plural.noun(sure, "file", "files")) that FileID is sure about\(heldBack > 0 ? " and leaves \(Plural.noun(heldBack, "other file", "other files")) where \(heldBack == 1 ? "it is" : "they are")" : ""). To check every move first, choose a smaller folder.") { EmptyView() }
            Panel {
                PanelHeader("Where the files would go") {
                    MetaText("\(storedPlanFolders.count.formatted()) folder\(storedPlanFolders.count == 1 ? "" : "s") · counts cover the whole plan", size: 12)
                }
                VStack(spacing: 0) {
                    ForEach(Array(storedPlanFolders.prefix(40).enumerated()), id: \.offset) { _, entry in
                        HStack(spacing: 10) {
                            Image(systemName: bucketIconName(entry.category))
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(Theme.gold)
                                .frame(width: 20)
                                .accessibilityHidden(true)
                            Text(entry.category).font(Theme.Font.mono(12)).foregroundStyle(Theme.text1)
                                .lineLimit(1).truncationMode(.middle)
                            Spacer()
                            Text("\(entry.count.formatted()) file\(entry.count == 1 ? "" : "s")")
                                .font(Theme.Font.body(11.5)).monospacedDigit().foregroundStyle(Theme.text3)
                        }
                        .padding(.horizontal, 14).padding(.vertical, 6)
                        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
                        .accessibilityElement(children: .combine)
                    }
                    if storedPlanFolders.count > 40 {
                        MetaText("+ \((storedPlanFolders.count - 40).formatted()) more folders", size: 11.5)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, 14).padding(.vertical, 8)
                    }
                }
            }
        }
    }

    private var staysPutSection: some View {
        Panel {
            HStack(spacing: 10) {
                Circle().fill(Theme.delight).frame(width: 7, height: 7).accessibilityHidden(true)
                SectionLabel("Staying put")
                MetaText("\(summary.staysPutFiles.formatted()) file\(summary.staysPutFiles == 1 ? "" : "s")\(summary.anchorFolders > 0 ? " in \(summary.anchorFolders.formatted()) well-named folder\(summary.anchorFolders == 1 ? "" : "s")" : "") stay exactly where they are.", size: 12)
                Spacer()
            }
            .padding(.horizontal, 14)
            .frame(height: 38)
            .accessibilityElement(children: .combine)
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
                            .lineLimit(3).truncationMode(.tail)
                            .multilineTextAlignment(.leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .help(reason)
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

    /// Confidence badge for a proposed move. Shown only when the engine stamped a band; rule-cascade moves get none.
    @ViewBuilder
    private func confidenceBadge(_ confidence: String) -> some View {
        switch confidence.lowercased() {
        case "auto":
            confidenceLabel("Sure", icon: "checkmark.seal.fill", tint: Theme.gold,
                            help: "FileID is sure about this one")
        case "review":
            confidenceLabel("Check", icon: "questionmark.circle", tint: Theme.warning,
                            help: "Fairly sure; worth a look")
        case "ask":
            confidenceLabel("Ask you", icon: "hand.raised", tint: Theme.text3,
                            help: "Not sure, so it starts unchecked. Tick it if the new folder is right.")
        default:
            EmptyView()
        }
    }

    private func confidenceLabel(_ word: String, icon: String, tint: Color, help: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: icon).accessibilityHidden(true)
            Text(word)
        }
        .font(Theme.Font.body(11.5, .medium))
        .foregroundStyle(tint)
        .fixedSize()
        .help(help)
        .accessibilityLabel(help)
    }

    // MARK: - Engine-routed plan + apply

    /// Asks the engine for a fresh plan. The reply lands on `engine.restructurePlan` and `applyPlan()` maps it.
    private func requestPlan() {
        guard let root = libraryRoot else { return }
        priorDeselectedIDs = Set(proposals.map(\.fileID)).subtracting(selectedIDs)
        storedPlanID = nil
        storedPlanMoveCount = 0
        storedPlanConfidenceCounts = nil
        storedPlanFolders = []
        loading = true
        status = nil
        planStoreVersion = store.version
        planDelivered = false
        if !engine.planRestructure(libraryRoot: root.path) {
            loading = false
            status = HelperGate.isDown(engine) ? HelperGate.stoppedNote : "FileID is still getting ready. Open this page again in a moment to see your plan."
            statusIsError = true
        }
    }

    /// Maps the engine plan onto the view's proposals and summary, and rebuilds the grouping, selection and inline caches the map, tree, rows and drill-down render from.
    private func applyPlan() {
        guard let plan = engine.restructurePlan, let root = libraryRoot else { return }
        // Ignore a plan for another root: the user may have switched folders mid-plan.
        guard Self.pathsMatch(plan.libraryRoot, root.path) else { return }

        if plan.truncated {
            guard let planID = plan.planID,
                  let totalMoves = plan.totalMoves, totalMoves > 0 else {
                loading = false
                status = "The saved plan couldn’t be opened. Press Suggest a layout to make a new one."
                statusIsError = true
                return
            }
            storedPlanID = planID
            storedPlanMoveCount = totalMoves
            storedPlanConfidenceCounts = plan.confidenceCounts
            storedPlanFolders = plan.categoryCounts.filter { $0.count > 0 }
            planBuild?.cancel()
            proposals = []
            groups = []
            selectedIDs = []
            summary = .empty
            inlineGroupsByOutcome = [:]
            inlineMatchedCountByOutcome = [:]
            sankeyHeaderStat = ""
            planVersion &+= 1
            loading = false
            planDelivered = true
            status = nil
            statusIsError = false
            return
        }
        storedPlanID = nil
        storedPlanMoveCount = 0
        storedPlanConfidenceCounts = nil
        storedPlanFolders = []

        // Plans reach 100k moves, so mapping, grouping and the catalog reads run off the main actor.
        let deselected = priorDeselectedIDs
        let skipped = skippedOutcomes
        let store = store
        planBuild?.cancel()
        planBuild = Task {
            let model = await Task.detached(priority: .userInitiated) {
                Self.buildModel(from: plan, libraryFiles: store.countFiles(under: plan.libraryRoot),
                                deselected: deselected, skipped: skipped, store: store)
            }.value
            guard !Task.isCancelled, let current = libraryRoot,
                  Self.pathsMatch(plan.libraryRoot, current.path) else { return }
            proposals = model.proposals
            summary = model.summary
            groups = model.groups
            selectedIDs = model.selectedIDs
            inlineGroupsByOutcome = model.inlineGroups
            inlineMatchedCountByOutcome = model.inlineMatched
            sankeyHeaderStat = model.headerStat
            totalAnalyzableFiles = model.analyzable
            captionedFraction = model.analyzable > 0 ? Double(model.captioned) / Double(model.analyzable) : 0
            embeddingCounts = model.embeddings
            planVersion &+= 1
            loading = false
            planDelivered = true
        }
    }

    /// Coming back to the page re-plans only when files changed or the last plan never arrived: a large plan keeps no proposals on screen, so an empty list says nothing about whether it was delivered.
    nonisolated static func needsReplan(storeVersion: Int, planStoreVersion: Int, planDelivered: Bool) -> Bool {
        storeVersion != planStoreVersion || !planDelivered
    }

    nonisolated static func buildModel(
        from plan: RestructurePlan, libraryFiles: Int, deselected: Set<Int64>,
        skipped: Set<RestructureOutcome>, store: ReadStore
    ) -> PlanModel {
        let mapped = mapProposals(from: plan)
        let summary = makeSummary(from: plan, proposals: mapped, libraryFiles: libraryFiles)

        let by = Dictionary(grouping: mapped, by: { $0.bucket })
        let groups = by.map { Group(bucket: $0.key, proposals: $0.value) }
            .sorted { a, b in a.proposals.count != b.proposals.count ? a.proposals.count > b.proposals.count : a.bucket < b.bucket }

        var selectedIDs = Set(mapped.map(\.fileID))
        // Ask-confidence moves start deselected: the user must check them before applying (RESTRUCTURE.md §6).
        for p in mapped where p.confidence.lowercased() == "ask" {
            selectedIDs.remove(p.fileID)
        }
        selectedIDs.subtract(deselected)
        // Re-exclude skipped outcomes, or their dimmed rows would still be applied.
        for p in mapped where skipped.contains(outcome(for: p)) {
            selectedIDs.remove(p.fileID)
        }

        // Per-outcome groupings for the recommendation rows' inline file lists, cached so rendering does no grouping.
        let totalCap = 30
        let bucketCap = 4
        var byOutcome: [RestructureOutcome: [Group]] = [:]
        var matchedCount: [RestructureOutcome: Int] = [:]
        for outcome in [RestructureOutcome.tidy, .reorganize] {
            let matched = mapped.filter { Self.outcome(for: $0) == outcome }
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
        let srcCount = Set(mapped.map(\.sourceFolder)).count
        let dstCount = by.count
        let (total, captioned) = store.filesAnalysisStats()
        return PlanModel(
            proposals: mapped, summary: summary, groups: groups, selectedIDs: selectedIDs,
            inlineGroups: byOutcome, inlineMatched: matchedCount,
            headerStat: "\(srcCount) folder\(srcCount == 1 ? "" : "s") → \(dstCount) folder\(dstCount == 1 ? "" : "s")",
            analyzable: total, captioned: captioned,
            embeddings: store.contentEmbeddingCounts())
    }

    /// The selected, non-skipped moves from the current plan.
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
        // The set frozen when the dialog opened, not one a re-plan has since rewritten.
        let moves = pendingMoves
        guard pendingPlanID != nil || !moves.isEmpty else {
            status = "Nothing selected to apply."
            statusIsError = true
            return
        }
        applying = true
        undoing = false
        moveOutcome = nil
        status = nil
        if !engine.applyRestructure(
            libraryRoot: root.path, moves: moves, planID: pendingPlanID)
        {
            applying = false
            status = HelperGate.unsentNote(engine)
            statusIsError = true
        }
    }

    // MARK: - Engine plan → view-model mapping

    /// Builds the view's proposal model from the engine plan. `bucket` is the destination's parent directory relative to the library root, so the Sankey and TreeDiff icons and grouping match.
    nonisolated static func mapProposals(from plan: RestructurePlan) -> [Proposal] {
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

    /// The at-a-glance summary. Folder counts come from `folderClassifications`, or from distinct source folders per outcome when the plan has none.
    nonisolated static func makeSummary(from plan: RestructurePlan, proposals: [Proposal], libraryFiles: Int = 0) -> AssistantSummary {
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
            dissolvedFiles: dissolved.count)
    }

    /// "Mixed" folders contribute outliers (Tidy); "Junk"/unknown/absent
    /// dissolve (Reorganize).
    nonisolated static func kind(forTier tier: String?) -> ProposalKind {
        tier?.lowercased() == "mixed" ? .movedOutAsOutlier : .dissolved
    }

    /// Destination's parent directory relative to the library root, or the
    /// engine's category label when the destination isn't under the root.
    nonisolated static func bucketLabel(destination: String, root: String, fallback: String) -> String {
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

    nonisolated static func pathsMatch(_ a: String, _ b: String) -> Bool {
        if a == b { return true }
        return URL(fileURLWithPath: a).standardizedFileURL.path
            == URL(fileURLWithPath: b).standardizedFileURL.path
    }

    // MARK: - View-mode toggle + Sankey + recommendations

    private func toggleExpand(_ outcome: RestructureOutcome) {
        withAnimation(Theme.springStandard) {
            if expandedOutcome == outcome {
                expandedOutcome = nil
            } else {
                expandedOutcome = outcome
            }
        }
    }

    /// Inline file list for a recommendation card. Reads the cache built in `applyPlan()`, so the render path does no filtering or grouping.
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
                            Label("See all \(Plural.noun(matchedCount, "file", "files"))", systemImage: "arrow.up.right.square")
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
            // Keeps the Apply count and the apply pass in step.
            for p in proposals where outcomeFor(p) == outcome {
                selectedIDs.remove(p.fileID)
            }
        }
    }

    private func outcomeFor(_ p: Proposal) -> RestructureOutcome { Self.outcome(for: p) }

    nonisolated static func outcome(for p: Proposal) -> RestructureOutcome {
        switch p.kind {
        case .dissolved:        return .reorganize
        case .movedOutAsOutlier: return .tidy
        }
    }

    // MARK: - Drill-down sheet

    @ViewBuilder
    private func drillDownSheet(_ scope: DrillDownScope) -> some View {
        // Uses the grouping cached in `drillDownGroups`: a checkbox tap must not re-filter the set.
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
                DashedNotice(title: "No files to move here",
                             subtitle: "Nothing in the current plan moves from or to this folder. The plan may have changed since you opened this list. Press Done and look at the new plan.")
                    .padding(Theme.Metrics.pageInsetX)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView(.vertical) {
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
        .sheetSurface()
        .dsEscapeCancels { drillDown = nil }
    }

    /// Recomputes the filtered, bucketed proposals for a scope; called only when the scope changes.
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
