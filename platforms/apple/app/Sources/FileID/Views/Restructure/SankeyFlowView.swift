import SwiftUI

/// Sankey flow diagram for Restructure: one Canvas draws every ribbon. Layout is cached in `layout`; hover is one `.onContinuousHover` that hit-tests by bezier proximity.
struct SankeyFlowView: View {
    let proposals: [RestructureView.Proposal]
    var onTapSource: (String) -> Void = { _ in }
    var onTapDestination: (String) -> Void = { _ in }
    /// Tapping a "+ N more folders" rollup scopes the drill-down to its long-tail list.
    var onTapSourceRollup: ([String]) -> Void = { _ in }
    var onTapDestRollup: ([String]) -> Void = { _ in }
    var hoverBus: RestructureHoverBus

    @State private var layout: Layout = .empty
    @State private var lastWidth: CGFloat = 0
    @State private var lastHeight: CGFloat = 0
    @State private var cursorPos: CGPoint = .zero
    @State private var cursorActive = false
    @State private var entranceProgress: CGFloat = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private static let topN = 8
    static let otherSourceID = "src:__other__"
    static let otherDestID = "dst:__other__"

    /// Okabe-Ito palette, distinguishable under common color-vision deficiencies, for destination nodes (RESTRUCTURE.md §7).
    private static let okabeIto: [Color] = [
        Color(red: 0.000, green: 0.447, blue: 0.698),   // blue      #0072B2
        Color(red: 0.835, green: 0.369, blue: 0.000),   // vermilion #D55E00
        Color(red: 0.000, green: 0.620, blue: 0.451),   // green     #009E73
        Color(red: 0.337, green: 0.706, blue: 0.914),   // sky blue  #56B4E9
        Color(red: 0.902, green: 0.624, blue: 0.000),   // orange    #E69F00
        Color(red: 0.800, green: 0.475, blue: 0.655),   // purple    #CC79A7
        Color(red: 0.941, green: 0.894, blue: 0.259),   // yellow    #F0E442
    ]

    var body: some View {
        if proposals.isEmpty {
            EmptyView()
        } else {
            VStack(spacing: 8) {
                columnHeaderRow
                diagramArea
            }
        }
    }

