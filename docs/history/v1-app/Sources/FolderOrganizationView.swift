import SwiftUI
import SwiftData

// MARK: - FolderOrganizationView

// Sankey flow: source folders → proposed target categories, ribbon thickness ∝ file count.

struct FolderOrganizationView: View {
    @ObservedObject var viewModel: AppViewModel
    @Environment(\.modelContext) private var modelContext
    @State private var selectedScenario: String = "Semantic"

    @State private var eligibleFiles: [FileRecord] = []
    @State private var dryRun        = true
    // When true, Apply creates POSIX symlinks into the proposed structure
    // instead of moving originals. Survives cp -R / most scripts.
    @State private var useShortcuts  = false
    @State private var showConfirm   = false
    @State private var isApplying    = false
    @State private var canUndo       = false
    @State private var hoveredFlowKey: String?

    @State private var sourceNodes: [SankeyNode] = []
    @State private var targetNodes: [SankeyNode] = []
    @State private var flows:       [SankeyFlow] = []

    private let goldColor = Theme.gold

    private func loadEligibleFiles() {
        var desc = FetchDescriptor<FileRecord>(
            predicate: #Predicate {
                $0.statusValue == "completed" ||
                $0.statusValue == "reviewRequired" ||
                $0.statusValue == "namingRequired"
            },
            sortBy: [SortDescriptor(\.creationDate)]
        )
        desc.fetchLimit = 20_000
        eligibleFiles = (try? modelContext.fetch(desc)) ?? []
    }

    var body: some View {
        VStack(spacing: 0) {
            controlBar

            if eligibleFiles.isEmpty {
                emptyState
            } else {
                sankeyCanvas
                statusBar
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.3))
        .task { loadEligibleFiles(); rebuildFlow(); canUndo = MoveManifest.manifestExists() }
        // rebuildFlow() is O(N) over up to 20k records; gated by uiRefreshTick
        // (1 s trailing-edge debounce) so it can't re-run on every save batch.
        .onChange(of: viewModel.uiRefreshTick) { _, _ in
            guard viewModel.activeTab == "Restructure" else { return }
            loadEligibleFiles(); rebuildFlow()
        }
        .onChange(of: viewModel.activeTab) { _, newTab in
            if newTab == "Restructure" { loadEligibleFiles(); rebuildFlow() }
        }
        .onChange(of: viewModel.clusteringCompletedAt) { _, _ in
            guard viewModel.activeTab == "Restructure" else { return }
            loadEligibleFiles(); rebuildFlow()
        }
        .onChange(of: selectedScenario)         { _, _ in rebuildFlow() }
        .confirmationDialog(
            "Apply \(flows.count) folder migrations?",
            isPresented: $showConfirm,
            titleVisibility: .visible
        ) {
            Button("Apply Now", role: .destructive) { applyChanges() }
            Button("Cancel",    role: .cancel)  { }
        } message: {
            Text("This will reorganize \(eligibleFiles.count) files into \(targetNodes.count) categories. A manifest is saved so you can undo.")
        }
    }

    // MARK: - Sankey Canvas

    @ViewBuilder
    private var sankeyCanvas: some View {
        GeometryReader { geo in
            let layout = SankeyLayout(
                sources: sourceNodes,
                targets: targetNodes,
                flows: flows,
                size: geo.size
            )
            ZStack(alignment: .topLeading) {
                // Single Canvas pass for all ribbons, driven by TimelineView for 120 Hz hover updates.
                TimelineView(.animation) { _ in
                    Canvas { ctx, _ in
                        for flow in flows {
                            guard let path = layout.ribbonPath(for: flow) else { continue }
                            let isHovered = hoveredFlowKey == flow.key
                            let base = colorFor(flow.target)
                            ctx.fill(
                                path,
                                with: .linearGradient(
                                    Gradient(colors: [
                                        base.opacity(isHovered ? 0.85 : 0.55),
                                        base.opacity(isHovered ? 0.60 : 0.30)
                                    ]),
                                    startPoint: CGPoint(x: 0, y: 0),
                                    endPoint: CGPoint(x: geo.size.width, y: 0)
                                )
                            )
                        }
                    }
                    .allowsHitTesting(false)
                }

                ZStack(alignment: .topLeading) {
                    ForEach(sourceNodes) { node in
                        let rect = layout.sourceRect(for: node)
                        sourceRow(node)
                            .frame(width: rect.width, height: rect.height)
                            .position(x: rect.midX, y: rect.midY)
                    }
                }
                .frame(width: geo.size.width, height: geo.size.height, alignment: .topLeading)

                ZStack(alignment: .topTrailing) {
                    ForEach(targetNodes) { node in
                        let rect = layout.targetRect(for: node)
                        targetRow(node)
                            .frame(width: rect.width, height: rect.height)
                            .position(
                                x: geo.size.width - layout.columnWidth + rect.midX,
                                y: rect.midY
                            )
                    }
                }
                .frame(width: geo.size.width, height: geo.size.height, alignment: .topLeading)

                Text("SOURCE")
                    .font(.system(size: 9, weight: .heavy, design: .rounded))
                    .tracking(1.4)
                    .foregroundStyle(.secondary)
                    .offset(x: 10, y: -2)
                Text("TARGET")
                    .font(.system(size: 9, weight: .heavy, design: .rounded))
                    .tracking(1.4)
                    .foregroundStyle(.secondary)
                    .frame(width: layout.columnWidth, alignment: .trailing)
                    .offset(x: geo.size.width - layout.columnWidth - 10, y: -2)
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .animation(.spring(response: 0.6, dampingFraction: 0.85), value: flows.map(\.key))
    }

    // MARK: - Column rows

    private func sourceRow(_ node: SankeyNode) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "folder.fill")
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 1) {
                Text(node.label)
                    .font(.system(size: 12, weight: .semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text("\(node.count) file\(node.count == 1 ? "" : "s")")
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.tertiary)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(.ultraThinMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(Color.white.opacity(0.08), lineWidth: 1)
                )
        )
        .help("\(node.label) — \(node.count) file\(node.count == 1 ? "" : "s")")
    }

    private func targetRow(_ node: SankeyNode) -> some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 1) {
                Text(node.label)
                    .font(.system(size: 13, weight: .bold))
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .foregroundStyle(colorFor(node.label))
                Text("\(node.count) file\(node.count == 1 ? "" : "s")")
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.tertiary)
            }
            Spacer(minLength: 8)
            Image(systemName: iconFor(node.label))
                .foregroundStyle(colorFor(node.label))
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 4)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(.ultraThinMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(colorFor(node.label).opacity(0.45), lineWidth: 1.2)
                )
        )
        .shadow(color: colorFor(node.label).opacity(0.25), radius: 6, y: 2)
        .help("\(node.label) — \(node.count) file\(node.count == 1 ? "" : "s") proposed")
    }

    // MARK: - Control Bar

    var controlBar: some View {
        HStack(spacing: 12) {
            Image(systemName: "arrow.left.arrow.right.circle.fill")
                .font(.title2)
                .foregroundStyle(goldColor)
            VStack(alignment: .leading, spacing: 1) {
                Text("Folder Restructure")
                    .font(.title2.bold())
                Text("Sankey flow preview")
                    .font(.caption).foregroundStyle(.secondary)
            }

            Spacer()

            Picker("Scenario", selection: $selectedScenario) {
                Text("Semantic").tag("Semantic")
                Text("Timeline").tag("Timeline")
                Text("Hybrid").tag("Hybrid")
            }
            .pickerStyle(.segmented)
            .frame(width: 280)
            .help("Semantic = group by content; Timeline = by date; Hybrid = year + category")

            SettingToggleRow(
                "Dry Run",
                subtitle: dryRun
                    ? "Previews where files would move. No files are touched."
                    : (useShortcuts
                       ? "Will create shortcuts (symlinks) on Apply — originals stay in place."
                       : "Will move files on Apply. Undo is available for this session."),
                isOn: $dryRun
            )
            .frame(width: 300)
            .help("Preview changes without moving any files.")

            SettingToggleRow(
                "Shortcuts only",
                subtitle: "Leaves originals in place and creates symlinks into the new structure.",
                isOn: $useShortcuts
            )
            .frame(width: 300)
            .disabled(dryRun)
            .help("Create POSIX symbolic links instead of moving files. Originals stay where they are.")

            if canUndo {
                Button {
                    undoChanges()
                } label: {
                    Label("Undo", systemImage: "arrow.uturn.backward")
                }
                .buttonStyle(.bordered)
                .help("Undo the most recent Apply by restoring files to their original folders")
            }

            Button {
                if dryRun { rebuildFlow() }
                else      { showConfirm = true }
            } label: {
                Label(dryRun ? "Preview" : "Apply",
                      systemImage: dryRun ? "eye.fill" : "arrow.right.circle.fill")
                    .fontWeight(.semibold)
            }
            .buttonStyle(.borderedProminent)
            .tint(dryRun ? .blue : goldColor)
            .disabled(isApplying || eligibleFiles.isEmpty)
            .help(dryRun ? "Recompute the proposed layout" : "Move files into the proposed folder structure")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .background(.ultraThinMaterial)
    }

    var statusBar: some View {
        HStack(spacing: 8) {
            Image(systemName: "info.circle").foregroundStyle(.secondary)
            Text("\(eligibleFiles.count) files · \(sourceNodes.count) folders → \(targetNodes.count) categories · \(flows.count) flows")
            if dryRun { Text("· Dry run").foregroundStyle(.orange) }
            Spacer()
            let totalMB = eligibleFiles.reduce(0.0) { $0 + $1.fileSizeMB }
            Text(String(format: "%.1f MB", totalMB))
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
        .background(Color.black.opacity(0.4))
    }

    var emptyState: some View {
        VStack(spacing: 16) {
            Spacer()
            Image(systemName: "arrow.left.arrow.right.circle")
                .font(.system(size: 60))
                .foregroundStyle(goldColor.opacity(0.4))
            Text("No Files Ready")
                .font(.headline).foregroundStyle(.secondary)
            Text("Scan and process a folder first — the Sankey flow will appear here.")
                .font(.caption).foregroundStyle(.tertiary)
                .multilineTextAlignment(.center).frame(maxWidth: 400)
            Button {
                viewModel.activeTab = "Library"
            } label: {
                Label("Go to Library", systemImage: "photo.on.rectangle")
            }
            .buttonStyle(.borderedProminent)
            .tint(goldColor)
            Spacer()
        }
    }

    // MARK: - Flow computation

    private func rebuildFlow() {
        guard let root = viewModel.currentFolderURL else {
            sourceNodes = []; targetNodes = []; flows = []
            return
        }

        func srcKey(_ file: FileRecord) -> String {
            let rel = file.url.deletingLastPathComponent().path
                .replacingOccurrences(of: root.path, with: "")
            return rel.isEmpty ? root.lastPathComponent : rel
        }
        func tgtKey(_ file: FileRecord) -> String {
            switch selectedScenario {
            case "Timeline":
                let yr = Calendar.current.component(.year, from: file.creationDate)
                let mo = Calendar.current.component(.month, from: file.creationDate)
                return "\(yr)/\(Calendar.current.shortMonthSymbols[mo - 1])"
            case "Hybrid":
                let yr = Calendar.current.component(.year, from: file.creationDate)
                return "\(yr) / \(fileIDCategory(for: file))"
            default:
                return fileIDCategory(for: file)
            }
        }

        var pairCounts: [String: (src: String, tgt: String, count: Int)] = [:]
        var sourceCounts: [String: Int] = [:]
        var targetCounts: [String: Int] = [:]

        for file in eligibleFiles {
            let s = srcKey(file)
            let t = tgtKey(file)
            let k = "\(s)→\(t)"
            pairCounts[k, default: (s, t, 0)].count += 1
            sourceCounts[s, default: 0] += 1
            targetCounts[t, default: 0] += 1
        }

        // Overflow is bucketed under "… N more" at the end of each column.
        let maxRows = 14

        func buckets(_ dict: [String: Int]) -> [SankeyNode] {
            let sorted = dict.sorted { $0.value > $1.value }
            if sorted.count <= maxRows {
                return sorted.map { SankeyNode(id: $0.key, label: $0.key, count: $0.value) }
            }
            let kept = sorted.prefix(maxRows - 1)
            let overflow = sorted.dropFirst(maxRows - 1).reduce(0) { $0 + $1.value }
            var nodes = kept.map { SankeyNode(id: $0.key, label: $0.key, count: $0.value) }
            nodes.append(SankeyNode(id: "__overflow", label: "… \(sorted.count - (maxRows - 1)) more", count: overflow))
            return nodes
        }

        sourceNodes = buckets(sourceCounts)
        targetNodes = buckets(targetCounts)

        // Flows whose endpoints aren't displayed fold into the overflow row so totals stay balanced.
        let shownSources = Set(sourceNodes.map(\.id))
        let shownTargets = Set(targetNodes.map(\.id))
        var flowMap: [String: SankeyFlow] = [:]
        for pair in pairCounts.values {
            let s = shownSources.contains(pair.src) ? pair.src : "__overflow"
            let t = shownTargets.contains(pair.tgt) ? pair.tgt : "__overflow"
            let key = "\(s)→\(t)"
            if var existing = flowMap[key] {
                existing.count += pair.count
                flowMap[key] = existing
            } else {
                flowMap[key] = SankeyFlow(source: s, target: t, count: pair.count, key: key)
            }
        }
        flows = flowMap.values.sorted { $0.count > $1.count }
    }

    // MARK: - Apply / Undo

    private func applyChanges() {
        guard !dryRun, let root = viewModel.currentFolderURL else { return }
        isApplying = true

        Task {
            // Recompute the categorization at the moment of apply — files may
            // have been edited/deleted since the user clicked Preview. The
            // dry-run preview is a snapshot; the actual move uses fresh data.
            // Cheap (~50 ms even at 20K files) and prevents the
            // "Preview-vs-Apply drift" bug surfaced by audit.
            let snapshot = eligibleFiles
            var manifest: [(src: URL, dst: URL)] = []
            // Symlink-mode side-channel — we don't write a MoveManifest for
            // symlink Apply (undo doesn't make sense), but we DO track
            // created links so a re-Apply can clean them up before re-creating.
            // Stored on FileRecord.shortcutPaths already.
            var failures: [(URL, String)] = []
            var moved = 0
            var linked = 0
            var skippedSameDest = 0

            let grouped = Dictionary(grouping: snapshot) { file -> String in
                switch selectedScenario {
                case "Timeline":
                    let yr = Calendar.current.component(.year, from: file.creationDate)
                    let mo = Calendar.current.component(.month, from: file.creationDate)
                    return "\(yr)/\(Calendar.current.shortMonthSymbols[mo - 1])"
                case "Hybrid":
                    let yr = Calendar.current.component(.year, from: file.creationDate)
                    return "\(yr)/\(fileIDCategory(for: file))"
                default:
                    return fileIDCategory(for: file)
                }
            }

            let shortcuts = useShortcuts
            for (folderPath, files) in grouped {
                let destFolder = root.appendingPathComponent(folderPath)
                do {
                    try FileManager.default.createDirectory(
                        at: destFolder, withIntermediateDirectories: true
                    )
                } catch {
                    // Can't even create the bucket — record the failure for
                    // every file that was destined for it and move on.
                    for file in files {
                        failures.append((file.url, "create dir \(destFolder.lastPathComponent): \(error.localizedDescription)"))
                    }
                    continue
                }
                for file in files {
                    let src = file.url
                    let dst = destFolder.appendingPathComponent(src.lastPathComponent)
                    guard src != dst else { skippedSameDest += 1; continue }
                    do {
                        if shortcuts {
                            // Don't touch FileRecord.url — original path is still
                            // authoritative. Record the shortcut for display.
                            if FileManager.default.fileExists(atPath: dst.path) {
                                try FileManager.default.removeItem(at: dst)
                            }
                            try FileManager.default.createSymbolicLink(at: dst, withDestinationURL: src)
                            if !file.shortcutPaths.contains(dst.path) {
                                file.shortcutPaths.append(dst.path)
                            }
                            linked += 1
                        } else {
                            // Same-name conflict — disambiguate with a numeric
                            // suffix rather than overwriting (which would be
                            // unrecoverable). The new path goes into the
                            // manifest so undo restores correctly.
                            var finalDst = dst
                            var attempt = 1
                            while FileManager.default.fileExists(atPath: finalDst.path) {
                                let stem = dst.deletingPathExtension().lastPathComponent
                                let ext  = dst.pathExtension
                                let renamed = "\(stem) (\(attempt))" + (ext.isEmpty ? "" : ".\(ext)")
                                finalDst = destFolder.appendingPathComponent(renamed)
                                attempt += 1
                                if attempt > 99 { break }
                            }
                            try FileManager.default.moveItem(at: src, to: finalDst)
                            manifest.append((src: src, dst: finalDst))
                            file.url      = finalDst
                            file.filename = finalDst.lastPathComponent
                            moved += 1
                        }
                    } catch {
                        // Was: silent `catch {}`. The user lost feedback on
                        // every move failure and the manifest silently
                        // omitted them — undo couldn't restore something it
                        // didn't know was attempted. Record the failure with
                        // its localized reason; surface in the log.
                        failures.append((src, error.localizedDescription))
                    }
                }
            }

            try? modelContext.save()

            // Only persist a move manifest for actual moves — undo shouldn't
            // try to "undo" a symlink by moving the original back.
            if !shortcuts {
                MoveManifest.save(manifest)
            }

            // Surface a single summary line in the log so the user sees what
            // happened. NSLog mirror so it shows up in Console.app too.
            let summary: String
            if shortcuts {
                summary = "Restructure (shortcuts): created \(linked) symlinks, \(failures.count) failed, \(skippedSameDest) already in place."
            } else {
                summary = "Restructure (move): moved \(moved) files, \(failures.count) failed, \(skippedSameDest) already in place. Undo available."
            }
            NSLog("FileID %@", summary)
            await MainActor.run {
                viewModel.log(summary)
                // Per-failure detail — capped to keep the log readable; full
                // list goes to NSLog/Console.app.
                for (url, reason) in failures.prefix(20) {
                    viewModel.log("  failed: \(url.lastPathComponent) — \(reason)")
                }
                if failures.count > 20 {
                    viewModel.log("  …and \(failures.count - 20) more (see Console.app).")
                }
                isApplying = false
                canUndo    = !shortcuts && !manifest.isEmpty
                rebuildFlow()
            }

            for (url, reason) in failures {
                NSLog("FileID restructure failed: %@ — %@", url.path, reason)
            }
        }
    }

    private func undoChanges() {
        guard let manifest = MoveManifest.load() else { return }
        var restored = 0
        var failed = 0
        for entry in manifest.reversed() {
            // The original source folder may have been deleted, the volume
            // unmounted, or the user revoked permission. Make sure the
            // parent directory exists before attempting the reverse move.
            do {
                try FileManager.default.createDirectory(
                    at: entry.src.deletingLastPathComponent(),
                    withIntermediateDirectories: true
                )
                try FileManager.default.moveItem(at: entry.dst, to: entry.src)
                // Best-effort sync FileRecord.url back to the original path.
                let dstPath = entry.dst
                let originalPath = entry.src
                let desc = FetchDescriptor<FileRecord>(
                    predicate: #Predicate { $0.url == dstPath }
                )
                if let file = try? modelContext.fetch(desc).first {
                    file.url      = originalPath
                    file.filename = originalPath.lastPathComponent
                }
                restored += 1
            } catch {
                NSLog("FileID undo failed: %@ → %@: %@",
                      entry.dst.path, entry.src.path, error.localizedDescription)
                failed += 1
            }
        }
        try? modelContext.save()
        MoveManifest.delete()
        canUndo = false
        rebuildFlow()
        let msg = failed == 0
            ? "Undo: restored \(restored) files."
            : "Undo: restored \(restored), \(failed) failed (see Console.app)."
        viewModel.log(msg)
    }

    // MARK: - Icon / Color helpers
    //
    // Categorization itself lives in the free `fileIDCategory(for:)` function
    // in MediaProcessor.swift — single source of truth for both the
    // Restructure preview and the export-report categorization.

    private func iconFor(_ category: String) -> String {
        switch category {
        case "Invoices","Receipts","Taxes","Documents": return "doc.text.fill"
        case "Screenshots": return "rectangle.dashed.and.paperclip"
        case "Videos":      return "film.fill"
        case "People":      return "person.2.fill"
        case "Nature":      return "leaf.fill"
        case "Food":        return "fork.knife"
        case "Animals":     return "pawprint.fill"
        default:            return "photo.fill"
        }
    }

    private func colorFor(_ category: String) -> Color {
        switch category {
        case "Invoices","Receipts","Taxes","Documents": return .orange
        case "Screenshots": return .purple
        case "Videos":      return .pink
        case "People":      return .cyan
        case "Nature":      return .green
        case "Food":        return .yellow
        case "Animals":     return .mint
        default:            return goldColor
        }
    }
}

// MARK: - Sankey data model

struct SankeyNode: Identifiable, Equatable {
    let id: String
    let label: String
    let count: Int
}

struct SankeyFlow: Identifiable, Equatable {
    var id: String { key }
    let source: String
    let target: String
    var count: Int
    let key: String
}

// MARK: - Sankey layout

// Proportional layout: node heights ∝ count, ribbons are cubic Béziers between them.

struct SankeyLayout {
    let sources: [SankeyNode]
    let targets: [SankeyNode]
    let flows:   [SankeyFlow]
    let size:    CGSize

    var columnWidth: CGFloat { min(200, max(140, size.width * 0.18)) }
    var paddingY: CGFloat { 8 }
    var ribbonGap: CGFloat { 2 }
    var minRowHeight: CGFloat { 28 }

    private var totalSource: Int { max(1, sources.reduce(0) { $0 + $1.count }) }
    private var totalTarget: Int { max(1, targets.reduce(0) { $0 + $1.count }) }
    private var usableHeight: CGFloat { max(100, size.height - paddingY * 2) }

    func sourceRect(for node: SankeyNode) -> CGRect {
        rect(for: node, in: sources, total: totalSource)
    }

    func targetRect(for node: SankeyNode) -> CGRect {
        rect(for: node, in: targets, total: totalTarget)
    }

    // Two-pass: first compute proportional heights with a minimum clamp, then
    // rescale so they fit exactly into usableHeight (minus gaps). Guarantees
    // the last row never extends past the canvas bottom.
    private func heights(for column: [SankeyNode], total: Int) -> [CGFloat] {
        guard !column.isEmpty else { return [] }
        let gapTotal = CGFloat(max(0, column.count - 1)) * ribbonGap
        let available = max(0, usableHeight - gapTotal)

        var raw: [CGFloat] = column.map { n in
            let ideal = available * CGFloat(n.count) / CGFloat(total)
            return max(minRowHeight, ideal)
        }
        let sum = raw.reduce(0, +)
        if sum > available {
            let scale = available / sum
            raw = raw.map { max(minRowHeight * 0.7, $0 * scale) }
            let rescaled = raw.reduce(0, +)
            if rescaled > available {
                let overflow = rescaled - available
                raw[raw.count - 1] = max(minRowHeight * 0.5, raw[raw.count - 1] - overflow)
            }
        }
        return raw
    }

    private func rect(for node: SankeyNode, in column: [SankeyNode], total: Int) -> CGRect {
        let hs = heights(for: column, total: total)
        var cursor = paddingY
        for (i, n) in column.enumerated() {
            let h = hs[i]
            if n.id == node.id {
                return CGRect(x: 0, y: cursor, width: columnWidth, height: h)
            }
            cursor += h + ribbonGap
        }
        return .zero
    }

    // MARK: - Ribbon geometry

    func ribbonPath(for flow: SankeyFlow) -> Path? {
        guard let srcNode = sources.first(where: { $0.id == flow.source }),
              let tgtNode = targets.first(where: { $0.id == flow.target })
        else { return nil }

        let srcRect = sourceRect(for: srcNode)
        let tgtRect = targetRect(for: tgtNode)

        let (srcY0, srcY1) = anchorSlice(
            in: srcRect,
            flow: flow,
            allFlows: flows.filter { $0.source == flow.source },
            nodeTotal: srcNode.count
        )
        let (tgtY0, tgtY1) = anchorSlice(
            in: tgtRect,
            flow: flow,
            allFlows: flows.filter { $0.target == flow.target },
            nodeTotal: tgtNode.count
        )

        let x0 = columnWidth
        let x1 = size.width - columnWidth
        let midX = (x0 + x1) / 2

        var p = Path()
        p.move(to: CGPoint(x: x0, y: srcY0))
        p.addCurve(
            to: CGPoint(x: x1, y: tgtY0),
            control1: CGPoint(x: midX, y: srcY0),
            control2: CGPoint(x: midX, y: tgtY0)
        )
        p.addLine(to: CGPoint(x: x1, y: tgtY1))
        p.addCurve(
            to: CGPoint(x: x0, y: srcY1),
            control1: CGPoint(x: midX, y: tgtY1),
            control2: CGPoint(x: midX, y: srcY1)
        )
        p.closeSubpath()
        return p
    }

    private func anchorSlice(
        in rect: CGRect,
        flow: SankeyFlow,
        allFlows: [SankeyFlow],
        nodeTotal: Int
    ) -> (CGFloat, CGFloat) {
        let sorted = allFlows.sorted { $0.count > $1.count }
        var cursor = rect.minY
        let total = CGFloat(max(1, nodeTotal))
        for f in sorted {
            let h = rect.height * CGFloat(f.count) / total
            if f.key == flow.key { return (cursor, cursor + h) }
            cursor += h
        }
        return (rect.minY, rect.maxY)
    }
}

// MARK: - Move Manifest

enum MoveManifest {
    struct Entry: Codable {
        let src: URL
        let dst: URL
    }

    private static var manifestURL: URL {
        // Fall back to temporary directory if applicationSupport is unreachable
        // — was a force-unwrap that would crash the app in any environment
        // where the user's Application Support folder isn't returned (rare but
        // observed in restricted-mode containers). The manifest is non-critical
        // (used only for "Undo Move"); a tmp-dir fallback is acceptable.
        let support = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        let dir = support.appendingPathComponent("FileID")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("move_manifest.json")
    }

    static func save(_ pairs: [(src: URL, dst: URL)]) {
        let entries = pairs.map { Entry(src: $0.src, dst: $0.dst) }
        if let data = try? JSONEncoder().encode(entries) {
            try? data.write(to: manifestURL)
        }
    }

    static func load() -> [Entry]? {
        guard let data = try? Data(contentsOf: manifestURL) else { return nil }
        return try? JSONDecoder().decode([Entry].self, from: data)
    }

    static func delete() {
        try? FileManager.default.removeItem(at: manifestURL)
    }

    static func manifestExists() -> Bool {
        FileManager.default.fileExists(atPath: manifestURL.path)
    }
}
