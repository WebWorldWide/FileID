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
// F-C3-021-app: the plan + apply now route through the ENGINE butler
// (Restructure.proposeAll / Restructure.apply via IPC), not the app-side
// classifier. `requestPlan()` sends `planRestructure`; the engine's
// `restructurePlan` event is mapped onto the view's `Proposal`/`AssistantSummary`
// model in `applyPlan()` and drives the Sankey + TreeDiff + recommendation rows.
// Apply sends `applyRestructure` and reacts to `restructureApplyResult`. The
// app-side `RestructureEngine` below is retained (unreferenced) for reference.
import SwiftUI
import AppKit
import GRDB
import FileIDShared

struct RestructureView: View {
    let store: ReadStore
    let engine: EngineClient

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
    @State private var showingPicker = false
    @State private var staysPutExpanded: Bool = false
    @State private var confirmApply: Bool = false
    /// Single-flight guard for the apply / convert paths. A real move is
    /// irreversible inside the app, so a double-click must never run apply
    /// twice — the bar buttons disable while this is set.
    @State private var applying = false

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
    @State private var hasPulsed = false
    @State private var captionedFraction: Double = 0
    @State private var totalAnalyzableFiles = 0
    @State private var dismissedDeepAnalyzeHint = false
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
        let kind: ProposalKind
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
        ZStack(alignment: .bottom) {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    header
                    if libraryRoot == nil
                        || (!summary.hasContent && proposals.isEmpty) {
                        emptyState
                    } else {
                        if !proposals.isEmpty || summary.hasContent {
                            RestructureStatHero(summary: summary,
                                                  hoverBus: hoverBus)
                            if shouldShowDeepAnalyzeHint {
                                deepAnalyzeHintBanner
                            }
                            HStack {
                                viewModeToggle
                                Spacer()
                            }
                        }
                        if !proposals.isEmpty {
                            if viewMode == .cards {
                                unifiedHeroSurface
                            } else {
                                treeCard
                            }
                        } else if summary.hasContent {
                            nothingToMoveCard
                        }
                        if summary.staysPutFiles > 0 {
                            staysPutSection
                        }
                    }
                    if let s = status {
                        statusBanner(s)
                    }
                    // Reserve room for the floating apply bar.
                    Color.clear.frame(height: applyBarVisible ? 96 : 0)
                }
                .padding(.horizontal, 18)
                .padding(.top, 18)
                .padding(.bottom, 12)
            }
            if applyBarVisible {
                RestructureApplyBar(
                    selectedCount: selectedIDs.count,
                    totalCount: proposals.count,
                    canApply: !selectedIDs.isEmpty,
                    isApplying: applying,
                    onApplyShortcuts: { if !applying { confirmApply = true } },
                    onConvertToMoves: { if !applying { confirmApply = true } }
                )
                .padding(.horizontal, 18)
                .padding(.bottom, 14)
                .transition(.move(edge: .bottom).combined(with: .opacity))
                // F-C3-021-app: both apply-bar actions now route through the
                // engine butler, which performs real on-disk moves (it has no
                // macOS symlink-preview mode). The button labels still read
                // "shortcuts"/"convert" — that copy lives in RestructureApplyBar
                // and is now stale; the confirmation below states the real,
                // irreversible behavior so the user always confirms first.
                .confirmationDialog(
                    "Apply \(selectedIDs.count) move\(selectedIDs.count == 1 ? "" : "s")?",
                    isPresented: $confirmApply,
                    titleVisibility: .visible
                ) {
                    Button("Apply real moves", role: .destructive) {
                        applyViaEngine()
                    }
                    Button("Cancel", role: .cancel) { }
                } message: {
                    Text("FileID will move the selected files into the new structure on disk and update its library. This runs through the engine and isn't reversible inside the app — review the structure first.")
                }
            }
        }
        .animation(.spring(response: 0.4, dampingFraction: 0.8), value: applyBarVisible)
        .sheet(item: $drillDown) { scope in
            drillDownSheet(scope)
        }
        .fileImporter(isPresented: $showingPicker,
                       allowedContentTypes: [.folder],
                       allowsMultipleSelection: false) { result in
            switch result {
            case .success(let urls):
                if let url = urls.first {
                    libraryRoot = url
                    requestPlan()
                }
            case .failure: break
            }
        }
        .task {
            // Auto-default the destination root to the most recently scanned
            // folder so proposals load immediately. User can still override
            // via "Change destination…" but the upfront blocker is gone —
            // they see what the assistant proposes the moment they open
            // the tab.
            if libraryRoot == nil, let session = store.recentSessions(limit: 1).first {
                let url = URL(fileURLWithPath: session.rootPath)
                if FileManager.default.fileExists(atPath: url.path) {
                    libraryRoot = url
                    requestPlan()
                }
            }
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
        // Engine-authoritative plan arrived (F-C3-021-app) — map it onto the
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
            } else {
                status = "\(result.applied) moved · \(result.failed) failed"
            }
            store.notifyChanged()
            requestPlan()
        }
        // The engine died before a terminal plan/apply event could clear the
        // local in-flight UI — release it so the tab doesn't wedge.
        .onChange(of: engine.engineResetSignal) { _, _ in
            if applying {
                applying = false
                status = "Engine restarted — apply interrupted. Recheck your library and try again."
            }
            loading = false
        }
        // A plan computation that failed engine-side ends the "Computing…"
        // spinner (it would otherwise spin forever waiting for a plan event).
        .onChange(of: engine.lastError?.message) { _, _ in
            guard loading, let kind = engine.lastError?.kind,
                  kind == "plan_restructure_failed" || kind == "db_unavailable" else { return }
            loading = false
            status = "Couldn't compute a plan. Please try again."
        }
        // Group the drill-down's proposals once per scope, not on every
        // checkbox tap inside the sheet.
        .onChange(of: drillDown) { _, newScope in
            rebuildDrillDownGroups(for: newScope)
        }
    }

    // MARK: - Unified surface

    private var applyBarVisible: Bool {
        libraryRoot != nil && !proposals.isEmpty
    }

    private var shouldShowDeepAnalyzeHint: Bool {
        guard !dismissedDeepAnalyzeHint else { return false }
        guard engine.deepAnalyzeAvailable else { return false }
        guard !engine.deepAnalyzeInFlight else { return false }
        guard totalAnalyzableFiles > 0 else { return false }
        return captionedFraction < 0.4
    }

    @ViewBuilder
    private var deepAnalyzeHintBanner: some View {
        let captioned = Int((captionedFraction * Double(totalAnalyzableFiles)).rounded())
        let pct = Int((captionedFraction * 100).rounded())
        HStack(alignment: .center, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 10)
                    .fill(Theme.ai.opacity(0.18))
                    .frame(width: 38, height: 38)
                Image(systemName: "sparkles")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(Theme.ai)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text("Sharper proposals with Deep Analyze")
                    .font(.callout.weight(.semibold))
                Text(captioned == 0
                      ? "Right now we're sorting by folder name + people only. Running Deep Analyze reads the contents of each file (captions, OCR text, scene tags) so receipts go to Documents, screenshots to Photos, and so on."
                      : "Captioned \(captioned) of \(totalAnalyzableFiles) (\(pct)%). Running Deep Analyze on the rest gives bucketing more to work with.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 6) {
                Button {
                    let modelKind = DeepAnalyzeSettings.shared.activeKind.rawValue
                    engine.deepAnalyzeAll(modelKind: modelKind, skipExisting: true)
                } label: {
                    Label("Run Deep Analyze", systemImage: "sparkles")
                        .font(.caption.weight(.semibold))
                        .padding(.horizontal, 12).padding(.vertical, 6)
                        .background(
                            Capsule().fill(Theme.ai.opacity(0.85))
                        )
                        .foregroundStyle(.black)
                }
                .buttonStyle(.plain)
                Button("Dismiss") {
                    dismissedDeepAnalyzeHint = true
                }
                .buttonStyle(.plain)
                .font(.caption2)
                .foregroundStyle(.tertiary)
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
        .background(
            RoundedRectangle(cornerRadius: 14)
                .fill(.ultraThinMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 14)
                        .stroke(Theme.ai.opacity(0.30), lineWidth: 1)
                )
        )
    }

    @ViewBuilder
    private var unifiedHeroSurface: some View {
        VStack(alignment: .leading, spacing: 0) {
            sankeyHeroSection
            Divider().opacity(0.18)
            recommendationsList
        }
        // Background lives on its own subtree so the cached blur +
        // shadow don't re-rasterize when interactive children update.
        .background(
            RoundedRectangle(cornerRadius: 18)
                .fill(.ultraThinMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 18)
                        .stroke(Color.white.opacity(0.07), lineWidth: 1)
                )
                .shadow(color: .black.opacity(0.28), radius: 22, y: 8)
        )
    }

    @ViewBuilder
    private var sankeyHeroSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "arrow.triangle.branch")
                    .foregroundStyle(Theme.gold)
                    .font(.callout.weight(.semibold))
                Text("Folder map")
                    .font(.title3.weight(.semibold))
                Spacer()
                Text(sankeyHeaderStat)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            Text("Hover any folder, ribbon, or card to trace where its files are going. Tap to drill into the exact list.")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
            SankeyFlowView(
                proposals: proposals,
                onTapSource: { folder in
                    let match = proposals.first(where: {
                        ($0.sourceFolder as NSString).lastPathComponent == folder
                    })?.sourceFolder ?? folder
                    drillDown = .sourceFolder(match)
                },
                onTapDestination: { bucket in
                    drillDown = .destBucket(bucket)
                },
                onTapSourceRollup: { folders in
                    drillDown = .sourceFolders(folders)
                },
                onTapDestRollup: { buckets in
                    drillDown = .destBuckets(buckets)
                },
                hoverBus: hoverBus
            )
            .padding(.top, 4)
        }
        .padding(.horizontal, 18).padding(.top, 18).padding(.bottom, 14)
    }

    @ViewBuilder
    private var recommendationsList: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "sparkles")
                    .foregroundStyle(Theme.gold)
                    .font(.callout.weight(.semibold))
                Text("Recommendations")
                    .font(.title3.weight(.semibold))
                Spacer()
                Text("Tap Skip to exclude a group from the next apply.")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            .padding(.horizontal, 22).padding(.top, 18).padding(.bottom, 12)

            VStack(spacing: 0) {
                if summary.anchorFolders > 0 {
                    RestructureRecommendationRow(
                        outcome: .keep,
                        headline: "Keep \(summary.anchorFolders) folder\(summary.anchorFolders == 1 ? "" : "s") untouched",
                        bodyText: "These folders already have clear names and matching contents. \(summary.staysPutFiles) file\(summary.staysPutFiles == 1 ? "" : "s") staying exactly where they are.",
                        fileCount: summary.staysPutFiles,
                        folderCount: summary.anchorFolders,
                        isApproved: true,
                        isInformational: true,
                        hoverBus: hoverBus,
                        onHover: { hovering in
                            hoverBus.set(hovering ? .outcome(.keep) : nil)
                        },
                        expandedContent: { EmptyView() }
                    )
                }
                if summary.mixedFolders > 0 {
                    if summary.anchorFolders > 0 {
                        Divider().opacity(0.14).padding(.leading, 76)
                    }
                    let approved = !skippedOutcomes.contains(.tidy)
                    let expanded = expandedOutcome == .tidy
                    RestructureRecommendationRow(
                        outcome: .tidy,
                        headline: "Tidy \(summary.mixedFolders) folder\(summary.mixedFolders == 1 ? "" : "s") — move \(summary.movedOutFiles) misplaced file\(summary.movedOutFiles == 1 ? "" : "s")",
                        bodyText: "Mostly-organized folders with a few files that don't fit the theme. The folder stays; the misplaced files go to where they belong.",
                        fileCount: summary.movedOutFiles,
                        folderCount: summary.mixedFolders,
                        isApproved: approved,
                        isExpanded: expanded,
                        hoverBus: hoverBus,
                        onToggleApproval: { toggleSkip(.tidy) },
                        onToggleExpand: { toggleExpand(.tidy) },
                        onHover: { hovering in
                            hoverBus.set(hovering ? .outcome(.tidy) : nil)
                        },
                        expandedContent: { inlineFileList(for: .outcome(.tidy)) }
                    )
                }
                if summary.junkFolders > 0 {
                    if summary.anchorFolders > 0 || summary.mixedFolders > 0 {
                        Divider().opacity(0.14).padding(.leading, 76)
                    }
                    let approved = !skippedOutcomes.contains(.reorganize)
                    let expanded = expandedOutcome == .reorganize
                    RestructureRecommendationRow(
                        outcome: .reorganize,
                        headline: "Reorganize \(summary.junkFolders) folder\(summary.junkFolders == 1 ? "" : "s") — sort \(summary.dissolvedFiles) file\(summary.dissolvedFiles == 1 ? "" : "s")",
                        bodyText: "Folders with generic names like \"Untitled\" or \"Camera Roll\" — files will be sorted into clear categories: People, Places, Documents, or Photos by year.",
                        fileCount: summary.dissolvedFiles,
                        folderCount: summary.junkFolders,
                        isApproved: approved,
                        isExpanded: expanded,
                        hoverBus: hoverBus,
                        onToggleApproval: { toggleSkip(.reorganize) },
                        onToggleExpand: { toggleExpand(.reorganize) },
                        onHover: { hovering in
                            hoverBus.set(hovering ? .outcome(.reorganize) : nil)
                        },
                        expandedContent: { inlineFileList(for: .outcome(.reorganize)) }
                    )
                }
            }
        }
    }

    /// Status banner shown after an apply / convert run. Quiet pill
    /// at the bottom of the page — replaces the prior GlassCard so
    /// it doesn't compete with the unified surface above.
    @ViewBuilder
    private func statusBanner(_ message: String) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "checkmark.seal.fill")
                .foregroundStyle(Theme.gold)
            Text(message)
                .font(.callout)
            Spacer()
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(.ultraThinMaterial)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(Theme.gold.opacity(0.25), lineWidth: 1)
        )
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center, spacing: 12) {
                Image(systemName: "rectangle.3.offgrid")
                    .font(.system(size: 32))
                    .foregroundStyle(Theme.gold)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Restructure").font(.largeTitle.bold())
                    Text("FileID looks at your folders, keeps the well-named ones, and proposes a tidier home for the rest. Nothing moves until you apply.")
                        .font(.callout).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer()
                Button {
                    showingPicker = true
                } label: {
                    Label(libraryRoot == nil ? "Pick destination root…" : "Change destination…",
                          systemImage: "folder")
                }
                .buttonStyle(.bordered)
            }
            if let root = libraryRoot {
                HStack(spacing: 4) {
                    Image(systemName: "folder.fill")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Text("Destination: \(root.path)")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1).truncationMode(.head)
                }
            }
        }
    }

    private var nothingToMoveCard: some View {
        GlassCard {
            HStack(spacing: 10) {
                Image(systemName: "checkmark.seal.fill")
                    .font(.title2)
                    .foregroundStyle(.green)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Nothing to move").font(.headline)
                    Text("Your library is already organized — every folder is a recognized anchor.")
                        .font(.callout).foregroundStyle(.secondary)
                }
                Spacer()
            }
        }
    }

    /// One row inside the "Staying put" disclosure. Hovering it
    /// writes `sourceFolder(name)` to the hover bus so the matching
    /// node in the Sankey lights up — the disclosure becomes a
    /// pointer into the diagram instead of a dead list.
    ///
    /// A standalone view (not an inline @ViewBuilder) so its read of
    /// `hoverBus` subscribes only this row — not the whole RestructureView
    /// body, which would otherwise re-pass the full proposal array to the
    /// Sankey/tree on every mouse-move.
    private struct StaysPutRow: View {
        let entry: (folder: String, count: Int)
        let hoverBus: RestructureHoverBus
        var body: some View {
            let isHovered = hoverBus.touchesSource(entry.folder)
            HStack(spacing: 8) {
                Image(systemName: "folder.fill")
                    .font(.caption)
                    .foregroundStyle(.green.opacity(isHovered ? 1.0 : 0.8))
                Text(entry.folder)
                    .font(.caption.monospaced())
                    .foregroundStyle(isHovered ? Color.primary : .secondary)
                Spacer()
                Text("\(entry.count) file\(entry.count == 1 ? "" : "s")")
                    .font(.caption2.monospaced())
                    .foregroundStyle(.tertiary)
            }
            .padding(.horizontal, 6).padding(.vertical, 3)
            .background(
                RoundedRectangle(cornerRadius: 4)
                    .fill(Color.green.opacity(isHovered ? 0.10 : 0))
            )
            .contentShape(Rectangle())
            .onHover { hovering in
                hoverBus.set(hovering ? .sourceFolder(entry.folder) : nil)
            }
            .animation(.easeInOut(duration: 0.18), value: isHovered)
        }
    }

    private var staysPutSection: some View {
        GlassCard {
            DisclosureGroup(isExpanded: $staysPutExpanded) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("These folders are recognized as anchors — meaningful name plus matching contents. Their files stay exactly where they are.")
                        .font(.caption).foregroundStyle(.secondary)
                        .padding(.top, 4)
                    if !summary.staysPutBreakdown.isEmpty {
                        Divider().opacity(0.3)
                        ForEach(Array(summary.staysPutBreakdown.enumerated()), id: \.offset) { _, entry in
                            StaysPutRow(entry: entry, hoverBus: hoverBus)
                        }
                    }
                }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "lock.fill")
                        .foregroundStyle(.green)
                    Text("Staying put").font(.headline)
                    Text("\(summary.anchorFolders) folder\(summary.anchorFolders == 1 ? "" : "s") · \(summary.staysPutFiles) file\(summary.staysPutFiles == 1 ? "" : "s")")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                    Spacer()
                }
            }
        }
    }

    @ViewBuilder
    private var emptyState: some View {
        if libraryRoot == nil {
            EmptyStateView(
                icon: "rectangle.3.offgrid",
                title: "Pick a destination root",
                message: "Choose where the proposed folder hierarchy should live. Files won't move yet — you'll preview the changes first and apply them as shortcuts before committing to real moves."
            )
        } else if loading {
            // Computing state lives on a clipped LavaLamp surface so
            // the page never goes flat while the engine works. Same
            // gold/orange canvas + frosted material as the rest of
            // the app — the only "loading" UI in the tree that earns
            // its motion budget.
            ZStack {
                LavaLampBackground()
                    .clipShape(RoundedRectangle(cornerRadius: 14))
                VStack(spacing: 10) {
                    ProgressView()
                        .controlSize(.large)
                        .tint(Theme.gold)
                    Text("Computing proposals…")
                        .font(.callout.bold())
                    Text("Looking at every folder, classifying it, and picking a tidy home for every file.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 380)
                }
                .padding(28)
            }
            .frame(maxWidth: .infinity, minHeight: 240)
            .overlay(
                RoundedRectangle(cornerRadius: 14)
                    .stroke(Theme.gold.opacity(0.18), lineWidth: 1)
            )
        } else {
            EmptyStateView(
                icon: "rectangle.3.offgrid",
                title: "Nothing to restructure yet",
                message: "Pick a folder in the sidebar and click Start Scan. Once images are tagged, the assistant will propose a clean folder layout you can apply."
            )
        }
    }

    // MARK: - File list

    /// Render a destination bucket + its file list. The inline
    /// preview card and the inline expanded recommendation card cap
    /// at 50 rows + a "+ X more" hint to keep the page short.
    /// The drill-down sheet calls this with `unlimited: true` so
    /// every file is visible — that's the whole point of "drill
    /// down". `LazyVStack` upstream handles the virtualization, so
    /// even buckets with thousands of files scroll smoothly.
    @ViewBuilder
    private func bucketSection(_ g: Group, unlimited: Bool = false) -> some View {
        let allSelected = g.proposals.allSatisfy { selectedIDs.contains($0.fileID) }
        let cap = 50
        let visible: ArraySlice<Proposal> = unlimited
            ? ArraySlice(g.proposals)
            : g.proposals.prefix(cap)
        let hiddenCount = unlimited ? 0 : max(0, g.proposals.count - cap)
        VStack(alignment: .leading, spacing: 0) {
            // Folder header — visually distinct from the files below.
            // Bigger icon, two-line label, gold tint = "this is the
            // destination folder."
            HStack(alignment: .center, spacing: 10) {
                Image(systemName: bucketIcon(g.bucket))
                    .foregroundStyle(Theme.gold)
                    .font(.title3)
                    .frame(width: 22, alignment: .center)
                VStack(alignment: .leading, spacing: 1) {
                    Text(g.bucket).font(.callout.bold())
                    Text("Folder · \(g.proposals.count) file\(g.proposals.count == 1 ? "" : "s") moving in")
                        .font(.caption2).foregroundStyle(.secondary)
                }
                Spacer()
                Button(allSelected ? "Deselect all" : "Select all") {
                    if allSelected {
                        for p in g.proposals { selectedIDs.remove(p.fileID) }
                    } else {
                        for p in g.proposals { selectedIDs.insert(p.fileID) }
                    }
                }
                .buttonStyle(.borderless)
                .font(.caption)
            }
            .padding(.bottom, 8)
            // File list — indented with a thin vertical tree guide on
            // the left. Reads as "these files live inside that folder".
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(visible)) { p in
                    proposalRow(p)
                }
                if hiddenCount > 0 {
                    Text("+ \(hiddenCount) more file\(hiddenCount == 1 ? "" : "s")")
                        .font(.caption2).foregroundStyle(.tertiary)
                        .padding(.leading, 4)
                }
            }
            .padding(.leading, 28)
            .overlay(alignment: .leading) {
                // Tree guide — same idiom as Finder column view.
                Rectangle()
                    .fill(Color.secondary.opacity(0.25))
                    .frame(width: 1)
                    .padding(.leading, 11)
            }
        }
    }

    private func proposalRow(_ p: Proposal) -> some View {
        let on = selectedIDs.contains(p.fileID)
        let filename = URL(fileURLWithPath: p.oldPath).lastPathComponent
        let sourceName = (p.sourceFolder as NSString).lastPathComponent
        let icon = Self.fileIcon(forFilename: filename)
        return HStack(spacing: 8) {
            Image(systemName: on ? "checkmark.square.fill" : "square")
                .foregroundStyle(on ? Theme.gold : .secondary)
                .onTapGesture {
                    if on { selectedIDs.remove(p.fileID) } else { selectedIDs.insert(p.fileID) }
                }
            // File icon — distinguishes a file row from the folder
            // header above it at a glance.
            Image(systemName: icon)
                .foregroundStyle(.secondary)
                .font(.caption)
                .frame(width: 14, alignment: .center)
            VStack(alignment: .leading, spacing: 1) {
                Text(filename)
                    .font(.caption.monospaced())
                    .lineLimit(1).truncationMode(.head)
                Text("from \(sourceName.isEmpty ? "root" : sourceName)")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .lineLimit(1).truncationMode(.head)
            }
            Spacer()
        }
        .contentShape(Rectangle())
        .onTapGesture {
            if on { selectedIDs.remove(p.fileID) } else { selectedIDs.insert(p.fileID) }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Move \(filename) from \(p.sourceFolder) to \(p.bucket)")
        .accessibilityAddTraits([.isButton, on ? .isSelected : []])
        .accessibilityHint(on ? "Selected. Tap to skip." : "Tap to include in the next apply.")
    }

    /// SF Symbol for a filename based on its extension. Helps the user
    /// scan a long file list — photos vs videos vs PDFs are visually
    /// distinct without reading the extension.
    private static func fileIcon(forFilename name: String) -> String {
        let ext = (name as NSString).pathExtension.lowercased()
        switch ext {
        case "jpg", "jpeg", "png", "heic", "heif", "gif", "tiff", "tif", "bmp", "webp", "raw", "dng":
            return "photo"
        case "mp4", "mov", "m4v", "avi", "mkv", "webm":
            return "video"
        case "pdf":
            return "doc.richtext"
        case "doc", "docx", "pages", "rtf", "txt", "md":
            return "doc.text"
        case "mp3", "m4a", "wav", "flac", "aac", "ogg":
            return "waveform"
        case "zip", "tar", "gz", "7z":
            return "archivebox"
        default:
            return "doc"
        }
    }

    private func bucketIcon(_ bucket: String) -> String { bucketIconName(bucket) }

    // MARK: - Engine-routed plan + apply (F-C3-021-app)

    /// Ask the engine butler for a fresh restructure plan. The reply arrives
    /// asynchronously on `engine.restructurePlan`; `applyPlan()` (driven by
    /// `engine.restructurePlanSignal`) maps it onto the view's proposal model.
    private func requestPlan() {
        guard let root = libraryRoot else { return }
        // Capture per-file deselections BEFORE the new plan lands so a re-plan
        // (e.g. after Deep Analyze finishes mid-review) doesn't re-check rows
        // the user unchecked. Re-applied in applyPlan().
        priorDeselectedIDs = Set(proposals.map(\.fileID)).subtracting(selectedIDs)
        loading = true
        status = nil
        if !engine.planRestructure(libraryRoot: root.path) {
            loading = false
            status = "Engine is starting — reopen the tab to load your plan."
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

        let mapped = Self.mapProposals(from: plan)
        proposals = mapped
        summary = Self.makeSummary(from: plan, proposals: mapped)

        let by = Dictionary(grouping: mapped, by: { $0.bucket })
        groups = by.map { Group(bucket: $0.key, proposals: $0.value) }
            .sorted { $0.proposals.count > $1.proposals.count }

        selectedIDs = Set(mapped.map(\.fileID))
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

        loading = false
    }

    /// Apply the selected, non-skipped moves through the engine butler.
    /// Single-flight (F-C4-003): `applying` gates the apply-bar buttons and is
    /// cleared when `restructureApplyResult` arrives (or the engine resets).
    private func applyViaEngine() {
        guard !applying else { return }
        guard let root = libraryRoot, let plan = engine.restructurePlan else {
            status = "Pick a library folder before applying."
            return
        }
        // Selected ∧ not-skipped (F-C4-012). Belt-and-suspenders: filter the
        // engine plan's own moves by this set so a skipped outcome never moves.
        let eligible = Set(
            proposals
                .filter { selectedIDs.contains($0.fileID) && !skippedOutcomes.contains(outcomeFor($0)) }
                .map(\.fileID))
        let moves = plan.moves.filter { eligible.contains($0.fileID) }
        guard !moves.isEmpty else {
            status = "Nothing selected to apply."
            return
        }
        applying = true
        status = nil
        if !engine.applyRestructure(libraryRoot: root.path, moves: moves) {
            applying = false
            status = "Engine is unavailable — try again in a moment."
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
                kind: kind(forTier: m.tier))
        }
    }

    /// Derive the at-a-glance summary from the plan. Folder tiers come from the
    /// engine's `folderClassifications` when present; otherwise (current macOS
    /// engine) every received move is a relocation — the engine strips
    /// anchor-folder moves before sending — so we count distinct source folders
    /// per outcome. That is aggregation of the engine's moves, NOT a re-run of
    /// the retired app-side classifier.
    static func makeSummary(from plan: RestructurePlan, proposals: [Proposal]) -> AssistantSummary {
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
            // The plan carries only moves; stay-put (anchor) files are absent,
            // so a stay-put file count isn't derivable here.
            staysPutFiles: 0,
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

    /// Cards/Tree segmented toggle in a glass pill. Always visible
    /// when there's content to show.
    @ViewBuilder
    private var viewModeToggle: some View {
        HStack(spacing: 0) {
            modeButton(.cards, label: "Cards", icon: "rectangle.stack")
            modeButton(.tree,  label: "Tree",  icon: "list.bullet.indent")
            Spacer()
            Text(viewMode == .cards
                  ? "Recommendations + flow diagram"
                  : "Side-by-side folder trees (power user)")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 4)
    }

    @ViewBuilder
    private func modeButton(_ mode: ViewMode, label: String, icon: String) -> some View {
        let active = viewMode == mode
        Button {
            withAnimation(.easeInOut(duration: 0.18)) {
                viewModeRaw = mode.rawValue
            }
        } label: {
            HStack(spacing: 4) {
                Image(systemName: icon).font(.caption)
                Text(label).font(.caption.bold())
            }
            .padding(.horizontal, 12).padding(.vertical, 6)
            .background(
                Capsule().fill(active ? Theme.gold : Color.clear)
            )
            .foregroundStyle(active ? .black : Color.primary.opacity(0.7))
        }
        .buttonStyle(.plain)
    }

    private var sankeyHeaderStat: String {
        let srcCount = Set(proposals.map(\.sourceFolder)).count
        let dstCount = Set(proposals.map(\.bucket)).count
        return "\(srcCount) source\(srcCount == 1 ? "" : "s") → \(dstCount) destination\(dstCount == 1 ? "" : "s")"
    }

    private func toggleExpand(_ outcome: RestructureOutcome) {
        withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
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
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(cachedGroups.enumerated()), id: \.element.id) { idx, g in
                    if idx > 0 { Divider().opacity(0.18) }
                    bucketSection(g)
                }
                if matchedCount > totalCap {
                    HStack {
                        Spacer()
                        Button {
                            drillDown = scope
                        } label: {
                            Label("See all \(matchedCount) files in detail",
                                  systemImage: "arrow.up.right.square")
                                .font(.caption.bold())
                        }
                        .buttonStyle(.borderless)
                        .foregroundStyle(.secondary)
                    }
                    .padding(.top, 4)
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

    /// Tree-diff dual-pane (power-user view).
    private var treeCard: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 6) {
                    Image(systemName: "list.bullet.indent")
                        .foregroundStyle(Theme.gold)
                    Text("Side-by-side tree").font(.headline)
                    Spacer()
                    Text("Tap any folder to see its files.")
                        .font(.caption2).foregroundStyle(.tertiary)
                }
                TreeDiffView(
                    proposals: proposals,
                    summary: summary,
                    onTapSource: { folder in
                        drillDown = .sourceFolder(folder)
                    },
                    onTapDestination: { bucket in
                        drillDown = .destBucket(bucket)
                    },
                    hoverBus: hoverBus
                )
            }
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
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: drillDownIcon(scope))
                    .foregroundStyle(Theme.gold)
                    .font(.title3)
                VStack(alignment: .leading, spacing: 1) {
                    Text(title).font(.headline)
                    Text("\(matchedCount) file\(matchedCount == 1 ? "" : "s") affected. Tap any row to include or skip it before applying.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done") { drillDown = nil }
                    .keyboardShortcut(.defaultAction)
            }
            Divider().opacity(0.3)
            if groups.isEmpty {
                VStack(spacing: 6) {
                    Image(systemName: "checkmark.seal.fill")
                        .font(.system(size: 36)).foregroundStyle(.green)
                    Text("Nothing to show.").font(.callout.bold())
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(40)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 8) {
                        // Buckets pre-grouped in `rebuildDrillDownGroups`.
                        // Inside the drill-down we render `unlimited: true`
                        // so big buckets aren't truncated — the LazyVStack
                        // keeps it cheap.
                        ForEach(Array(groups.enumerated()), id: \.element.id) { idx, g in
                            if idx > 0 { Divider().opacity(0.18) }
                            bucketSection(g, unlimited: true)
                        }
                    }
                    .padding(8)
                }
            }
        }
        .padding(20)
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
            return "Going to \(bs.count) smaller bucket\(bs.count == 1 ? "" : "s")"
        }
    }

    private func drillDownIcon(_ scope: DrillDownScope) -> String {
        switch scope {
        case .all:                       return "list.bullet.indent"
        case .outcome(let o):            return o.icon
        case .sourceFolder:              return "folder.fill"
        case .destBucket:                return "tray.and.arrow.down.fill"
        case .sourceFolders:             return "rectangle.3.offgrid.fill"
        case .destBuckets:               return "rectangle.3.offgrid.fill"
        }
    }
}

