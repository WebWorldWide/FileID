import SwiftUI

/// Sankey flow diagram for the Restructure tab. All ribbons render
/// in one `Canvas`. Layout (sources, destinations, flows, slot Y
/// positions, cross-highlight indexes) is computed once per data or
/// geometry change and cached in `layout`. Hover is a single
/// `.onContinuousHover` that hit-tests by cursor-to-bezier proximity.
struct SankeyFlowView: View {
    let proposals: [RestructureView.Proposal]
    var onTapSource: (String) -> Void = { _ in }
    var onTapDestination: (String) -> Void = { _ in }
    /// Tapping "+ N more folders" / "+ N more buckets" — the parent
    /// scopes the drill-down to the long-tail list rather than the
    /// literal rollup label.
    var onTapSourceRollup: ([String]) -> Void = { _ in }
    var onTapDestRollup: ([String]) -> Void = { _ in }
    var hoverBus: RestructureHoverBus

    @State private var layout: Layout = .empty
    @State private var lastWidth: CGFloat = 0
    @State private var lastHeight: CGFloat = 0
    @State private var cursorPos: CGPoint = .zero
    @State private var cursorActive = false
    @State private var entranceProgress: CGFloat = 0

    private static let topN = 8
    private static let otherSourceID = "src:__other__"
    private static let otherDestID = "dst:__other__"

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