    /// Column headers: "FROM (N)" → "TO (M)" with an arrow.
    @ViewBuilder
    private var columnHeaderRow: some View {
        let nodeColW: CGFloat = 196
        HStack(spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "tray.and.arrow.up.fill")
                    .font(.system(size: 9))
                    .foregroundStyle(Theme.text2)
                Text("FROM")
                    .font(Theme.Font.sectionLabel)
                    .tracking(0.6)
                    .foregroundStyle(Theme.text2)
                Text("\(layout.sourceFolderCount)")
                    .font(Theme.Font.body(11.5, .semibold).monospacedDigit())
                    .foregroundStyle(Theme.text3)
            }
            .frame(width: nodeColW, alignment: .leading)
            Spacer(minLength: 0)
            HStack(spacing: 8) {
                Rectangle().fill(Color.white.opacity(0.06)).frame(height: 1)
                Image(systemName: "arrow.right")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Theme.gold.opacity(0.55))
                Rectangle().fill(Color.white.opacity(0.06)).frame(height: 1)
            }
            Spacer(minLength: 0)
            HStack(spacing: 6) {
                Text("\(layout.destinationFolderCount)")
                    .font(Theme.Font.body(11.5, .semibold).monospacedDigit())
                    .foregroundStyle(Theme.text3)
                Text("TO")
                    .font(Theme.Font.sectionLabel)
                    .tracking(0.6)
                    .foregroundStyle(Theme.text2)
                Image(systemName: "tray.and.arrow.down.fill")
                    .font(.system(size: 9))
                    .foregroundStyle(Theme.text2)
            }
            .frame(width: nodeColW, alignment: .trailing)
        }
        .padding(.horizontal, 2)
    }

    /// The ribbon canvas, node columns and tooltip overlay.
    @ViewBuilder
    private var diagramArea: some View {
        GeometryReader { geo in
            let nodeColW: CGFloat = 196
            let totalW = geo.size.width
            let totalH = geo.size.height
            let ribbonAreaX: ClosedRange<CGFloat> =
                nodeColW ... (totalW - nodeColW)

            ZStack(alignment: .topLeading) {
                // One Canvas draws every ribbon: one draw pass per render instead of ~70 Path views.
                ribbonCanvas(ribbonAreaX: ribbonAreaX, totalH: totalH)
                    .opacity(Double(entranceProgress))

                    ForEach(layout.sources) { node in
                        if let slot = layout.srcSlots[node.id] {
                            Button {
                                if node.id == Self.otherSourceID {
                                    onTapSourceRollup(layout.rollupSourceFolders)
                                } else {
                                    // Full path, not the basename, so same-named folders stay distinct.
                                    onTapSource(node.identityKey)
                                }
                            } label: {
                                nodeView(node, slot: slot, isFocused: isNodeFocused(node))
                            }
                            .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.m))
                            .accessibilityLabel(RestructureSpeech.mapFolder(direction: "From", name: node.label, files: node.count))
                            .accessibilityHint("Shows the files moving out of this folder")
                                .frame(width: nodeColW)
                                .position(x: nodeColW / 2, y: slot.midY)
                                .onHover { hovering in
                                    hoverBus.set(
                                        hovering && node.id != Self.otherSourceID
                                            ? .sourceFolder(node.identityKey)
                                            : nil
                                    )
                                }
                        }
                    }

                    ForEach(layout.destinations) { node in
                        if let slot = layout.dstSlots[node.id] {
                            Button {
                                if node.id == Self.otherDestID {
                                    onTapDestRollup(layout.rollupDestBuckets)
                                } else {
                                    onTapDestination(node.label)
                                }
                            } label: {
                                nodeView(node, slot: slot, isFocused: isNodeFocused(node))
                            }
                            .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.m))
                            .accessibilityLabel(RestructureSpeech.mapFolder(direction: "To", name: node.label, files: node.count))
                            .accessibilityHint("Shows the files moving into this folder")
                                .frame(width: nodeColW)
                                .position(x: totalW - nodeColW / 2,
                                            y: slot.midY)
                                .onHover { hovering in
                                    hoverBus.set(
                                        hovering && node.id != Self.otherDestID
                                            ? .destBucket(node.identityKey)
                                            : nil
                                    )
                                }
                        }
                    }
                }
                // One hover detector for the ribbon area; writes to the bus only when the answer changes.
                .onContinuousHover { phase in
                    handleContinuousHover(phase: phase,
                                           ribbonAreaX: ribbonAreaX,
                                           totalW: totalW)
                }
                .onAppear {
                    recomputeLayoutIfNeeded(width: totalW, height: totalH)
                }
                .onChange(of: geo.size) { _, newSize in
                    recomputeLayoutIfNeeded(width: newSize.width,
                                              height: newSize.height)
                }
                // Watches content, not count: a regenerated plan can have the same size.
                .onChange(of: proposals) { _, _ in
                    layout = Self.computeLayout(
                        proposals: proposals,
                        width: totalW,
                        height: totalH
                    )
                    // Replay entrance animation when data changes.
                    entranceProgress = 0
                    withAnimation(reduceMotion ? nil : Theme.springStandard) {
                        entranceProgress = 1
                    }
                }
                hoverTooltip(geo: geo)
            }
            .frame(height: sankeyHeight)
            .clipped()
            .onAppear {
                if entranceProgress < 1 {
                    withAnimation(reduceMotion ? nil : Theme.springStandard) {
                        entranceProgress = 1
                    }
                }
            }
    }

    // MARK: - Ribbon canvas

    /// Draws all ribbons in one Canvas; the hovered ribbon is drawn last so it overlays the others.
    @ViewBuilder
    private func ribbonCanvas(ribbonAreaX: ClosedRange<CGFloat>,
                                totalH: CGFloat) -> some View {
        let traced = layout.highlightedFlowIDs(for: hoverBus.context)
        let dimAll = !traced.isEmpty

        Canvas(opaque: false, rendersAsynchronously: false) { ctx, _ in
            for flow in layout.flows where !traced.contains(flow.id) {
                drawRibbon(
                    ctx: ctx,
                    flow: flow,
                    ribbonAreaX: ribbonAreaX,
                    state: dimAll ? .dimmed : .resting
                )
            }
            for flow in layout.flows where traced.contains(flow.id) {
                drawRibbon(
                    ctx: ctx,
                    flow: flow,
                    ribbonAreaX: ribbonAreaX,
                    state: .highlighted
                )
            }
        }
        // Only opacity animates, so the Canvas redraws once per state change.
        .animation(Theme.quick, value: traced)
    }

    private enum RibbonState { case resting, dimmed, highlighted }

    private func drawRibbon(ctx: GraphicsContext,
                              flow: Flow,
                              ribbonAreaX: ClosedRange<CGFloat>,
                              state: RibbonState) {
        guard let srcSlot = layout.srcSlots[flow.sourceID],
              let dstSlot = layout.dstSlots[flow.destID] else { return }
        let start = CGPoint(x: ribbonAreaX.lowerBound, y: srcSlot.midY)
        let end = CGPoint(x: ribbonAreaX.upperBound, y: dstSlot.midY)
        var path = Path()
        path.move(to: start)
        let dx = end.x - start.x
        path.addCurve(
            to: end,
            control1: CGPoint(x: start.x + dx * 0.5, y: start.y),
            control2: CGPoint(x: end.x - dx * 0.5, y: end.y)
        )
        let opacity: CGFloat
        switch state {
        case .resting:     opacity = 0.42
        case .dimmed:      opacity = 0.08
        case .highlighted: opacity = 0.95
        }
        let thickness = ribbonThickness(for: flow.count)
        ctx.stroke(
            path,
            with: .color(flow.tint.opacity(opacity)),
            style: StrokeStyle(
                lineWidth: thickness,
                lineCap: .round,
                lineJoin: .round
            )
        )
    }

    private func ribbonThickness(for count: Int) -> CGFloat {
        let total = max(layout.totalFlowCount, 1)
        let ratio = CGFloat(count) / CGFloat(total)
        // Capped at 9pt so big flows cannot swallow neighbours at crossings.
        return max(1.5, min(9, ratio * lastHeight * 0.5))
    }

    // MARK: - Node rendering

    @ViewBuilder
    private func nodeView(_ node: Node, slot: Slot,
                           isFocused: Bool) -> some View {
        let isRollup = node.id == Self.otherSourceID || node.id == Self.otherDestID
        HStack(spacing: 8) {
            ZStack {
                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                    .fill(node.tint.opacity(isFocused ? 0.32 : 0.18))
                    .frame(width: 24, height: 24)
                Image(systemName: node.icon)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(node.tint)
            }
            VStack(alignment: .leading, spacing: 0) {
                PathLabel(path: node.label, font: Theme.Font.body(12.5, .semibold),
                          color: isRollup ? Theme.text2 : Theme.text1)
                Text("\(node.count) file\(node.count == 1 ? "" : "s")")
                    .font(Theme.Font.body(11.5, .medium).monospacedDigit())
                    .foregroundStyle(Theme.text3)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
        .background(
            Rectangle()
                // Rollup nodes get a lighter background so they read as "everything else".
                .fill(isRollup ? AnyShapeStyle(Color.white.opacity(0.04))
                                 : AnyShapeStyle(.ultraThinMaterial))
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                .stroke(
                    node.tint.opacity(isFocused ? 0.95 : (isRollup ? 0.18 : 0.30)),
                    lineWidth: isFocused ? 1.5 : 1
                )
        )
        // Focused node gets a 12pt glow. The resting shadow is 2.5pt so adjacent nodes don't bleed together at small gaps.
        .scaleEffect(isFocused ? 1.04 : 1.0)
        .shadow(
            color: isFocused ? node.tint.opacity(0.55) : .black.opacity(0.22),
            radius: isFocused ? 12 : 2.5,
            y: isFocused ? 4 : 1
        )
        .frame(height: slot.height)
        .contentShape(Rectangle())
        .help("\(node.label) · \(node.count) file\(node.count == 1 ? "" : "s") · click to see them")
        .animation(Theme.quick, value: isFocused)
    }

    // MARK: - Hover handling

    /// Marks the ribbon whose bezier at the cursor's X is nearest, if within `proximity` points. O(F) per move with F under about 70.
    private func handleContinuousHover(phase: HoverPhase,
                                         ribbonAreaX: ClosedRange<CGFloat>,
                                         totalW: CGFloat) {
        switch phase {
        case .ended:
            // Clear only a flow hover; a folder or bucket hover came from a node or another surface.
            cursorActive = false
            if case .flow = hoverBus.context {
                hoverBus.set(nil)
            }
        case .active(let point):
            // The cursor @State is written only over a ribbon: writing on every move re-strokes all ribbon paths.
            // Only the ribbon strip is hit-tested; node columns own their hover.
            guard ribbonAreaX.contains(point.x) else {
                cursorActive = false
                if case .flow = hoverBus.context {
                    hoverBus.set(nil)
                }
                return
            }
            var bestFlow: Flow?
            var bestDistance: CGFloat = .infinity
            for flow in layout.flows {
                guard let srcSlot = layout.srcSlots[flow.sourceID],
                      let dstSlot = layout.dstSlots[flow.destID] else {
                    continue
                }
                let curveY = bezierY(
                    cursorX: point.x,
                    startX: ribbonAreaX.lowerBound,
                    endX: ribbonAreaX.upperBound,
                    startY: srcSlot.midY,
                    endY: dstSlot.midY
                )
                let dy = abs(curveY - point.y)
                let proximity = ribbonThickness(for: flow.count) * 0.5 + 6
                if dy < proximity && dy < bestDistance {
                    bestDistance = dy
                    bestFlow = flow
                }
            }
            if let flow = bestFlow {
                cursorPos = point
                cursorActive = true
                let next: RestructureHoverContext = .flow(
                    sourceFolder: flow.sourceFolder,
                    destBucket: flow.destBucket
                )
                if hoverBus.context != next { hoverBus.set(next) }
            } else {
                cursorActive = false
                if case .flow = hoverBus.context {
                    hoverBus.set(nil)
                }
            }
        }
    }

    /// Tooltip following the cursor over a ribbon: source, destination and file count. It stays inside the diagram and ignores hits.
    @ViewBuilder
    private func hoverTooltip(geo: GeometryProxy) -> some View {
        if cursorActive,
           case .flow(let folder, let bucket) = hoverBus.context {
            let flow = layout.flows.first(where: {
                $0.sourceFolder == folder && $0.destBucket == bucket
            })
            let count = flow?.count ?? 0
            let srcLabel = flow?.sourceName ?? folder
            let dstLabel = flow?.destName ?? bucket
            let tooltipW: CGFloat = 240
            let tooltipH: CGFloat = 56
            let rawX = cursorPos.x + 14
            let rawY = cursorPos.y - tooltipH - 10
            let x = max(8, min(rawX, geo.size.width - tooltipW - 8))
            let y = max(8, min(rawY, geo.size.height - tooltipH - 8))
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text("\(count)")
                        .font(Theme.Font.body(14, .bold).monospacedDigit())
                        .foregroundStyle(Theme.gold)
                    Text("file\(count == 1 ? "" : "s") moving")
                        .font(Theme.Font.body(12, .semibold))
                        .foregroundStyle(Theme.text1)
                    Spacer(minLength: 0)
                }
                HStack(spacing: 5) {
                    Text(srcLabel)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Image(systemName: "arrow.right")
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(Theme.text3)
                    Text(dstLabel)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .font(Theme.Font.mono(10.5))
                .foregroundStyle(Theme.text2)
            }
            .padding(.horizontal, 10).padding(.vertical, 7)
            .frame(width: tooltipW, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                    .fill(.regularMaterial)
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                            .stroke(Theme.gold.opacity(0.45), lineWidth: 1)
                    )
            )
            .shadow(color: .black.opacity(0.5), radius: 12, y: 6)
            .position(x: x + tooltipW / 2, y: y + tooltipH / 2)
            .transition(.opacity)
            .allowsHitTesting(false)
        }
    }

    /// Y of the bezier at `cursorX`, with flat horizontal tangents at both ends.
    private func bezierY(cursorX: CGFloat,
                           startX: CGFloat, endX: CGFloat,
                           startY: CGFloat, endY: CGFloat) -> CGFloat {
        guard endX != startX else { return (startY + endY) / 2 }
        let t = max(0, min(1, (cursorX - startX) / (endX - startX)))
        let oneMinus = 1 - t
        return (oneMinus * oneMinus * oneMinus) * startY
             + 3 * (oneMinus * oneMinus) * t * startY
             + 3 * oneMinus * (t * t) * endY
             + (t * t * t) * endY
    }

    /// Whether the hover bus lights this node: the hovered node itself, the nodes it feeds or is fed by, both ends of a flow, or every node in an outcome.
    private func isNodeFocused(_ node: Node) -> Bool {
        switch hoverBus.context {
        case .none:
            return false
        case .sourceFolder(let folder):
            if node.isSource { return node.id == layout.sourceNodeID(forFolder: folder) }
            return layout.highlightedFlowIDs(for: hoverBus.context).contains { layout.flowsByID[$0]?.destID == node.id }
        case .destBucket(let bucket):
            if !node.isSource { return node.id == layout.destNodeID(forBucket: bucket) }
            return layout.highlightedFlowIDs(for: hoverBus.context).contains { layout.flowsByID[$0]?.sourceID == node.id }
        case .flow(let folder, let bucket):
            guard let flow = layout.flows.first(where: { $0.sourceFolder == folder && $0.destBucket == bucket }) else { return false }
            return node.id == (node.isSource ? flow.sourceID : flow.destID)
        case .outcome(let outcome):
            return layout.nodesByOutcome[outcome]?.contains(node.id) ?? false
        }
    }

    // MARK: - Layout (cached)

    /// Updates the @State layout cache only when geometry changes meaningfully, so a single-pixel resize doesn't drop it.
    private func recomputeLayoutIfNeeded(width: CGFloat, height: CGFloat) {
        let widthChanged = abs(width - lastWidth) > 1.5
        let heightChanged = abs(height - lastHeight) > 1.5
        if !widthChanged && !heightChanged && layout.isPopulated { return }
        let new = Self.computeLayout(proposals: proposals,
                                       width: width, height: height)
        layout = new
        lastWidth = width
        lastHeight = height
    }

    /// Pure layout computation, run on proposal changes and never on the hover path.
    static func computeLayout(proposals: [RestructureView.Proposal],
                                        width: CGFloat,
                                        height: CGFloat) -> Layout {
        guard !proposals.isEmpty, width > 0, height > 0 else {
            return .empty
        }

        struct SrcAccum { var folder: String; var count: Int; var isJunk: Bool }
        var srcAccum: [String: SrcAccum] = [:]
        var dstCount: [String: Int] = [:]
        for p in proposals {
            if var existing = srcAccum[p.sourceFolder] {
                existing.count += 1
                if p.kind != .dissolved { existing.isJunk = false }
                srcAccum[p.sourceFolder] = existing
            } else {
                srcAccum[p.sourceFolder] = SrcAccum(
                    folder: p.sourceFolder,
                    count: 1,
                    isJunk: p.kind == .dissolved
                )
            }
            dstCount[p.bucket, default: 0] += 1
        }

        // The long tail rolls up into one "Other" node per column.
        let allSrcs = srcAccum.values
            .sorted { a, b in a.count != b.count ? a.count > b.count : a.folder < b.folder }
        let visibleSrcs = Array(allSrcs.prefix(topN))
        let tailSrcs = Array(allSrcs.dropFirst(topN))
        let allDsts = dstCount
            .map { (bucket: $0.key, count: $0.value) }
            .sorted { a, b in a.count != b.count ? a.count > b.count : a.bucket < b.bucket }
        let visibleDsts = Array(allDsts.prefix(topN))
        let tailDsts = Array(allDsts.dropFirst(topN))

        // Sources use a neutral tint so the destination colors dominate; the tray.2 icon marks junk folders.
        var sources: [Node] = visibleSrcs.map { entry in
            let display = (entry.folder as NSString).lastPathComponent
            return Node(
                id: "src:\(entry.folder)",
                label: display.isEmpty ? entry.folder : display,
                identityKey: entry.folder,
                count: entry.count,
                icon: entry.isJunk ? "tray.2" : "tray.and.arrow.up",
                tint: .secondary,
                isSource: true
            )
        }
        if !tailSrcs.isEmpty {
            let count = tailSrcs.reduce(0) { $0 + $1.count }
            sources.append(Node(
                id: otherSourceID,
                label: "+ \(tailSrcs.count) more folder\(tailSrcs.count == 1 ? "" : "s")",
                identityKey: otherSourceID,
                count: count,
                icon: "ellipsis.circle",
                tint: .secondary,
                isSource: true
            ))
        }
        // Destination nodes use the Okabe-Ito CVD-safe palette (RESTRUCTURE.md §7).
        var destinations: [Node] = visibleDsts.enumerated().map { (idx, entry) in
            Node(
                id: "dst:\(entry.bucket)",
                label: entry.bucket,
                identityKey: entry.bucket,
                count: entry.count,
                icon: bucketIconName(entry.bucket),
                tint: Self.okabeIto[idx % Self.okabeIto.count],
                isSource: false
            )
        }
        if !tailDsts.isEmpty {
            let count = tailDsts.reduce(0) { $0 + $1.count }
            destinations.append(Node(
                id: otherDestID,
                label: "+ \(tailDsts.count) more folder\(tailDsts.count == 1 ? "" : "s")",
                identityKey: otherDestID,
                count: count,
                icon: "ellipsis.circle",
                tint: .secondary,
                isSource: false
            ))
        }

        // Long-tail endpoints remap to the "Other" nodes so every flow has a valid pair.
        let visibleSrcIDs = Set(sources.map(\.id))
        let visibleDstIDs = Set(destinations.map(\.id))
        struct Bucket { var sourceFolder: String; var destBucket: String;
                         var sourceID: String; var destID: String;
                         var tint: Color; var count: Int; var outcomes: Set<RestructureOutcome> }
        var bucketsByKey: [String: Bucket] = [:]
        // Ribbons carry their destination's color.
        let tintByDestID: [String: Color] = Dictionary(
            uniqueKeysWithValues: destinations.map { ($0.id, $0.tint) }
        )
        for p in proposals {
            let srcID = visibleSrcIDs.contains("src:\(p.sourceFolder)")
                ? "src:\(p.sourceFolder)"
                : otherSourceID
            let dstID = visibleDstIDs.contains("dst:\(p.bucket)")
                ? "dst:\(p.bucket)"
                : otherDestID
            let key = "\(srcID)→\(dstID)"
            let tint = tintByDestID[dstID] ?? .secondary
            let outcome: RestructureOutcome = p.kind == .dissolved ? .reorganize : .tidy
            if var existing = bucketsByKey[key] {
                existing.count += 1
                existing.outcomes.insert(outcome)
                bucketsByKey[key] = existing
            } else {
                bucketsByKey[key] = Bucket(
                    sourceFolder: p.sourceFolder,
                    destBucket: p.bucket,
                    sourceID: srcID,
                    destID: dstID,
                    tint: tint,
                    count: 1,
                    outcomes: [outcome]
                )
            }
        }
        let flows: [Flow] = bucketsByKey.values.map {
            Flow(
                sourceID: $0.sourceID,
                destID: $0.destID,
                sourceFolder: $0.sourceFolder,
                destBucket: $0.destBucket,
                tint: $0.tint,
                count: $0.count,
                sourceName: $0.sourceID == otherSourceID ? Self.otherFoldersName(tailSrcs.count) : Self.folderName($0.sourceFolder),
                destName: $0.destID == otherDestID ? Self.otherFoldersName(tailDsts.count) : $0.destBucket,
                outcomes: $0.outcomes
            )
        }
        let totalFlowCount = flows.reduce(0) { $0 + $1.count }

        // Barycentric ordering reduces ribbon crossings; one or two passes suffice.
        var srcOrder = sources.map(\.id)
        var dstOrder = destinations.map(\.id)
        for _ in 0..<2 {
            let dstIndex: [String: Int] = Dictionary(
                uniqueKeysWithValues: dstOrder.enumerated().map { ($1, $0) }
            )
            let srcWeight: [String: Double] = Dictionary(
                grouping: flows,
                by: { $0.sourceID }
            ).mapValues { fls in
                let totalW = fls.reduce(0) { $0 + $1.count }
                guard totalW > 0 else { return 0.0 }
                let weighted = fls.reduce(0.0) { acc, f in
                    acc + Double(f.count) * Double(dstIndex[f.destID] ?? 0)
                }
                return weighted / Double(totalW)
            }
            srcOrder.sort {
                (srcWeight[$0] ?? 0) < (srcWeight[$1] ?? 0)
            }
            let srcIndex: [String: Int] = Dictionary(
                uniqueKeysWithValues: srcOrder.enumerated().map { ($1, $0) }
            )
            let dstWeight: [String: Double] = Dictionary(
                grouping: flows,
                by: { $0.destID }
            ).mapValues { fls in
                let totalW = fls.reduce(0) { $0 + $1.count }
                guard totalW > 0 else { return 0.0 }
                let weighted = fls.reduce(0.0) { acc, f in
                    acc + Double(f.count) * Double(srcIndex[f.sourceID] ?? 0)
                }
                return weighted / Double(totalW)
            }
            dstOrder.sort {
                (dstWeight[$0] ?? 0) < (dstWeight[$1] ?? 0)
            }
        }
        let srcMap: [String: Node] = Dictionary(
            uniqueKeysWithValues: sources.map { ($0.id, $0) }
        )
        let dstMap: [String: Node] = Dictionary(
            uniqueKeysWithValues: destinations.map { ($0.id, $0) }
        )
        sources = srcOrder.compactMap { srcMap[$0] }
        destinations = dstOrder.compactMap { dstMap[$0] }

        // Rollups go last in their column; the barycentric sort could leave them mid-column, reading as a real folder.
        if let i = sources.firstIndex(where: { $0.id == otherSourceID }) {
            let rollup = sources.remove(at: i)
            sources.append(rollup)
        }
        if let i = destinations.firstIndex(where: { $0.id == otherDestID }) {
            let rollup = destinations.remove(at: i)
            destinations.append(rollup)
        }

        let srcSlots = layoutSlots(nodes: sources, totalHeight: height)
        let dstSlots = layoutSlots(nodes: destinations, totalHeight: height)

        // The hover path looks flows up by id instead of scanning.
        let flowsByID = Dictionary(
            uniqueKeysWithValues: flows.map { ($0.id, $0) }
        )

        // Cross-highlight indexes, so hover never scans `proposals`.
        var destinationsForSource: [String: Set<String>] = [:]
        var sourcesForDestination: [String: Set<String>] = [:]
        for p in proposals {
            destinationsForSource[p.sourceFolder, default: []].insert(p.bucket)
            sourcesForDestination[p.bucket, default: []].insert(p.sourceFolder)
        }
        var nodesByOutcome: [RestructureOutcome: Set<String>] = [:]
        for p in proposals {
            let outcome: RestructureOutcome = (p.kind == .dissolved)
                ? .reorganize : .tidy
            nodesByOutcome[outcome, default: []].insert("src:\(p.sourceFolder)")
            nodesByOutcome[outcome, default: []].insert("dst:\(p.bucket)")
            // Rollups light up with their outcome too.
            if !visibleSrcIDs.contains("src:\(p.sourceFolder)") {
                nodesByOutcome[outcome, default: []].insert(otherSourceID)
            }
            if !visibleDstIDs.contains("dst:\(p.bucket)") {
                nodesByOutcome[outcome, default: []].insert(otherDestID)
            }
        }

        return Layout(
            sources: sources,
            destinations: destinations,
            flows: flows,
            flowsByID: flowsByID,
            srcSlots: srcSlots,
            dstSlots: dstSlots,
            totalFlowCount: totalFlowCount,
            destinationsForSource: destinationsForSource,
            sourcesForDestination: sourcesForDestination,
            nodesByOutcome: nodesByOutcome,
            rollupSourceFolders: tailSrcs.map(\.folder),
            rollupDestBuckets: tailDsts.map(\.bucket),
            isPopulated: true
        )
    }

    static func folderName(_ folder: String) -> String {
        let name = (folder as NSString).lastPathComponent
        return name.isEmpty ? folder : name
    }

    static func otherFoldersName(_ count: Int) -> String { "\(count) other folder\(count == 1 ? "" : "s")" }

    private static let columnVerticalBuffer: CGFloat = 14
    private static let nodeSlotHeight: CGFloat = 44
    private static let nodeGap: CGFloat = 14

    /// Proportional slot Y positions for a column: small nodes flex up to `preferredMin`, the rest scale to fit. The gaps keep node shadows apart, and the buffer leaves room for the focused halo.
    static func layoutSlots(nodes: [Node], totalHeight: CGFloat) -> [String: Slot] {
        guard !nodes.isEmpty, totalHeight > 0 else { return [:] }
        let gap = nodeGap
        let totalCount = nodes.reduce(0) { $0 + $1.count }
        let preferredMin = nodeSlotHeight
        let absoluteMin: CGFloat = 40
        let buffer = columnVerticalBuffer
        let layoutHeight = max(0, totalHeight - buffer * 2)
        let availableHeight = max(0, layoutHeight - CGFloat(nodes.count - 1) * gap)
        let absMinTotal = absoluteMin * CGFloat(nodes.count)
        if availableHeight < absMinTotal {
            let h = max(0, availableHeight / CGFloat(nodes.count))
            var result: [String: Slot] = [:]
            var y: CGFloat = buffer
            for n in nodes {
                result[n.id] = Slot(topY: y, height: h)
                y += h + gap
            }
            return result
        }
        var heights: [CGFloat] = nodes.map { _ in 0 }
        var fixedTotal: CGFloat = 0
        var flexCountSum = 0
        let flexThreshold = max(1, totalCount / 20)
        for (i, n) in nodes.enumerated() {
            if n.count <= flexThreshold {
                heights[i] = preferredMin
                fixedTotal += preferredMin
            } else {
                flexCountSum += n.count
            }
        }
        let flexAvailable = max(0, availableHeight - fixedTotal)
        let floorTotal = preferredMin * CGFloat(nodes.count)
        if availableHeight >= floorTotal, flexCountSum > 0 {
            let extra = availableHeight - floorTotal
            for (i, n) in nodes.enumerated() where heights[i] == 0 {
                heights[i] = preferredMin + extra * CGFloat(n.count) / CGFloat(flexCountSum)
            }
        }
        for (i, n) in nodes.enumerated() {
            if heights[i] == 0 {
                let h = flexCountSum > 0
                    ? max(preferredMin, flexAvailable * CGFloat(n.count) / CGFloat(flexCountSum))
                    : preferredMin
                heights[i] = h
            }
        }
        let computedTotal = heights.reduce(0, +)
        if computedTotal > availableHeight {
            let scale = availableHeight / computedTotal
            for i in 0..<heights.count {
                heights[i] = max(absoluteMin, heights[i] * scale)
            }
            let total2 = heights.reduce(0, +)
            if total2 > availableHeight {
                let h = availableHeight / CGFloat(nodes.count)
                heights = Array(repeating: h, count: nodes.count)
            }
        }
        var result: [String: Slot] = [:]
        // Starting at `buffer` leaves room for the focused halo above the first node and below the last.
        var y: CGFloat = buffer
        for (i, n) in nodes.enumerated() {
            result[n.id] = Slot(topY: y, height: heights[i])
            y += heights[i] + gap
        }
        return result
    }

    private var sankeyHeight: CGFloat { Self.mapHeight(nodeCount: max(layout.sources.count, layout.destinations.count, 1)) }

    /// Tall enough that every node slot keeps its two label lines and the gap between nodes.
    static func mapHeight(nodeCount maxNodes: Int) -> CGFloat {
        let fitsNodes = CGFloat(maxNodes) * (Self.nodeSlotHeight + Self.nodeGap) + 2 * Self.columnVerticalBuffer
        if maxNodes < 4 { return max(160 + CGFloat(maxNodes) * 40, fitsNodes) }
        return max(320, fitsNodes)
    }

}