// MARK: - Engine (app-side)

enum RestructureEngine {

    /// Result of a `compute` call: proposals + an at-a-glance summary
    /// of what the assistant did.
    struct ComputeResult: Sendable {
        let proposals: [RestructureView.Proposal]
        let summary: RestructureView.AssistantSummary
    }

    /// Walk every file in the library, group by current parent folder,
    /// classify each folder as Anchor / Mixed / Junk via FolderClassifier,
    /// and emit move proposals only for outliers (Mixed) or for every
    /// file (Junk). Files inside Anchor folders stay put — no proposals.
    static func compute(store: ReadStore, libraryRoot: URL?) -> ComputeResult {
        guard let root = libraryRoot else {
            return ComputeResult(proposals: [], summary: .empty)
        }
        // 1. Pull every file via the paged accessor.
        var all: [FileRow] = []
        let page = 5000
        var offset = 0
        while true {
            let batch = store.files(offset: offset, limit: page, search: "", kindFilter: nil)
            if batch.isEmpty { break }
            all.append(contentsOf: batch)
            offset += batch.count
            if batch.count < page { break }
        }

        // 2. Build the helper maps the classifier consumes.
        let nameMap = Self.fileToPersonNames(store: store)
        let knownPersons: [KnownPerson] = store.persons()
            .filter { $0.hasAnyName && !$0.isUnknown }
            .map { KnownPerson(id: $0.id, displayName: $0.displayName) }

        // 3. Group files by their current parent folder.
        var byFolder: [String: [FileRow]] = [:]
        for f in all {
            let parent = (f.pathText as NSString).deletingLastPathComponent
            byFolder[parent, default: []].append(f)
        }

        // 4. Run the classifier.
        let folderMetas: [String: [FileMeta]] = byFolder.mapValues { rows in
            rows.map { FileMeta(id: $0.id, date: $0.displayDate) }
        }
        let classifications = FolderClassifier.classifyAll(
            byFolder: folderMetas,
            knownPersons: knownPersons,
            personLookup: nameMap
        )
        let tierByFolder: [String: FolderClassifier.Tier] =
            Dictionary(uniqueKeysWithValues: classifications.map { ($0.folderPath, $0.tier) })

        // 5. Generate proposals based on each file's enclosing tier.
        let cal = Calendar(identifier: .gregorian)
        var out: [RestructureView.Proposal] = []
        out.reserveCapacity(all.count)
        var anchorFolders = 0, mixedFolders = 0, junkFolders = 0
        var staysPut = 0, movedOut = 0, dissolved = 0
        // Per-anchor folder breakdown for the "Staying put" disclosure.
        var staysPutBreakdown: [(folder: String, count: Int)] = []

        for c in classifications {
            switch c.tier {
            case .anchor:
                anchorFolders += 1
                staysPut += c.fileCount
                staysPutBreakdown.append((folder: c.folderName, count: c.fileCount))
            case .mixed:         mixedFolders  += 1
            case .junk:          junkFolders   += 1
            }
        }
        staysPutBreakdown.sort { $0.folder.localizedCaseInsensitiveCompare($1.folder) == .orderedAscending }

        for f in all {
            let parent = (f.pathText as NSString).deletingLastPathComponent
            let tier = tierByFolder[parent] ?? .junk

            // Decide whether this file moves and what kind of move.
            let kind: RestructureView.ProposalKind?
            switch tier {
            case .anchor:
                kind = nil   // stays put, no proposal
            case .mixed(_, _, let outlierIDs):
                if outlierIDs.contains(f.id) {
                    kind = .movedOutAsOutlier
                    movedOut += 1
                } else {
                    kind = nil   // matches the folder's intent; stays put
                    staysPut += 1
                }
            case .junk:
                kind = .dissolved
                dissolved += 1
            }
            guard let proposalKind = kind else { continue }

            // Sanitize bucket + baseName so a malicious vlm_proposed_name
            // (e.g. "../../etc/passwd") can't escape the library root.
            let bucket = sanitizePathSegment(
                Self.bucketForFile(f, nameMap: nameMap, cal: cal)
            )
            let oldURL = f.url
            let ext = oldURL.pathExtension
            let rawBase: String
            if let p = f.vlmProposedName, !p.isEmpty {
                rawBase = ext.isEmpty ? p : "\(p).\(ext)"
            } else {
                rawBase = oldURL.lastPathComponent
            }
            let baseName = sanitizeFilename(rawBase)
            let target = root
                .appendingPathComponent(bucket, isDirectory: true)
                .appendingPathComponent(baseName)

            // Containment check: even with sanitization, verify the
            // resolved target sits inside the library root before we
            // record a proposal. Drop anything that doesn't.
            let resolvedTarget = target.standardizedFileURL.path
            let resolvedRoot = root.standardizedFileURL.path
            guard resolvedTarget.hasPrefix(resolvedRoot + "/") else {
                continue
            }

            // Skip if the destination is identical to the source (file
            // already lives where the heuristic would put it).
            guard resolvedTarget != f.pathText else {
                if proposalKind == .movedOutAsOutlier { movedOut -= 1; staysPut += 1 }
                if proposalKind == .dissolved        { dissolved -= 1; staysPut += 1 }
                continue
            }
            out.append(RestructureView.Proposal(
                fileID: f.id,
                oldPath: f.pathText,
                newPath: resolvedTarget,
                bucket: bucket,
                sourceFolder: parent,
                kind: proposalKind
            ))
        }

        let summary = RestructureView.AssistantSummary(
            anchorFolders: anchorFolders,
            mixedFolders:  mixedFolders,
            junkFolders:   junkFolders,
            staysPutFiles: staysPut,
            movedOutFiles: movedOut,
            dissolvedFiles: dissolved,
            staysPutBreakdown: staysPutBreakdown
        )
        return ComputeResult(proposals: out, summary: summary)
    }