    /// Column headers: "FROM (N)" → "TO (M)" with a connecting arrow
    /// in the middle. Tiny tracking-spaced caps reads as a section
    /// header, not a label, and the arrow makes the directionality
    /// of the diagram unmistakable at a glance.
    @ViewBuilder
    private var columnHeaderRow: some View {
        let nodeColW: CGFloat = 196
        HStack(spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "tray.and.arrow.up.fill")
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
                Text("FROM")
                    .font(.system(size: 10, weight: .bold))
                    .tracking(0.8)
                    .foregroundStyle(.secondary)
                Text("\(layout.sources.count)")
                    .font(.system(size: 10, weight: .semibold).monospacedDigit())
                    .foregroundStyle(.tertiary)
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
                Text("\(layout.destinations.count)")
                    .font(.system(size: 10, weight: .semibold).monospacedDigit())
                    .foregroundStyle(.tertiary)
                Text("TO")
                    .font(.system(size: 10, weight: .bold))
                    .tracking(0.8)
                    .foregroundStyle(.secondary)
                Image(systemName: "tray.and.arrow.down.fill")
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
            }
            .frame(width: nodeColW, alignment: .trailing)
        }
        .padding(.horizontal, 2)
    }

    /// Main diagram area — GeometryReader with the ribbon Canvas +
    /// node columns + tooltip overlay.
    @ViewBuilder
    private var diagramArea: some View {
        GeometryReader { geo in
            let nodeColW: CGFloat = 196
            let totalW = geo.size.width
            let totalH = geo.size.height
            let ribbonAreaX: ClosedRange<CGFloat> =
                nodeColW ... (totalW - nodeColW)

            ZStack(alignment: .topLeading) {
                // 1. Single Canvas draws every ribbon. One view,
                // one draw pass per render — orders of magnitude
                // cheaper than 70 Path views.
                ribbonCanvas(ribbonAreaX: ribbonAreaX, totalH: totalH)
                    .opacity(Double(entranceProgress))

                    // 2. Source column.
                    ForEach(layout.sources) { node in
                        if let slot = layout.srcSlots[node.id] {
                            nodeView(node, slot: slot,
                                      isFocused: isNodeFocused(node))
                                .frame(width: nodeColW)
                                .position(x: nodeColW / 2, y: slot.midY)
                                .onTapGesture {
                                    if node.id == Self.otherSourceID {
                                        onTapSourceRollup(layout.rollupSourceFolders)
                                    } else {
                                        onTapSource(node.label)
                                    }
                                }
                                .onHover { hovering in
                                    hoverBus.set(
                                        hovering && node.id != Self.otherSourceID
                                            ? .sourceFolder(node.identityKey)
                                            : nil
                                    )
                                }
                        }
                    }

                    // 3. Destination column.
                    ForEach(layout.destinations) { node in
                        if let slot = layout.dstSlots[node.id] {
                            nodeView(node, slot: slot,
                                      isFocused: isNodeFocused(node))
                                .frame(width: nodeColW)
                                .position(x: totalW - nodeColW / 2,
                                            y: slot.midY)
                                .onTapGesture {
                                    if node.id == Self.otherDestID {
                                        onTapDestRollup(layout.rollupDestBuckets)
                                    } else {
                                        onTapDestination(node.label)
                                    }
                                }
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
                // Single hover detector for the entire ribbon area.
                // Runs per mousemove, finds the closest ribbon by
                // proximity to its bezier curve, writes to the bus
                // only when the answer changes.
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
                .onChange(of: proposals.count) { _, _ in
                    layout = Self.computeLayout(
                        proposals: proposals,
                        width: totalW,
                        height: totalH
                    )
                    // Replay entrance animation when data changes.
                    entranceProgress = 0
                    withAnimation(.easeOut(duration: 0.55)) {
                        entranceProgress = 1
                    }
                }
                // Floating tooltip near the cursor when hovering a
                // ribbon — shows the source folder, destination, and
                // file count of the ribbon under the pointer.
                hoverTooltip(geo: geo)
            }
            .frame(height: sankeyHeight)
            .clipped()
            .onAppear {
                if entranceProgress < 1 {
                    withAnimation(.easeOut(duration: 0.55)) {
                        entranceProgress = 1
                    }
                }
            }
    }

    // MARK: - Ribbon canvas

    /// Draw all ribbons in a single Canvas. Hovered ribbon (if any)
    /// is drawn last so it overlays the others. Non-hovered ribbons
    /// share the source-node tint at low opacity at rest, lower still
    /// when something else is hovered.
    @ViewBuilder
    private func ribbonCanvas(ribbonAreaX: ClosedRange<CGFloat>,
                                totalH: CGFloat) -> some View {
        let hoveredFlowID = hoveredFlowID()
        let dimAll = hoveredFlowID != nil

        Canvas(opaque: false, rendersAsynchronously: false) { ctx, _ in
            // Draw non-highlighted ribbons first (under).
            for flow in layout.flows where flow.id != hoveredFlowID {
                drawRibbon(
                    ctx: ctx,
                    flow: flow,
                    ribbonAreaX: ribbonAreaX,
                    state: dimAll ? .dimmed : .resting
                )
            }
            // Highlighted ribbon last so it sits on top.
            if let id = hoveredFlowID,
               let flow = layout.flowsByID[id] {
                drawRibbon(
                    ctx: ctx,
                    flow: flow,
                    ribbonAreaX: ribbonAreaX,
                    state: .highlighted
                )
            }
        }
        // Performant ribbon animation: only opacity transitions, no
        // per-ribbon view diff. Canvas redraws once per state change.
        .animation(.easeOut(duration: 0.18), value: hoveredFlowID)
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
        case .resting:     opacity = 0.18
        case .dimmed:      opacity = 0.04
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
        // Cap at 9pt — big flows still dominate proportionally but
        // can't swallow neighbors at intersection points.
        return max(1.5, min(9, ratio * lastHeight * 0.5))
    }

    // MARK: - Node rendering

    @ViewBuilder
    private func nodeView(_ node: Node, slot: Slot,
                           isFocused: Bool) -> some View {
        let isRollup = node.id == Self.otherSourceID || node.id == Self.otherDestID
        HStack(spacing: 8) {
            ZStack {
                RoundedRectangle(cornerRadius: 6)
                    .fill(node.tint.opacity(isFocused ? 0.32 : 0.18))
                    .frame(width: 24, height: 24)
                Image(systemName: node.icon)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(node.tint)
            }
            VStack(alignment: .leading, spacing: 0) {
                Text(node.label)
                    .font(.system(size: 12, weight: .semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .foregroundStyle(isRollup ? .secondary : .primary)
                Text("\(node.count) file\(node.count == 1 ? "" : "s")")
                    .font(.system(size: 10, weight: .medium).monospacedDigit())
                    .foregroundStyle(.tertiary)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 9)
                // Rollup nodes get a lighter, less assertive
                // background so they read clearly as
                // "everything else" rather than competing with the
                // real first-class folders.
                .fill(isRollup ? AnyShapeStyle(Color.secondary.opacity(0.06))
                                 : AnyShapeStyle(.ultraThinMaterial))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 9)
                .stroke(
                    node.tint.opacity(isFocused ? 0.95 : (isRollup ? 0.18 : 0.30)),
                    lineWidth: isFocused ? 1.5 : 1
                )
        )
        // Subtle scale + tinted glow for the focused node. Resting
        // shadow shrunk 5 → 2.5pt so adjacent nodes don't bleed into
        // each other at small gaps. Focused state still gets a
        // generous 12pt glow because only one node is focused at a
        // time so there's no neighbor to overlap with.
        .scaleEffect(isFocused ? 1.04 : 1.0)
        .shadow(
            color: isFocused ? node.tint.opacity(0.55) : .black.opacity(0.22),
            radius: isFocused ? 12 : 2.5,
            y: isFocused ? 4 : 1
        )
        .frame(height: slot.height)
        .contentShape(RoundedRectangle(cornerRadius: 9))
        .help("\(node.count) file\(node.count == 1 ? "" : "s") · tap to see them")
        .animation(.easeOut(duration: 0.18), value: isFocused)
    }

    // MARK: - Hover handling

    /// What ribbon is currently considered "hovered" — derived from
    /// the bus, not from a local @State, so external surfaces (cards,
    /// staysPut rows) can drive the highlight too.
    private func hoveredFlowID() -> String? {
        switch hoverBus.context {
        case .none:
            return nil
        case .flow(let folder, let bucket):
            // Walk the cached flowsByID sparingly — small set.
            for f in layout.flows where f.sourceFolder == folder
                                      && f.destBucket == bucket {
                return f.id
            }
            return nil
        case .sourceFolder, .destBucket, .outcome:
            // Node + outcome highlights affect MANY ribbons. We
            // signal via the dimAll path inside ribbonCanvas (resting
            // ribbons that touch the focused source/bucket/outcome
            // stay bright; others dim). The single-flow lookup
            // returns nil so no specific ribbon gets the .highlighted
            // overlay treatment.
            return nil
        }
    }

    /// Handle continuous mousemove inside the diagram. Finds the
    /// nearest ribbon's bezier point at the cursor's X, marks that
    /// ribbon as hovered if the cursor is within `proximity` points
    /// of the curve. O(F) per move — F < ~70 here, ~µs.
    private func handleContinuousHover(phase: HoverPhase,
                                         ribbonAreaX: ClosedRange<CGFloat>,
                                         totalW: CGFloat) {
        switch phase {
        case .ended:
            // Only clear if the bus was holding a flow we set. Don't
            // stomp a sourceFolder/destBucket hover that came from a
            // node or an external surface.
            cursorActive = false
            if case .flow = hoverBus.context {
                hoverBus.set(nil)
            }
        case .active(let point):
            cursorPos = point
            cursorActive = true
            // Only hit-test inside the ribbon strip — node columns
            // own their own hover logic.
            guard ribbonAreaX.contains(point.x) else {
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
                let next: RestructureHoverContext = .flow(
                    sourceFolder: flow.sourceFolder,
                    destBucket: flow.destBucket
                )
                if hoverBus.context != next { hoverBus.set(next) }
            } else if case .flow = hoverBus.context {
                hoverBus.set(nil)
            }
        }
    }

    /// Floating tooltip that follows the cursor when hovering a
    /// ribbon. Shows source → destination + file count. Pinned to a
    /// safe position (won't overflow the diagram bounds) and
    /// pointer-events transparent so it doesn't capture hover.
    @ViewBuilder
    private func hoverTooltip(geo: GeometryProxy) -> some View {
        if cursorActive,
           case .flow(let folder, let bucket) = hoverBus.context {
            let count = layout.flows.first(where: {
                $0.sourceFolder == folder && $0.destBucket == bucket
            })?.count ?? 0
            let display = (folder as NSString).lastPathComponent
            let srcLabel = display.isEmpty ? folder : display
            let tooltipW: CGFloat = 240
            let tooltipH: CGFloat = 56
            // Anchor tooltip slightly above-right of the cursor;
            // clamp within the diagram bounds.
            let rawX = cursorPos.x + 14
            let rawY = cursorPos.y - tooltipH - 10
            let x = max(8, min(rawX, geo.size.width - tooltipW - 8))
            let y = max(8, min(rawY, geo.size.height - tooltipH - 8))
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text("\(count)")
                        .font(.system(size: 14, weight: .bold,
                                       design: .rounded).monospacedDigit())
                        .foregroundStyle(Theme.gold)
                    Text("file\(count == 1 ? "" : "s") moving")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.primary)
                    Spacer(minLength: 0)
                }
                HStack(spacing: 5) {
                    Text(srcLabel)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Image(systemName: "arrow.right")
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(.tertiary)
                    Text(bucket)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 10).padding(.vertical, 7)
            .frame(width: tooltipW, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(.regularMaterial)
                    .overlay(
                        RoundedRectangle(cornerRadius: 8)
                            .stroke(Theme.gold.opacity(0.45), lineWidth: 1)
                    )
            )
            .shadow(color: .black.opacity(0.5), radius: 12, y: 6)
            .position(x: x + tooltipW / 2, y: y + tooltipH / 2)
            .transition(.opacity)
            .allowsHitTesting(false)
        }
    }

    /// Y of a horizontal cubic bezier at a given cursor X.
    /// Control points are pinned at the midpoint horizontally so the
    /// curve has flat tangents at start/end (matches the rendered
    /// path). Solved via the cubic bezier parametric formula at
    /// t = (cursorX - startX) / (endX - startX).
    private func bezierY(cursorX: CGFloat,
                           startX: CGFloat, endX: CGFloat,
                           startY: CGFloat, endY: CGFloat) -> CGFloat {
        guard endX != startX else { return (startY + endY) / 2 }
        let t = max(0, min(1, (cursorX - startX) / (endX - startX)))
        let oneMinus = 1 - t
        // Control points: (startX + dx*0.5, startY) and
        // (endX - dx*0.5, endY). For a horizontal sweep, the y
        // component reduces to the standard cubic interpolation
        // between startY and endY weighted by the bezier basis.
        return (oneMinus * oneMinus * oneMinus) * startY
             + 3 * (oneMinus * oneMinus) * t * startY
             + 3 * oneMinus * (t * t) * endY
             + (t * t * t) * endY
    }

    /// True if a node should glow because of the current bus state.
    /// Cross-highlight rules:
    ///   - bus.sourceFolder matches a source node directly, OR
    ///     matches a destination that this source feeds.
    ///   - bus.destBucket matches a destination directly, OR matches
    ///     a source that feeds this destination.
    ///   - bus.flow lights both endpoints.
    ///   - bus.outcome lights any node that participates in proposals
    ///     of that outcome class.
    private func isNodeFocused(_ node: Node) -> Bool {
        switch hoverBus.context {
        case .none:
            return false
        case .sourceFolder(let folder):
            if node.isSource { return node.identityKey == folder }
            return layout.destinationsForSource[folder]?.contains(node.identityKey) ?? false
        case .destBucket(let bucket):
            if !node.isSource { return node.identityKey == bucket }
            return layout.sourcesForDestination[bucket]?.contains(node.identityKey) ?? false
        case .flow(let folder, let bucket):
            return node.isSource ? node.identityKey == folder
                                  : node.identityKey == bucket
        case .outcome(let outcome):
            return layout.nodesByOutcome[outcome]?.contains(node.id) ?? false
        }
    }

    // MARK: - Layout (cached)

    /// Updates @State layout cache only when geometry changed
    /// meaningfully. A single-pixel resize won't blow away the cache.
    /// Non-mutating because @State's wrappedValue setter routes
    /// writes through SwiftUI's StoredLocation, not through `self`.
    private func recomputeLayoutIfNeeded(width: CGFloat, height: CGFloat) {
        let widthChanged = abs(width - lastWidth) > 1.5
        let heightChanged = abs(height - lastHeight) > 1.5
        if !widthChanged && !heightChanged && layout.isPopulated { return }
        let new = Self.computeLayout(proposals: proposals,
                                       width: width, height: height)
        // The closure-captured self is a value; assigning to its
        // @State backing requires us to issue the write through the
        // .onAppear/.onChange closure body, which DOES have mutating
        // access to the wrapper. We rely on SwiftUI binding semantics
        // here.
        layout = new
        lastWidth = width
        lastHeight = height
    }

    /// Pure layout computation. No @State, no SwiftUI — just data
    /// transformations. Cheap enough to call on every proposals
    /// change but never hit on the hover path.
    private static func computeLayout(proposals: [RestructureView.Proposal],
                                        width: CGFloat,
                                        height: CGFloat) -> Layout {
        guard !proposals.isEmpty, width > 0, height > 0 else {
            return .empty
        }

        // 1. Build raw source/destination tallies.
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

        // 2. Pick top-N visible nodes per column. Long tail rolls up
        // into a single "Other" placeholder.
        let allSrcs = srcAccum.values
            .sorted { $0.count > $1.count }
        let visibleSrcs = Array(allSrcs.prefix(topN))
        let tailSrcs = Array(allSrcs.dropFirst(topN))
        let allDsts = dstCount
            .map { (bucket: $0.key, count: $0.value) }
            .sorted { $0.count > $1.count }
        let visibleDsts = Array(allDsts.prefix(topN))
        let tailDsts = Array(allDsts.dropFirst(topN))

        var sources: [Node] = visibleSrcs.map { entry in
            let display = (entry.folder as NSString).lastPathComponent
            return Node(
                id: "src:\(entry.folder)",
                label: display.isEmpty ? entry.folder : display,
                identityKey: entry.folder,
                count: entry.count,
                icon: entry.isJunk ? "tray.2" : "tray.and.arrow.up",
                tint: entry.isJunk ? Theme.gold : .orange,
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
        var destinations: [Node] = visibleDsts.map { entry in
            Node(
                id: "dst:\(entry.bucket)",
                label: entry.bucket,
                identityKey: entry.bucket,
                count: entry.count,
                icon: bucketIcon(entry.bucket),
                tint: Theme.gold,
                isSource: false
            )
        }
        if !tailDsts.isEmpty {
            let count = tailDsts.reduce(0) { $0 + $1.count }
            destinations.append(Node(
                id: otherDestID,
                label: "+ \(tailDsts.count) more bucket\(tailDsts.count == 1 ? "" : "s")",
                identityKey: otherDestID,
                count: count,
                icon: "ellipsis.circle",
                tint: .secondary,
                isSource: false
            ))
        }

        // 3. Aggregate flows. Long-tail sources/destinations remap
        // to the "Other" rollup nodes so every flow has a valid
        // endpoint pair.
        let visibleSrcIDs = Set(sources.map(\.id))
        let visibleDstIDs = Set(destinations.map(\.id))
        struct Bucket { var sourceFolder: String; var destBucket: String;
                         var sourceID: String; var destID: String;
                         var tint: Color; var count: Int }
        var bucketsByKey: [String: Bucket] = [:]
        // Tint per source ID — gold for junk, orange for mixed.
        let tintBySourceID: [String: Color] = Dictionary(
            uniqueKeysWithValues: sources.map { ($0.id, $0.tint) }
        )
        for p in proposals {
            let srcID = visibleSrcIDs.contains("src:\(p.sourceFolder)")
                ? "src:\(p.sourceFolder)"
                : otherSourceID
            let dstID = visibleDstIDs.contains("dst:\(p.bucket)")
                ? "dst:\(p.bucket)"
                : otherDestID
            let key = "\(srcID)→\(dstID)"
            let tint = tintBySourceID[srcID] ?? Theme.gold
            if var existing = bucketsByKey[key] {
                existing.count += 1
                bucketsByKey[key] = existing
            } else {
                bucketsByKey[key] = Bucket(
                    sourceFolder: p.sourceFolder,
                    destBucket: p.bucket,
                    sourceID: srcID,
                    destID: dstID,
                    tint: tint,
                    count: 1
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
                count: $0.count
            )
        }
        let totalFlowCount = flows.reduce(0) { $0 + $1.count }

        // 4. Barycentric ordering — re-sort source/destination columns
        // so each node sits near the average position of the nodes it
        // connects to. This is the standard heuristic for cutting
        // Sankey ribbon crossings; one or two passes is usually
        // enough. We seed with count-sorted positions, then iterate.
        var srcOrder = sources.map(\.id)
        var dstOrder = destinations.map(\.id)
        for _ in 0..<2 {
            // Source y = weighted avg of destination indices.
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
            // Destination y = weighted avg of source indices (with new order).
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
        // Apply ordering.
        let srcMap: [String: Node] = Dictionary(
            uniqueKeysWithValues: sources.map { ($0.id, $0) }
        )
        let dstMap: [String: Node] = Dictionary(
            uniqueKeysWithValues: destinations.map { ($0.id, $0) }
        )
        sources = srcOrder.compactMap { srcMap[$0] }
        destinations = dstOrder.compactMap { dstMap[$0] }

        // Pin "+ N more" rollups to the bottom of their column —
        // barycentric sort can otherwise place them mid-column,
        // which reads as a peer of the real folders rather than an
        // aggregation of the long tail.
        if let i = sources.firstIndex(where: { $0.id == otherSourceID }) {
            let rollup = sources.remove(at: i)
            sources.append(rollup)
        }
        if let i = destinations.firstIndex(where: { $0.id == otherDestID }) {
            let rollup = destinations.remove(at: i)
            destinations.append(rollup)
        }

        // 5. Compute slot Y positions per column.
        let srcSlots = layoutSlots(nodes: sources, totalHeight: height)
        let dstSlots = layoutSlots(nodes: destinations, totalHeight: height)

        // 6. Build a flow-by-id lookup so the hover path doesn't have
        // to scan the array.
        let flowsByID = Dictionary(
            uniqueKeysWithValues: flows.map { ($0.id, $0) }
        )

        // 7. Helper indexes for cross-highlight without per-render
        // O(N) scans of `proposals`.
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
            // Also include the rollups so when the user hovers an
            // outcome class the +N rollups light up too.
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

    /// Compute proportional slot Y positions for a column. Flexes
    /// small nodes up to `preferredMin`, scales the rest to fit
    /// `totalHeight`. The 14pt gap is wider than any node shadow
    /// radius so adjacent cards' shadows don't bleed into each
    /// other; the 14pt vertical buffer (`columnVerticalBuffer`)
    /// reserves room above the first / below the last node for the
    /// focused-state gold halo to bloom without clipping.
    private static let columnVerticalBuffer: CGFloat = 14

    private static func layoutSlots(nodes: [Node], totalHeight: CGFloat) -> [String: Slot] {
        guard !nodes.isEmpty, totalHeight > 0 else { return [:] }
        let gap: CGFloat = 14
        let totalCount = nodes.reduce(0) { $0 + $1.count }
        let preferredMin: CGFloat = 28
        let absoluteMin: CGFloat = 18
        let buffer = columnVerticalBuffer
        // Effective layout height excludes the top + bottom buffers.
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
        // Start at `buffer` so the first node has room above it for a
        // focused-state shadow halo. Final node's bottom edge will
        // be at most layoutHeight + buffer = totalHeight - buffer,
        // leaving the matching buffer at the bottom.
        var y: CGFloat = buffer
        for (i, n) in nodes.enumerated() {
            result[n.id] = Slot(topY: y, height: heights[i])
            y += heights[i] + gap
        }
        return result
    }

    private static func bucketIcon(_ bucket: String) -> String { bucketIconName(bucket) }

    private var sankeyHeight: CGFloat {
        let maxNodes = max(layout.sources.count, layout.destinations.count, 4)
        return min(580, max(320, CGFloat(maxNodes) * 56))
    }

    // MARK: - Types

    struct Layout {
        var sources: [Node]
        var destinations: [Node]
        var flows: [Flow]
        var flowsByID: [String: Flow]
        var srcSlots: [String: Slot]
        var dstSlots: [String: Slot]
        var totalFlowCount: Int
        var destinationsForSource: [String: Set<String>]
        var sourcesForDestination: [String: Set<String>]
        var nodesByOutcome: [RestructureOutcome: Set<String>]
        /// Long-tail source folder paths that were collapsed into the
        /// "+ N more folders" rollup node. Tapping the rollup drills
        /// down into these.
        var rollupSourceFolders: [String]
        var rollupDestBuckets: [String]
        var isPopulated: Bool

        static let empty = Layout(
            sources: [], destinations: [], flows: [],
            flowsByID: [:], srcSlots: [:], dstSlots: [:],
            totalFlowCount: 0,
            destinationsForSource: [:],
            sourcesForDestination: [:],
            nodesByOutcome: [:],
            rollupSourceFolders: [],
            rollupDestBuckets: [],
            isPopulated: false
        )
    }

    struct Node: Identifiable, Hashable {
        let id: String
        let label: String
        let identityKey: String
        let count: Int
        let icon: String
        let tint: Color
        let isSource: Bool
    }

    struct Flow: Identifiable, Hashable {
        var id: String { "\(sourceID)→\(destID)" }
        let sourceID: String
        let destID: String
        let sourceFolder: String
        let destBucket: String
        let tint: Color
        let count: Int
    }

    struct Slot: Hashable {
        let topY: CGFloat
        let height: CGFloat
        var midY: CGFloat { topY + height / 2 }
    }
}
