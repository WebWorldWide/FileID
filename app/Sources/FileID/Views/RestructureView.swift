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
// The classifier lives in Database/FolderClassifier.swift. The View
// here visualizes the resulting proposals + their impact category.
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
    @State private var selectedIDs: Set<Int64> = []
    @State private var loading = false
    @State private var status: String?
    @State private var showingPicker = false
    @State private var staysPutExpanded: Bool = false
    @State private var confirmConvertToRealMoves: Bool = false

    // V7 — view-mode toggle (cards / tree), per-card approval state,
    // drill-down sheet scope.
    @AppStorage("restructure.viewMode") private var viewModeRaw: String = ViewMode.cards.rawValue
    private var viewMode: ViewMode {
        ViewMode(rawValue: viewModeRaw) ?? .cards
    }
    enum ViewMode: String { case cards, tree }

    /// Outcomes the user has explicitly skipped from the apply pass.
    /// Default = empty (everything approved). Cards visually de-emphasize
    /// skipped outcomes and the Apply button respects the filter.
    @State private var skippedOutcomes: Set<RestructureOutcome> = []
    /// Single-card expand: at most one recommendation card shows its
    /// inline file list at a time. Click another card to switch.
    @State private var expandedOutcome: RestructureOutcome? = nil
    @State private var drillDown: DrillDownScope? = nil

    /// Hover bus shared by Sankey, recommendation cards, tree, and
    /// the staysPut disclosure. Hovering any folder or card surfaces
    /// the same context — every connected ribbon, card, and row
    /// highlights together. Lifted from SankeyFlowView's local @State
    /// so cross-highlight isn't trapped inside one view.
    @State private var hoverBus = RestructureHoverBus()
    /// Drives the gold one-shot pulse on the Apply button when
    /// proposals first become non-empty after computation. Single-fire
    /// per session — re-arming the trigger requires `hasPulsed` reset.
    @State private var hasPulsed: Bool = false

    /// Which subset of proposals the drill-down sheet renders.
    enum DrillDownScope: Identifiable, Hashable {
        case all
        case outcome(RestructureOutcome)
        case sourceFolder(String)
        case destBucket(String)
        var id: String {
            switch self {
            case .all:                       return "all"
            case .outcome(let o):            return "outcome:\(o.rawValue)"
            case .sourceFolder(let f):       return "src:\(f)"
            case .destBucket(let b):         return "dst:\(b)"
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
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                header
                if libraryRoot == nil || (!summary.hasContent && proposals.isEmpty && !loading) {
                    emptyState
                } else {
                    // View-mode toggle — Cards (Sankey + recommendations,
                    // mom-friendly) vs Tree (dual-pane, power-user).
                    if !proposals.isEmpty || summary.hasContent {
                        viewModeToggle
                    }
                    if !proposals.isEmpty {
                        actionsBar
                        if viewMode == .cards {
                            sankeyCard
                            recommendationsStack
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
                    GlassCard {
                        Text(s).font(.callout)
                    }
                }
            }
            .padding(20)
        }
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
                    Task { await regenerate() }
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
                    await regenerate()
                }
            }
        }
        // Recompute proposals only on Deep Analyze terminal events
        // (`.deepAnalyzeComplete`). The previous per-file 3-s throttle
        // still triggered up to 50K full recomputations during a long
        // batch — `regenerate()` walks every file in the library, so
        // even at 3-s spacing it caused UI stutters and slowed the
        // engine. Terminal-only is the right granularity: Deep Analyze
        // is a single batch run, and proposals don't need to track
        // mid-flight progress.
        .onChange(of: engine.deepAnalyzeComplete?.processed ?? -1) { _, _ in
            guard libraryRoot != nil else { return }
            store.notifyChanged()
            Task { await regenerate() }
        }
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

    /// At-a-glance "what will happen" card. Plain English with action
    /// verbs ("staying put", "being tidied", "being reorganized")
    /// instead of the previous data-analyst language ("anchor",
    /// "outliers extracted", "dissolved"). Lead with the outcome the
    /// user cares about, then the count + concrete description.
    private var assistantSummaryCard: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 14) {
                HStack(spacing: 8) {
                    Image(systemName: "sparkles")
                        .foregroundStyle(Theme.gold)
                        .font(.title3)
                    VStack(alignment: .leading, spacing: 1) {
                        Text("Here's what will happen").font(.headline)
                        Text("FileID looked at every folder and decided what to do with each one. Nothing moves until you click Apply.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer()
                }
                Divider().opacity(0.3)
                if summary.anchorFolders > 0 {
                    outcomeRow(
                        icon: "lock.fill", tint: .green,
                        headline: stayingPutHeadline,
                        body: "These folders already have clear names and the right contents — FileID won't touch them."
                    )
                }
                if summary.mixedFolders > 0 {
                    outcomeRow(
                        icon: "tray.and.arrow.up.fill", tint: .orange,
                        headline: tidyingHeadline,
                        body: "Mostly-organized folders that have a few files that don't fit the folder's theme. The folder stays; the misplaced files move to where they belong."
                    )
                }
                if summary.junkFolders > 0 {
                    outcomeRow(
                        icon: "arrow.triangle.branch", tint: Theme.gold,
                        headline: reorganizingHeadline,
                        body: "Folders with generic names like \"Untitled\" or \"Camera Roll\" — FileID will sort their files into clear categories: People, Places, Documents, or Photos by year."
                    )
                }
            }
        }
    }

    private var stayingPutHeadline: String {
        let f = summary.anchorFolders
        return "Staying put: \(f) folder\(f == 1 ? "" : "s")"
    }
    private var tidyingHeadline: String {
        let f = summary.mixedFolders
        let m = summary.movedOutFiles
        return "Being tidied: \(f) folder\(f == 1 ? "" : "s") (moving \(m) misplaced file\(m == 1 ? "" : "s") out)"
    }
    private var reorganizingHeadline: String {
        let f = summary.junkFolders
        let d = summary.dissolvedFiles
        return "Being reorganized: \(f) folder\(f == 1 ? "" : "s") (\(d) file\(d == 1 ? "" : "s") will be re-sorted)"
    }

    /// Single outcome row in the assistant summary. Bigger headline,
    /// secondary body text wrapped to its own line — gives each
    /// outcome enough room to read like a sentence, not a label.
    @ViewBuilder
    private func outcomeRow(icon: String, tint: Color,
                              headline: String, body: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: icon)
                .foregroundStyle(tint)
                .font(.title3)
                .frame(width: 22, alignment: .center)
                .padding(.top, 2)
            VStack(alignment: .leading, spacing: 2) {
                Text(headline).font(.callout.bold())
                Text(body)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
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
    @ViewBuilder
    private func staysPutRow(_ entry: (folder: String, count: Int)) -> some View {
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
                            staysPutRow(entry)
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

    private var actionsBar: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                // Selection summary + bulk select/clear (left), apply
                // CTA (right).
                HStack(alignment: .center, spacing: 8) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text("\(selectedIDs.count) of \(proposals.count) file\(proposals.count == 1 ? "" : "s") selected")
                            .font(.callout.bold())
                        Text("Tap any row below to include or skip it.")
                            .font(.caption2).foregroundStyle(.tertiary)
                    }
                    Spacer()
                    Button("Select all") { selectedIDs = Set(proposals.map(\.fileID)) }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    Button("Clear") { selectedIDs.removeAll() }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                }
                Divider().opacity(0.3)
                // Two-step apply workflow — labeled so the user
                // understands shortcuts come first (safe preview),
                // then optionally a real-move commit. Both buttons
                // sit on the same row but the secondary is visually
                // de-emphasized.
                HStack(alignment: .center, spacing: 12) {
                    stepBadge(number: "1", tint: Theme.gold)
                    Button {
                        applySelected(mode: .symlink)
                    } label: {
                        Label("Apply as shortcuts (\(selectedIDs.count))",
                              systemImage: "link")
                            .padding(.horizontal, 14).padding(.vertical, 7)
                            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.gold))
                            .foregroundStyle(.black)
                            .font(.callout.bold())
                    }
                    .buttonStyle(.plain)
                    .disabled(selectedIDs.isEmpty)
                    .scaleEffect(hasPulsed ? 1.0 : (proposals.isEmpty ? 1.0 : 1.04))
                    .animation(.spring(response: 0.45, dampingFraction: 0.55), value: hasPulsed)
                    .onChange(of: proposals.isEmpty) { _, isEmpty in
                        // Single subtle pulse the first time proposals
                        // arrive — signals "ready to act" without the
                        // showy, repeating bounce that wears thin.
                        if !isEmpty && !hasPulsed {
                            withAnimation(.spring(response: 0.45, dampingFraction: 0.55)) {
                                hasPulsed = false
                            }
                            DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) {
                                hasPulsed = true
                            }
                        }
                    }
                    .help("Creates shortcuts at the new paths pointing back to the original files. Originals stay put — fully reversible by deleting the shortcuts.")
                    Text("Safe preview — originals don't move.")
                        .font(.caption2).foregroundStyle(.secondary)
                    Spacer()
                }
                HStack(alignment: .center, spacing: 12) {
                    stepBadge(number: "2", tint: .secondary)
                    Button {
                        confirmConvertToRealMoves = true
                    } label: {
                        Label("Convert to real moves",
                              systemImage: "arrow.triangle.swap")
                            .padding(.horizontal, 14).padding(.vertical, 7)
                            .background(RoundedRectangle(cornerRadius: 8)
                                .stroke(Theme.gold.opacity(0.6), lineWidth: 1))
                            .foregroundStyle(Theme.gold)
                            .font(.callout)
                    }
                    .buttonStyle(.plain)
                    .help("Once the structure looks right, this replaces every shortcut with a real on-disk move. Not reversible in-app.")
                    .confirmationDialog(
                        "Convert all shortcuts to real moves?",
                        isPresented: $confirmConvertToRealMoves,
                        titleVisibility: .visible
                    ) {
                        Button("Convert to real moves", role: .destructive) {
                            convertAllToRealMoves()
                        }
                        Button("Cancel", role: .cancel) { }
                    } message: {
                        Text("Every shortcut in the new tree will be replaced with the actual file moved into place. This isn't reversible inside the app — only do this once you've reviewed the structure.")
                    }
                    Text("After you're happy with the preview.")
                        .font(.caption2).foregroundStyle(.secondary)
                    Spacer()
                }
            }
        }
    }

    /// Small numbered circle used to label the two apply steps. Visual
    /// affordance for "do this first, then this" workflows.
    @ViewBuilder
    private func stepBadge(number: String, tint: Color) -> some View {
        Text(number)
            .font(.caption.bold().monospacedDigit())
            .foregroundStyle(tint == .secondary ? Color.secondary : .black)
            .frame(width: 20, height: 20)
            .background(Circle().fill(tint == .secondary ? Color.secondary.opacity(0.15) : tint))
    }

    // MARK: - Before / after visualization

    /// Single-column flow card. Each row is one of your current
    /// folders, and the row tells the WHOLE story for that folder:
    /// how many files are staying, how many are moving, and where
    /// they're going (with destination chips). Replaces the previous
    /// two-column "Today | arrow | Proposed" layout — that visual
    /// looked tidy but never connected sources to destinations, so a
    /// user couldn't actually trace where any one folder's files
    /// would land.
    private var beforeAfterCard: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 6) {
                    Image(systemName: "arrow.triangle.2.circlepath")
                        .foregroundStyle(Theme.gold)
                    Text("Folder map").font(.headline)
                    Spacer()
                    Text("\(flowRows.count) folder\(flowRows.count == 1 ? "" : "s") in your library")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }
                // Legend — three states a folder can be in, with the
                // exact icon + color used in the rows below. Removes
                // the "what does the green dot mean?" question.
                legendStrip
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(flowRows.enumerated()), id: \.element.id) { idx, row in
                        if idx > 0 { Divider().opacity(0.18) }
                        flowRowView(row)
                    }
                }
                .padding(.top, 4)
            }
        }
    }

    /// Per-source-folder flow record: total file count, kind, how
    /// many are staying inside this folder, and the breakdown of
    /// destinations the rest are going to.
    private struct FlowRow: Identifiable {
        let id: String
        let folder: String
        let totalFiles: Int
        let kind: BeforeKind
        let staying: Int
        let destinations: [(bucket: String, count: Int)]
    }

    /// Compute one FlowRow per current folder. Anchor folders show
    /// "all staying"; mixed folders show the staying count + each
    /// destination; junk folders show only destinations (everything
    /// is leaving).
    private var flowRows: [FlowRow] {
        // Anchor folders — directly from the staysPutBreakdown.
        var rows: [FlowRow] = summary.staysPutBreakdown.map { entry in
            FlowRow(id: "anchor:\(entry.folder)",
                    folder: entry.folder,
                    totalFiles: entry.count,
                    kind: .anchor,
                    staying: entry.count,
                    destinations: [])
        }
        // Mixed / junk folders — group proposals by sourceFolder, then
        // within each, group by bucket. Display name is the last path
        // component of sourceFolder.
        let bySource = Dictionary(grouping: proposals, by: { $0.sourceFolder })
        for (sourceFolder, sourceProposals) in bySource {
            let display = (sourceFolder as NSString).lastPathComponent
            // Re-group by destination bucket, keep the most-files-first.
            let byBucket = Dictionary(grouping: sourceProposals, by: { $0.bucket })
            let destinations = byBucket
                .map { (bucket: $0.key, count: $0.value.count) }
                .sorted { $0.count > $1.count }
            // Junk if all proposals from this folder are .dissolved.
            let isJunk = sourceProposals.allSatisfy { $0.kind == .dissolved }
            let kind: BeforeKind = isJunk ? .junk : .mixed
            // For mixed folders, "staying" is the difference between
            // the folder's total file count today and the number of
            // outliers leaving. We don't have a global file count
            // per source folder here, so use the proposal count as
            // the moving count and treat the rest of the folder as
            // staying (anchor entries already captured those).
            let movingCount = sourceProposals.count
            // For mixed folders, files NOT in proposals stay put.
            // We don't have that count directly; show the moving
            // count + label "moving out" as the action verb.
            rows.append(FlowRow(
                id: "src:\(sourceFolder)",
                folder: display.isEmpty ? sourceFolder : display,
                totalFiles: movingCount,
                kind: kind,
                staying: 0,
                destinations: destinations
            ))
        }
        return rows.sorted { (a, b) -> Bool in
            // Anchors first (the user knows these are safe), then
            // mixed, then junk. Within each kind, alphabetical.
            let order: [BeforeKind: Int] = [.anchor: 0, .mixed: 1, .junk: 2]
            if order[a.kind, default: 0] != order[b.kind, default: 0] {
                return order[a.kind, default: 0] < order[b.kind, default: 0]
            }
            return a.folder.localizedCaseInsensitiveCompare(b.folder) == .orderedAscending
        }
    }

    /// Render one source folder's flow as a single self-contained
    /// row: header line + destination chips below.
    @ViewBuilder
    private func flowRowView(_ row: FlowRow) -> some View {
        let style = beforeRowStyle(row.kind)
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Image(systemName: style.icon)
                    .foregroundStyle(style.tint)
                    .font(.callout)
                    .frame(width: 16, alignment: .center)
                VStack(alignment: .leading, spacing: 1) {
                    Text(row.folder)
                        .font(.callout.bold())
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(flowSubtitle(for: row))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 6)
            }
            if !row.destinations.isEmpty {
                // Destination chips — one per bucket files are going to.
                // Each chip carries the bucket name + count, so the user
                // can see "5 files to People/Mom, 3 to Documents" at a
                // glance without reading paragraph text. Horizontal
                // scroll handles the rare case of many destinations
                // without breaking the row layout.
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(Array(row.destinations.enumerated()), id: \.offset) { _, dest in
                            destinationChip(bucket: dest.bucket, count: dest.count)
                        }
                    }
                }
                .padding(.leading, 24)
            }
        }
        .padding(.vertical, 4)
    }

    /// One destination pill: arrow + bucket icon + bucket name +
    /// count. Tints match the existing bucket-icon convention
    /// (gold-on-translucent gold, secondary text).
    @ViewBuilder
    private func destinationChip(bucket: String, count: Int) -> some View {
        HStack(spacing: 4) {
            Image(systemName: "arrow.right")
                .font(.caption2)
                .foregroundStyle(Theme.gold.opacity(0.6))
            Image(systemName: bucketIcon(bucket))
                .font(.caption2)
                .foregroundStyle(Theme.gold)
            Text(bucket)
                .font(.caption2.monospaced())
                .foregroundStyle(.primary)
                .lineLimit(1)
            Text("(\(count))")
                .font(.caption2.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(RoundedRectangle(cornerRadius: 6).fill(Theme.gold.opacity(0.08)))
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(Theme.gold.opacity(0.25), lineWidth: 0.5))
    }

    private func flowSubtitle(for row: FlowRow) -> String {
        switch row.kind {
        case .anchor:
            return "\(row.totalFiles) file\(row.totalFiles == 1 ? "" : "s") · staying put"
        case .mixed:
            let plural = row.totalFiles == 1 ? "" : "s"
            let dests = row.destinations.count
            return "\(row.totalFiles) file\(plural) moving out → \(dests) destination\(dests == 1 ? "" : "s")"
        case .junk:
            let plural = row.totalFiles == 1 ? "" : "s"
            return "\(row.totalFiles) file\(plural) being reorganized"
        }
    }

    /// Tiny three-chip legend showing what each color/icon means.
    /// Rendered inline at the top of the folder map so the user
    /// doesn't have to memorize the convention.
    @ViewBuilder
    private var legendStrip: some View {
        HStack(spacing: 8) {
            legendChip(icon: "lock.fill", tint: .green,
                       label: "Stays put",
                       hint: "Meaningful name + matching contents")
            legendChip(icon: "tray.and.arrow.up", tint: .orange,
                       label: "Has outliers",
                       hint: "Most files stay, a few move")
            legendChip(icon: "tray.2", tint: Theme.gold,
                       label: "Dissolves",
                       hint: "Generic name → files re-bucket")
            Spacer()
        }
    }

    @ViewBuilder
    private func legendChip(icon: String, tint: Color, label: String, hint: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: icon)
                .foregroundStyle(tint)
                .font(.caption)
            VStack(alignment: .leading, spacing: 0) {
                Text(label).font(.caption2.bold())
                Text(hint).font(.system(size: 9)).foregroundStyle(.tertiary)
            }
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(RoundedRectangle(cornerRadius: 6).fill(tint.opacity(0.06)))
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(tint.opacity(0.20), lineWidth: 0.5))
    }

    private enum BeforeKind: Hashable {
        case anchor             // stays put intact
        case mixed              // some outliers leaving
        case junk               // dissolved entirely
    }

    private func beforeRowStyle(_ kind: BeforeKind) -> (icon: String, tint: Color) {
        switch kind {
        case .anchor: return ("lock.fill",         .green)
        case .mixed:  return ("tray.and.arrow.up", .orange)
        case .junk:   return ("tray.2",            Theme.gold)
        }
    }

    /// Single unified card showing every proposed move grouped by
    /// destination folder. Files are indented under their destination
    /// folder header with a thin tree guide on the left — same visual
    /// idiom as Finder column view / VS Code's file tree, so it's
    /// immediately legible as "files inside this folder".
    private var proposalsPreviewCard: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 6) {
                    Image(systemName: "list.bullet.indent")
                        .foregroundStyle(Theme.gold)
                    Text("Per-file detail").font(.headline)
                    Spacer()
                    Text("Tap a row to include or skip it.")
                        .font(.caption2).foregroundStyle(.tertiary)
                }
                .padding(.bottom, 12)
                ForEach(Array(groups.enumerated()), id: \.element.id) { idx, g in
                    if idx > 0 {
                        Divider().opacity(0.25)
                            .padding(.vertical, 10)
                    }
                    bucketSection(g)
                }
            }
        }
    }

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

    private func bucketIcon(_ bucket: String) -> String {
        if bucket.hasPrefix("People")    { return "person.crop.circle.fill" }
        if bucket.hasPrefix("Places")    { return "mappin.circle.fill" }
        if bucket.hasPrefix("Documents") { return "doc.text.fill" }
        if bucket.hasPrefix("Photos")    { return "photo.stack.fill" }
        return "tray.fill"
    }

    // MARK: - Compute

    private func regenerate() async {
        loading = true
        defer { loading = false }
        status = nil
        let root = libraryRoot
        let result = await Task.detached(priority: .userInitiated) {
            return RestructureEngine.compute(store: store, libraryRoot: root)
        }.value
        proposals = result.proposals
        summary = result.summary
        // Bucketed for display, sorted by bucket size descending.
        let by = Dictionary(grouping: result.proposals, by: { $0.bucket })
        groups = by.map { Group(bucket: $0.key, proposals: $0.value) }
            .sorted { $0.proposals.count > $1.proposals.count }
        selectedIDs = Set(result.proposals.map(\.fileID))   // default: select all moves
    }

    private func applySelected(mode: RestructureEngine.ApplyMode) {
        let toMove = proposals.filter { selectedIDs.contains($0.fileID) }
        Task {
            let result = await RestructureEngine.apply(proposals: toMove,
                                                       store: store, mode: mode)
            let modeLabel = mode == .symlink ? "linked" : "moved"
            status = "\(result.moved) \(modeLabel) · skipped \(result.skipped) · failed \(result.failed)"
                + (result.conflicts.isEmpty ? "" : " · \(result.conflicts.count) conflicts")
            await regenerate()
        }
    }

    private func convertAllToRealMoves() {
        let candidates = proposals
        Task {
            let result = await RestructureEngine.convertSymlinksToMoves(
                proposals: candidates, store: store
            )
            status = "Converted \(result.moved) shortcuts to real moves · skipped \(result.skipped) · failed \(result.failed)"
            await regenerate()
        }
    }

    // MARK: - V7 view-mode toggle + Sankey + recommendations

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

    /// Sankey-style "where is everything going?" flow diagram.
    private var sankeyCard: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 6) {
                    Image(systemName: "arrow.triangle.branch")
                        .foregroundStyle(Theme.gold)
                    Text("Folder map").font(.headline)
                    Spacer()
                    Text(sankeyHeaderStat)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }
                Text("Each ribbon shows files moving from a current folder (left) to a destination (right). Hover a ribbon to follow it; tap any folder to see the exact files.")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
                SankeyFlowView(
                    proposals: proposals,
                    onTapSource: { folder in
                        // Find the original sourceFolder full path that
                        // matched this display name, then scope the sheet.
                        let match = proposals.first(where: {
                            ($0.sourceFolder as NSString).lastPathComponent == folder
                        })?.sourceFolder ?? folder
                        drillDown = .sourceFolder(match)
                    },
                    onTapDestination: { bucket in
                        drillDown = .destBucket(bucket)
                    },
                    hoverBus: hoverBus
                )
            }
        }
    }

    private var sankeyHeaderStat: String {
        let srcCount = Set(proposals.map(\.sourceFolder)).count
        let dstCount = Set(proposals.map(\.bucket)).count
        return "\(srcCount) source\(srcCount == 1 ? "" : "s") → \(dstCount) destination\(dstCount == 1 ? "" : "s")"
    }

    /// Stack of recommendation cards — one per outcome class.
    /// Spacing bumped to 14pt + per-card outer shadow so each card
    /// reads as its own surface even when the .ultraThinMaterial blur
    /// of an adjacent card runs close. Skipped cards keep 0.55 opacity
    /// so the user can still see what they passed on.
    @ViewBuilder
    private var recommendationsStack: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 6) {
                Image(systemName: "sparkles")
                    .foregroundStyle(Theme.gold)
                Text("Recommendations").font(.headline)
                Spacer()
                Text("Tap Skip to exclude a group from the next apply.")
                    .font(.caption2).foregroundStyle(.tertiary)
            }
            if summary.anchorFolders > 0 {
                RecommendationCard(
                    outcome: .keep,
                    headline: "Keep \(summary.anchorFolders) folder\(summary.anchorFolders == 1 ? "" : "s") untouched",
                    bodyText: "These folders already have clear names and matching contents. \(summary.staysPutFiles) file\(summary.staysPutFiles == 1 ? "" : "s") staying exactly where they are.",
                    fileCount: summary.staysPutFiles,
                    folderCount: summary.anchorFolders,
                    isApproved: true,
                    isInformational: true,
                    isHighlighted: hoverBus.touchesOutcome(.keep),
                    onHover: { hovering in
                        hoverBus.set(hovering ? .outcome(.keep) : nil)
                    },
                    expandedContent: { EmptyView() }
                )
            }
            if summary.mixedFolders > 0 {
                let approved = !skippedOutcomes.contains(.tidy)
                let expanded = expandedOutcome == .tidy
                RecommendationCard(
                    outcome: .tidy,
                    headline: "Tidy \(summary.mixedFolders) folder\(summary.mixedFolders == 1 ? "" : "s") — move \(summary.movedOutFiles) misplaced file\(summary.movedOutFiles == 1 ? "" : "s")",
                    bodyText: "Mostly-organized folders with a few files that don't fit the theme. The folder stays; the misplaced files go to where they belong.",
                    fileCount: summary.movedOutFiles,
                    folderCount: summary.mixedFolders,
                    isApproved: approved,
                    isExpanded: expanded,
                    isHighlighted: hoverBus.touchesOutcome(.tidy),
                    onToggleApproval: { toggleSkip(.tidy) },
                    onToggleExpand: { toggleExpand(.tidy) },
                    onHover: { hovering in
                        hoverBus.set(hovering ? .outcome(.tidy) : nil)
                    },
                    expandedContent: { inlineFileList(for: .outcome(.tidy)) }
                )
            }
            if summary.junkFolders > 0 {
                let approved = !skippedOutcomes.contains(.reorganize)
                let expanded = expandedOutcome == .reorganize
                RecommendationCard(
                    outcome: .reorganize,
                    headline: "Reorganize \(summary.junkFolders) folder\(summary.junkFolders == 1 ? "" : "s") — sort \(summary.dissolvedFiles) file\(summary.dissolvedFiles == 1 ? "" : "s")",
                    bodyText: "Folders with generic names like \"Untitled\" or \"Camera Roll\" — files will be sorted into clear categories: People, Places, Documents, or Photos by year.",
                    fileCount: summary.dissolvedFiles,
                    folderCount: summary.junkFolders,
                    isApproved: approved,
                    isExpanded: expanded,
                    isHighlighted: hoverBus.touchesOutcome(.reorganize),
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

    private func toggleExpand(_ outcome: RestructureOutcome) {
        withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
            if expandedOutcome == outcome {
                expandedOutcome = nil
            } else {
                expandedOutcome = outcome
            }
        }
    }

    /// Inline file-list view for a recommendation card. Shows the
    /// first few buckets of files affected. Capped at ~30 rows; a
    /// "See all in detail" button still opens the full sheet for
    /// deep review.
    @ViewBuilder
    private func inlineFileList(for scope: DrillDownScope) -> some View {
        let matched = proposals.filter { matches(scope, $0) }
        let byBucket = Dictionary(grouping: matched, by: { $0.bucket })
        let bucketOrder = byBucket.keys.sorted {
            (byBucket[$0]?.count ?? 0) > (byBucket[$1]?.count ?? 0)
        }
        let totalCap = 30
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(bucketOrder.prefix(4).enumerated()), id: \.element) { idx, bucket in
                if idx > 0 { Divider().opacity(0.18) }
                let g = Group(bucket: bucket, proposals: Array((byBucket[bucket] ?? []).prefix(totalCap)))
                bucketSection(g)
            }
            if matched.count > totalCap {
                HStack {
                    Spacer()
                    Button {
                        drillDown = scope
                    } label: {
                        Label("See all \(matched.count) files in detail",
                              systemImage: "arrow.up.right.square")
                            .font(.caption.bold())
                    }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.secondary)
                }
                .padding(.top, 4)
            }
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
        let matched = proposals.filter { matches(scope, $0) }
        let title = drillDownTitle(scope, count: matched.count)
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: drillDownIcon(scope))
                    .foregroundStyle(Theme.gold)
                    .font(.title3)
                VStack(alignment: .leading, spacing: 1) {
                    Text(title).font(.headline)
                    Text("\(matched.count) file\(matched.count == 1 ? "" : "s") affected. Tap any row to include or skip it before applying.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done") { drillDown = nil }
                    .keyboardShortcut(.defaultAction)
            }
            Divider().opacity(0.3)
            if matched.isEmpty {
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
                        // Group the filtered proposals by destination
                        // bucket so the user sees "files going to X"
                        // structure even inside the scoped view. Inside
                        // the drill-down we render `unlimited: true` so
                        // big buckets aren't truncated — the LazyVStack
                        // keeps it cheap.
                        let byBucket = Dictionary(grouping: matched, by: { $0.bucket })
                        let bucketOrder = byBucket.keys.sorted {
                            (byBucket[$0]?.count ?? 0) > (byBucket[$1]?.count ?? 0)
                        }
                        ForEach(Array(bucketOrder.enumerated()), id: \.element) { idx, bucket in
                            if idx > 0 { Divider().opacity(0.18) }
                            let g = Group(bucket: bucket, proposals: byBucket[bucket] ?? [])
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

    private func matches(_ scope: DrillDownScope, _ p: Proposal) -> Bool {
        switch scope {
        case .all:                       return true
        case .outcome(let o):            return outcomeFor(p) == o
        case .sourceFolder(let f):       return p.sourceFolder == f
        case .destBucket(let b):         return p.bucket == b
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
        }
    }

    private func drillDownIcon(_ scope: DrillDownScope) -> String {
        switch scope {
        case .all:                       return "list.bullet.indent"
        case .outcome(let o):            return o.icon
        case .sourceFolder:              return "folder.fill"
        case .destBucket:                return "tray.and.arrow.down.fill"
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

            // Compute the destination via the existing heuristic.
            let bucket = Self.bucketForFile(f, nameMap: nameMap, cal: cal)
            let oldURL = f.url
            let ext = oldURL.pathExtension
            let baseName: String
            if let p = f.vlmProposedName, !p.isEmpty {
                baseName = ext.isEmpty ? p : "\(p).\(ext)"
            } else {
                baseName = oldURL.lastPathComponent
            }
            let target = root
                .appendingPathComponent(bucket, isDirectory: true)
                .appendingPathComponent(baseName)
            // Skip if the destination is identical to the source (e.g. file
            // already lives where the heuristic would put it). Counts as
            // staysPut for summary purposes.
            guard target.path != f.pathText else {
                if proposalKind == .movedOutAsOutlier { movedOut -= 1; staysPut += 1 }
                if proposalKind == .dissolved        { dissolved -= 1; staysPut += 1 }
                continue
            }
            out.append(RestructureView.Proposal(
                fileID: f.id,
                oldPath: f.pathText,
                newPath: target.path,
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
    /// each file and rewrites its path_text row.
    static func apply(proposals: [RestructureView.Proposal],
                       store: ReadStore, mode: ApplyMode) async -> ApplyResult {
        let fm = FileManager.default
        var moved = 0
        var skipped = 0
        var failed = 0
        var conflicts: [String] = []
        var pathUpdates: [(Int64, String)] = []
        for p in proposals {
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let newURL = URL(fileURLWithPath: p.newPath)
            if oldURL == newURL { skipped += 1; continue }
            do {
                try fm.createDirectory(at: newURL.deletingLastPathComponent(),
                                        withIntermediateDirectories: true)
            } catch {
                failed += 1; continue
            }
            if fm.fileExists(atPath: newURL.path) {
                conflicts.append(p.newPath); skipped += 1; continue
            }
            do {
                switch mode {
                case .symlink:
                    try fm.createSymbolicLink(at: newURL, withDestinationURL: oldURL)
                case .realMove:
                    try fm.moveItem(at: oldURL, to: newURL)
                    pathUpdates.append((p.fileID, newURL.path))
                }
                moved += 1
            } catch {
                failed += 1
            }
        }
        if !pathUpdates.isEmpty {
            await store.updatePathTexts(pathUpdates)
        }
        return ApplyResult(moved: moved, skipped: skipped, failed: failed,
                           conflicts: conflicts, mode: mode)
    }

    /// For each proposal whose newPath is a symlink → original, replace
    /// it with a real move and update the DB.
    static func convertSymlinksToMoves(proposals: [RestructureView.Proposal],
                                        store: ReadStore) async -> ApplyResult {
        let fm = FileManager.default
        var moved = 0
        var skipped = 0
        var failed = 0
        var pathUpdates: [(Int64, String)] = []
        for p in proposals {
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let newURL = URL(fileURLWithPath: p.newPath)
            // Skip if newPath isn't a symlink (hand-edited tree).
            guard let attrs = try? fm.attributesOfItem(atPath: newURL.path),
                  let type = attrs[.type] as? FileAttributeType,
                  type == .typeSymbolicLink else {
                skipped += 1; continue
            }
            do {
                try fm.removeItem(at: newURL)
                try fm.moveItem(at: oldURL, to: newURL)
                moved += 1
                pathUpdates.append((p.fileID, newURL.path))
            } catch {
                failed += 1
            }
        }
        if !pathUpdates.isEmpty {
            await store.updatePathTexts(pathUpdates)
        }
        return ApplyResult(moved: moved, skipped: skipped, failed: failed,
                           conflicts: [], mode: .realMove)
    }
}