    /// Pure-heuristic destination bucket for a file. Same logic the
    /// previous flat compute() used; isolated so the new tier-aware
    /// compute() can call it for files in Junk / Mixed-outlier contexts.
    private static func bucketForFile(
        _ f: FileRow,
        nameMap: [Int64: [String]],
        cal: Calendar
    ) -> String {
        let date = f.displayDate
        let year = date.map { String(cal.component(.year, from: $0)) }
        let month = date.map { Self.monthName(cal.component(.month, from: $0)) }
        // VLM-driven document subcategory wins over the
        // people/places/year heuristics when DA has captioned the
        // file — keeps a screenshot or receipt out of People/<face>.
        if let vlmSubcategory = vlmDocumentSubcategory(for: f) {
            return "Documents/" + vlmSubcategory + (year.map { "/\($0)" } ?? "")
        }
        if let names = nameMap[f.id], let first = names.first, !first.isEmpty {
            return "People/\(first)" + (year.map { "/\($0)" } ?? "")
        } else if let lat = f.locationLat, let lon = f.locationLon {
            let latB = (lat * 2).rounded() / 2
            let lonB = (lon * 2).rounded() / 2
            return "Places/" + String(format: "%.1f_%.1f", latB, lonB) + (year.map { "/\($0)" } ?? "")
        } else if f.hasText || f.kind == "pdf" || f.kind == "doc" {
            return "Documents" + (year.map { "/\($0)" } ?? "")
        } else if let y = year {
            return "Photos/\(y)" + (month.map { "/\($0)" } ?? "")
        } else {
            return "Misc"
        }
    }

    /// If the VLM caption strongly identifies this image as a kind of
    /// document (receipt, screenshot, diagram, scanned form, etc.),
    /// return the appropriate Documents subcategory. Otherwise nil and
    /// the original heuristic takes over.
    private static func vlmDocumentSubcategory(for f: FileRow) -> String? {
        guard let desc = f.vlmDescription?.lowercased(), !desc.isEmpty else {
            return nil
        }
        // Receipt / invoice / bill / order
        if desc.contains("receipt") || desc.contains("invoice")
            || desc.contains("bill") || desc.contains("order confirmation") {
            return "Receipts"
        }
        // Screenshot
        if desc.contains("screenshot") || desc.contains("screen capture")
            || desc.contains("screen recording") {
            return "Screenshots"
        }
        // Forms / contracts / official paperwork
        if desc.contains("form") || desc.contains("contract")
            || desc.contains("agreement") || desc.contains("application")
            || desc.contains("license") {
            return "Forms"
        }
        // Tickets / boarding passes / itineraries
        if desc.contains("ticket") || desc.contains("boarding pass")
            || desc.contains("itinerary") {
            return "Travel"
        }
        // ID cards / passport / driver's license
        if desc.contains("passport") || desc.contains("driver's license")
            || desc.contains("id card") || desc.contains("identification") {
            return "ID"
        }
        // Diagrams / charts / whiteboards
        if desc.contains("whiteboard") || desc.contains("diagram")
            || desc.contains("chart") || desc.contains("flowchart")
            || desc.contains("mind map") {
            return "Diagrams"
        }
        return nil
    }

    private static func fileToPersonNames(store: ReadStore) -> [Int64: [String]] {
        var out: [Int64: [String]] = [:]
        for person in store.persons() {
            guard person.hasAnyName, !person.isUnknown else { continue }
            let name = person.displayName
            for f in store.files(forPersonID: person.id, limit: 5000) {
                out[f.id, default: []].append(name)
            }
        }
        return out
    }

    private static func monthName(_ m: Int) -> String {
        let names = ["", "01-Jan","02-Feb","03-Mar","04-Apr","05-May","06-Jun",
                     "07-Jul","08-Aug","09-Sep","10-Oct","11-Nov","12-Dec"]
        return names[max(1, min(12, m))]
    }

    // MARK: Apply

    struct ApplyResult: Sendable {
        let moved: Int
        let skipped: Int
        let failed: Int
        let conflicts: [String]
        let mode: ApplyMode
    }

    enum ApplyMode: Sendable {
        case symlink   // Originals untouched; new tree mirrors via symlinks.
        case realMove  // `mv` on disk; updates DB path_text rows.
    }

    /// Symlink mode (default) leaves originals in place; realMove `mv`s
    /// each file and rewrites its path_text row immediately after the
    /// move (rolling the file back on DB failure), so a crash or WAL
    /// contention strands at most one file instead of the whole batch.
    static func apply(proposals: [RestructureView.Proposal],
                       store: ReadStore, mode: ApplyMode,
                       libraryRoot: URL) async -> ApplyResult {
        let fm = FileManager.default
        var moved = 0
        var skipped = 0
        var failed = 0
        var conflicts: [String] = []
        let resolvedRoot = libraryRoot.resolvingSymlinksInPath().path

        // Fail fast: if the DB isn't writable (engine mid-batch holding
        // the lock past the busy timeout, wiped DB, …), move NOTHING —
        // moved-but-unrecorded files are the failure mode this prevents.
        var updateQueue: DatabaseQueue?
        if mode == .realMove {
            guard let q = try? store.openPathUpdateQueue() else {
                return ApplyResult(moved: 0, skipped: 0, failed: proposals.count,
                                   conflicts: [], mode: mode)
            }
            updateQueue = q
        }

        for p in proposals {
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let newURL = URL(fileURLWithPath: p.newPath)
            if oldURL == newURL { skipped += 1; continue }
            let parent = newURL.deletingLastPathComponent()
            // SEC-7: destination's resolved parent must stay inside the
            // resolved root — a symlinked bucket component must not let
            // a move (or link) land outside the authorized tree.
            guard pathIsContained(parent, inResolvedRoot: resolvedRoot) else {
                failed += 1; continue
            }
            do {
                try fm.createDirectory(at: parent, withIntermediateDirectories: true)
            } catch {
                failed += 1; continue
            }
            // SEC-5: re-verify after createDirectory (check-to-use window).
            guard pathIsContained(parent, inResolvedRoot: resolvedRoot) else {
                failed += 1; continue
            }
            // No fileExists pre-check: it opens a TOCTOU window.
            // createSymbolicLink / moveItem already throw on existing
            // destination — that's the atomic test.
            do {
                switch mode {
                case .symlink:
                    try fm.createSymbolicLink(at: newURL, withDestinationURL: oldURL)
                case .realMove:
                    try fm.moveItem(at: oldURL, to: newURL)
                    do {
                        try store.updatePathText(fileID: p.fileID,
                                                 newPath: newURL.path,
                                                 on: updateQueue!)
                    } catch {
                        // Disk and DB must not diverge — put the file back.
                        try? fm.moveItem(at: newURL, to: oldURL)
                        failed += 1
                        continue
                    }
                }
                moved += 1
            } catch CocoaError.fileWriteFileExists {
                conflicts.append(p.newPath); skipped += 1
            } catch let err as NSError where err.domain == NSPOSIXErrorDomain && err.code == EEXIST {
                conflicts.append(p.newPath); skipped += 1
            } catch {
                // Some FileManager errors surface as generic NSErrors;
                // treat any "file exists at destination" indicator as
                // a conflict, otherwise count as failed.
                if fm.fileExists(atPath: newURL.path) {
                    conflicts.append(p.newPath); skipped += 1
                } else {
                    failed += 1
                }
            }
        }
        if moved > 0 { store.notifyChanged() }
        return ApplyResult(moved: moved, skipped: skipped, failed: failed,
                           conflicts: conflicts, mode: mode)
    }

    /// For each proposal whose newPath is a symlink → original, replace
    /// it with a real move and update the DB row immediately (rolling
    /// back on DB failure).
    static func convertSymlinksToMoves(proposals: [RestructureView.Proposal],
                                        store: ReadStore) async -> ApplyResult {
        let fm = FileManager.default
        var moved = 0
        var skipped = 0
        var failed = 0
        guard let updateQueue = try? store.openPathUpdateQueue() else {
            return ApplyResult(moved: 0, skipped: 0, failed: proposals.count,
                               conflicts: [], mode: .realMove)
        }
        for p in proposals {
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let newURL = URL(fileURLWithPath: p.newPath)
            // Skip if newPath isn't a symlink (hand-edited tree).
            guard let attrs = try? fm.attributesOfItem(atPath: newURL.path),
                  let type = attrs[.type] as? FileAttributeType,
                  type == .typeSymbolicLink else {
                skipped += 1; continue
            }
            // Verify the symlink still points where we created it.
            // An attacker with local filesystem access could swap the
            // link to redirect a "convert to real move" toward a
            // sensitive file (e.g. /etc/passwd).
            let destPath = (try? fm.destinationOfSymbolicLink(atPath: newURL.path))
                .map { resolveSymlinkDestination(linkPath: newURL.path, relative: $0) }
            guard let resolvedDest = destPath, resolvedDest == p.oldPath else {
                skipped += 1; continue
            }
            // unlink(2), not removeItem: it deletes only a link/file and
            // fails on a directory, so a check-to-use swap can never turn
            // this into a recursive delete of real user data.
            guard unlink(newURL.path) == 0 else {
                skipped += 1; continue
            }
            do {
                try fm.moveItem(at: oldURL, to: newURL)
            } catch {
                // Restore the link so the tree stays browsable.
                try? fm.createSymbolicLink(at: newURL, withDestinationURL: oldURL)
                failed += 1; continue
            }
            do {
                try store.updatePathText(fileID: p.fileID,
                                         newPath: newURL.path,
                                         on: updateQueue)
                moved += 1
            } catch {
                try? fm.moveItem(at: newURL, to: oldURL)
                try? fm.createSymbolicLink(at: newURL, withDestinationURL: oldURL)
                failed += 1
            }
        }
        if moved > 0 { store.notifyChanged() }
        return ApplyResult(moved: moved, skipped: skipped, failed: failed,
                           conflicts: [], mode: .realMove)
    }

    /// Strip path separators and `..` segments from a single bucket
    /// path (which itself may be multi-level like `Documents/Receipts`).
    /// Drops empty components so a leading slash can't escape root.
    static func sanitizePathSegment(_ raw: String) -> String {
        let parts = raw.split(separator: "/", omittingEmptySubsequences: true)
            .map { String($0) }
            .filter { $0 != "." && $0 != ".." && !$0.isEmpty }
            .map { sanitizeFilename($0) }
        return parts.joined(separator: "/")
    }

    /// Sanitize a generated filename via the canonical cross-platform
    /// rules (FilesystemNameSafe: illegal/control chars, trailing
    /// dots/spaces, Windows reserved basenames, length cap) — the old
    /// hand-rolled version here only stripped `/` and NUL, so a
    /// restructure could mint folders Windows can't read when the
    /// library syncs cross-platform. Leading dots still stripped so a
    /// generated name never becomes a hidden file.
    static func sanitizeFilename(_ raw: String) -> String {
        var s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while s.hasPrefix(".") { s.removeFirst() }
        if s.isEmpty || s == "." || s == ".." { return "untitled" }
        let safe = FilesystemNameSafe.componentSafe(s)
        return safe == "_" ? "untitled" : safe
    }

    /// Resolve a symlink target that may be relative to the link's
    /// containing directory; returns an absolute path with `..`
    /// segments collapsed for safe equality comparison against the
    /// original `oldPath`.
    private static func resolveSymlinkDestination(linkPath: String,
                                                    relative target: String) -> String {
        if target.hasPrefix("/") {
            return URL(fileURLWithPath: target).standardizedFileURL.path
        }
        let dir = (linkPath as NSString).deletingLastPathComponent
        let combined = (dir as NSString).appendingPathComponent(target)
        return URL(fileURLWithPath: combined).standardizedFileURL.path
    }
}
