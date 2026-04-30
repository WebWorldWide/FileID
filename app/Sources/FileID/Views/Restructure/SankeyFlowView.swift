// V7 — Sankey flow visualization for the Restructure tab.
//
// Source folders on the left, destination folders on the right,
// curved ribbons between them representing file movements. Ribbon
// width ∝ file count. Hovering a ribbon highlights its endpoints +
// dims the rest. Tap → drill-down sheet with the exact file list.
//
// Renders entirely in SwiftUI Path/Canvas — no third-party charts
// dependency. Gold gradient strokes on dark glass extend the
// LavaLamp aesthetic instead of clashing with it.
import SwiftUI

struct SankeyFlowView: View {
    let proposals: [RestructureView.Proposal]
    /// Tap callback for drill-down — pass either a source folder or
    /// destination bucket; consumer scopes the sheet accordingly.
    var onTapSource: (String) -> Void = { _ in }
    var onTapDestination: (String) -> Void = { _ in }
    /// Shared hover bus. Hovering a ribbon or node writes the
    /// matching context here; recommendation cards / staysPut /
    /// TreeDiff read the same bus and light up in sync.
    var hoverBus: RestructureHoverBus

    /// One node in the source or destination column.
    private struct Node: Identifiable, Hashable {
        let id: String
        let label: String
        /// The original (un-truncated) folder path for source nodes,
        /// or the bucket name for destinations. Used to match against
        /// the hover bus, which keys on full identity.
        let identityKey: String
        let count: Int
        let icon: String
        let tint: Color
        let isSource: Bool
    }

    /// One ribbon connecting a source to a destination.
    private struct Flow: Identifiable {
        var id: String { "\(sourceID)→\(destID)" }
        let sourceID: String
        let destID: String
        /// Identity keys passed up to the hover bus when hovered.
        let sourceFolder: String
        let destBucket: String
        let count: Int
    }

    // MARK: - Data

    /// Cap to top N nodes per column. The long tail rolls up into a
    /// single "Other folders" placeholder so the diagram stays
    /// readable even on a 200-folder library. Without this cap, 28+
    /// nodes per column produce spaghetti ribbons that obscure the
    /// labels they're supposed to connect.
    private static let topN: Int = 8
    private static let otherSourceID = "src:__other__"
    private static let otherDestID = "dst:__other__"

    private var sources: [Node] {
        let bySource = Dictionary(grouping: proposals, by: { $0.sourceFolder })
        let raw = bySource
            .map { (folder, props) -> (id: String, folder: String, count: Int, isJunk: Bool) in
                let isJunk = props.allSatisfy { $0.kind == .dissolved }
                return (id: "src:\(folder)", folder: folder, count: props.count, isJunk: isJunk)
            }
            .sorted { $0.count > $1.count }
        let visible = raw.prefix(Self.topN)
        let tail = raw.dropFirst(Self.topN)
        var nodes: [Node] = visible.map { entry in
            let display = (entry.folder as NSString).lastPathComponent
            return Node(
                id: entry.id,
                label: display.isEmpty ? entry.folder : display,
                identityKey: entry.folder,
                count: entry.count,
                icon: entry.isJunk ? "tray.2" : "tray.and.arrow.up",
                tint: entry.isJunk ? Theme.gold : .orange,
                isSource: true
            )
        }
        if !tail.isEmpty {
            let count = tail.reduce(0) { $0 + $1.count }
            nodes.append(Node(
                id: Self.otherSourceID,
                label: "+ \(tail.count) more folder\(tail.count == 1 ? "" : "s")",
                identityKey: Self.otherSourceID,
                count: count,
                icon: "ellipsis.circle",
                tint: .secondary,
                isSource: true
            ))
        }
        return nodes
    }

    private var destinations: [Node] {
        let byBucket = Dictionary(grouping: proposals, by: { $0.bucket })
        let raw = byBucket
            .map { (bucket, props) in (bucket: bucket, count: props.count) }
            .sorted { $0.count > $1.count }
        let visible = raw.prefix(Self.topN)
        let tail = raw.dropFirst(Self.topN)
        var nodes: [Node] = visible.map { entry in
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
        if !tail.isEmpty {
            let count = tail.reduce(0) { $0 + $1.count }
            nodes.append(Node(
                id: Self.otherDestID,
                label: "+ \(tail.count) more bucket\(tail.count == 1 ? "" : "s")",
                identityKey: Self.otherDestID,
                count: count,
                icon: "ellipsis.circle",
                tint: .secondary,
                isSource: false
            ))
        }
        return nodes
    }

    /// Map a long-tail source folder onto the rollup "Other" node so
    /// every flow has a valid endpoint in the rendered Sankey. Same
    /// for destinations.
    private func remapSourceID(_ folder: String, visibleSources: Set<String>) -> String {
        let id = "src:\(folder)"
        return visibleSources.contains(id) ? id : Self.otherSourceID
    }

    private func remapDestID(_ bucket: String, visibleDests: Set<String>) -> String {
        let id = "dst:\(bucket)"
        return visibleDests.contains(id) ? id : Self.otherDestID
    }

    private var flows: [Flow] {
        let visibleSources = Set(sources.map(\.id))
        let visibleDests = Set(destinations.map(\.id))
        // Group by the (remapped) source/dest pair so every long-tail
        // flow contributes to a single ribbon into the rollup nodes.
        // Carry the identity keys forward so the hover bus can name
        // the exact folder/bucket a ribbon represents.
        struct Pair {
            let src: String
            let dst: String
            let sourceFolder: String
            let destBucket: String
        }
        let pairs: [Pair] = proposals.map { p in
            Pair(
                src: remapSourceID(p.sourceFolder, visibleSources: visibleSources),
                dst: remapDestID(p.bucket, visibleDests: visibleDests),
                sourceFolder: p.sourceFolder,
                destBucket: p.bucket
            )
        }
        let grouped = Dictionary(grouping: pairs, by: { "\($0.src)→\($0.dst)" })
        return grouped.map { _, group -> Flow in
            let first = group[0]
            return Flow(
                sourceID: first.src,
                destID: first.dst,
                sourceFolder: first.sourceFolder,
                destBucket: first.destBucket,
                count: group.count
            )
        }
    }

    private func bucketIcon(_ bucket: String) -> String {
        if bucket.hasPrefix("People")    { return "person.crop.circle.fill" }
        if bucket.hasPrefix("Places")    { return "mappin.circle.fill" }
        if bucket.hasPrefix("Documents") { return "doc.text.fill" }
        if bucket.hasPrefix("Photos")    { return "photo.stack.fill" }
        return "tray.fill"
    }

    // MARK: - Body

    var body: some View {
        let srcs = sources
        let dsts = destinations
        let fls = flows

        if srcs.isEmpty || dsts.isEmpty {
            EmptyView()
        } else {
            GeometryReader { geo in
                let totalH = geo.size.height
                let nodeColW: CGFloat = 170
                let ribbonAreaW = max(40, geo.size.width - nodeColW * 2 - 16)

                let srcSlots = layoutSlots(nodes: srcs, totalHeight: totalH)
                let dstSlots = layoutSlots(nodes: dsts, totalHeight: totalH)

                ZStack(alignment: .topLeading) {
                    // Ribbons — drawn first so node rects sit on top.
                    ForEach(fls) { flow in
                        if let srcSlot = srcSlots[flow.sourceID],
                           let dstSlot = dstSlots[flow.destID] {
                            ribbon(
                                from: CGPoint(x: nodeColW, y: srcSlot.midY),
                                to: CGPoint(x: nodeColW + ribbonAreaW + 16,
                                              y: dstSlot.midY),
                                thickness: ribbonThickness(for: flow.count,
                                                            allFlows: fls,
                                                            totalHeight: totalH),
                                isHighlighted: isHighlighted(flow),
                                isDimmed: hasAnyHighlight && !isHighlighted(flow)
                            )
                            .onHover { hovering in
                                withAnimation(.easeInOut(duration: 0.18)) {
                                    hoverBus.set(hovering
                                        ? .flow(sourceFolder: flow.sourceFolder,
                                                 destBucket: flow.destBucket)
                                        : nil)
                                }
                            }
                            .help("\(flow.count) file\(flow.count == 1 ? "" : "s") going from \(label(forID: flow.sourceID, in: srcs)) to \(label(forID: flow.destID, in: dsts))")
                        }
                    }

                    // Source column.
                    ForEach(srcs) { node in
                        if let slot = srcSlots[node.id] {
                            nodeView(node, atSlot: slot,
                                      isFocused: nodeIsFocused(node))
                                .frame(width: nodeColW)
                                .position(x: nodeColW / 2,
                                            y: slot.midY)
                                .onTapGesture { onTapSource(node.label) }
                                .onHover { hovering in
                                    withAnimation(.easeInOut(duration: 0.18)) {
                                        hoverBus.set(hovering && node.id != Self.otherSourceID
                                            ? .sourceFolder(node.identityKey)
                                            : (hovering ? nil : nil))
                                    }
                                }
                        }
                    }

                    // Destination column.
                    ForEach(dsts) { node in
                        if let slot = dstSlots[node.id] {
                            nodeView(node, atSlot: slot,
                                      isFocused: nodeIsFocused(node))
                                .frame(width: nodeColW)
                                .position(x: geo.size.width - nodeColW / 2,
                                            y: slot.midY)
                                .onTapGesture { onTapDestination(node.label) }
                                .onHover { hovering in
                                    withAnimation(.easeInOut(duration: 0.18)) {
                                        hoverBus.set(hovering && node.id != Self.otherDestID
                                            ? .destBucket(node.identityKey)
                                            : (hovering ? nil : nil))
                                    }
                                }
                        }
                    }
                }
            }
            // Hard-bound height + .clipped() — `.position(x:y:)` inside
            // a GeometryReader doesn't respect the parent frame, so a
            // node positioned at y > frameHeight will render past the
            // bounds and visually overlap the next card. Clipping
            // forces the Sankey to stay inside its allotted slot.
            .frame(height: sankeyHeight(srcs: srcs, dsts: dsts))
            .clipped()
        }
    }

    /// One source/destination node — rounded rect with icon + label +
    /// count. Tapping opens the per-file drill-down for that node.
    /// `isFocused` lights up the node + its border when something
    /// connected to it is hovered (cross-highlight bus).
    @ViewBuilder
    private func nodeView(_ node: Node, atSlot slot: Slot, isFocused: Bool) -> some View {
        HStack(spacing: 6) {
            Image(systemName: node.icon)
                .font(.caption)
                .foregroundStyle(node.tint)
            VStack(alignment: .leading, spacing: 1) {
                Text(node.label)
                    .font(.caption.bold())
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text("\(node.count) file\(node.count == 1 ? "" : "s")")
                    .font(.system(size: 9).monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(node.tint.opacity(isFocused ? 0.22 : 0.10))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 6)
                .stroke(node.tint.opacity(isFocused ? 0.85 : 0.4),
                          lineWidth: isFocused ? 1.5 : 1)
        )
        .scaleEffect(isFocused ? 1.03 : 1.0)
        .shadow(color: isFocused ? node.tint.opacity(0.45) : .clear,
                  radius: isFocused ? 6 : 0)
        // Slot height is authoritative — no `max(28, …)` floor that
        // could overflow the slot bounds and overlap the next node.
        .frame(height: slot.height)
        .contentShape(Rectangle())
        .help("\(node.count) file\(node.count == 1 ? "" : "s") · tap to see them")
    }

    // MARK: - Cross-highlight bus

    /// Any kind of highlight currently active (anything in the bus).
    private var hasAnyHighlight: Bool {
        hoverBus.context != nil
    }

    /// True when this node should glow because of the current hover
    /// context. Three cases focus a node:
    ///   1. The bus names this exact source folder / destination bucket.
    ///   2. The bus names a flow whose endpoint is this node.
    ///   3. The bus names a different node on the OTHER column that
    ///      this node sends to / receives from (cross-highlight).
    ///   4. The bus names an outcome class that this source folder
    ///      contributes to (so cards lighting up reach back to nodes).
    private func nodeIsFocused(_ node: Node) -> Bool {
        switch hoverBus.context {
        case .none:
            return false
        case .sourceFolder(let folder):
            if node.isSource { return node.identityKey == folder }
            // Destination side: highlight if this bucket receives any
            // proposal from `folder`.
            return proposals.contains { $0.sourceFolder == folder && $0.bucket == node.identityKey }
        case .destBucket(let bucket):
            if !node.isSource { return node.identityKey == bucket }
            return proposals.contains { $0.bucket == bucket && $0.sourceFolder == node.identityKey }
        case .flow(let folder, let bucket):
            if node.isSource { return node.identityKey == folder }
            return node.identityKey == bucket
        case .outcome(let outcome):
            // Source nodes glow if any of their proposals match the
            // outcome class. Destinations glow if any proposal in the
            // bucket matches.
            if node.isSource {
                return proposals.contains {
                    $0.sourceFolder == node.identityKey && Self.outcome(for: $0) == outcome
                }
            }
            return proposals.contains {
                $0.bucket == node.identityKey && Self.outcome(for: $0) == outcome
            }
        }
    }

    /// Local copy of `RestructureView.outcomeFor(_:)` — duplicated
    /// here so the Sankey doesn't have to reach back into the parent
    /// view. `.keep` cards aren't reflected in proposals so they
    /// can't drive node focus.
    private static func outcome(for p: RestructureView.Proposal) -> RestructureOutcome {
        switch p.kind {
        case .dissolved:         return .reorganize
        case .movedOutAsOutlier: return .tidy
        }
    }

    // MARK: - Layout helpers

    private struct Slot {
        let topY: CGFloat
        let height: CGFloat
        var midY: CGFloat { topY + height / 2 }
    }

    /// Compute vertical slots for a column of nodes. Heights are
    /// proportional to node count; min 28pt so labels stay readable.
    /// CRITICAL: the sum of all slot heights + gaps NEVER exceeds
    /// `totalHeight`. Two passes guarantee min-heights, then a final
    /// scale-down clamps the total if the proportional pass overflowed.
    private func layoutSlots(nodes: [Node], totalHeight: CGFloat) -> [String: Slot] {
        guard !nodes.isEmpty else { return [:] }
        let gap: CGFloat = 6
        let totalCount = nodes.reduce(0) { $0 + $1.count }
        let preferredMin: CGFloat = 28
        let absoluteMin: CGFloat = 18
        let availableHeight = max(0, totalHeight - CGFloat(nodes.count - 1) * gap)

        // If even the absolute minimum doesn't fit (impossibly tight),
        // distribute the available height equally and bail.
        let absMinTotal = absoluteMin * CGFloat(nodes.count)
        if availableHeight < absMinTotal {
            let h = max(0, availableHeight / CGFloat(nodes.count))
            var result: [String: Slot] = [:]
            var y: CGFloat = 0
            for n in nodes {
                result[n.id] = Slot(topY: y, height: h)
                y += h + gap
            }
            return result
        }

        // First pass — assign preferred min to small nodes.
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
        // Second pass — distribute remaining proportional to count.
        let flexAvailable = max(0, availableHeight - fixedTotal)
        for (i, n) in nodes.enumerated() {
            if heights[i] == 0 {
                let h = flexCountSum > 0
                    ? max(preferredMin, flexAvailable * CGFloat(n.count) / CGFloat(flexCountSum))
                    : preferredMin
                heights[i] = h
            }
        }

        // Third pass — hard clamp. If the total exceeds availableHeight
        // (e.g. preferredMin × N > availableHeight), scale every slot
        // proportionally so they all fit. Floor at absoluteMin so labels
        // stay readable but slots never overlap.
        let computedTotal = heights.reduce(0, +)
        if computedTotal > availableHeight {
            let scale = availableHeight / computedTotal
            for i in 0..<heights.count {
                heights[i] = max(absoluteMin, heights[i] * scale)
            }
            // After flooring, sum may again exceed availableHeight if
            // many nodes hit the floor. Final equalize at absoluteMin.
            let total2 = heights.reduce(0, +)
            if total2 > availableHeight {
                let h = availableHeight / CGFloat(nodes.count)
                heights = Array(repeating: h, count: nodes.count)
            }
        }

        var result: [String: Slot] = [:]
        var y: CGFloat = 0
        for (i, n) in nodes.enumerated() {
            result[n.id] = Slot(topY: y, height: heights[i])
            y += heights[i] + gap
        }
        return result
    }

    private func sankeyHeight(srcs: [Node], dsts: [Node]) -> CGFloat {
        let maxNodes = max(srcs.count, dsts.count)
        // Each node ~38pt with gap; capped to 380pt so the diagram
        // can't dominate the screen but always has room for the full
        // top-N + rollup node without forcing the layout to scale
        // every node below the readable floor.
        return min(380, max(220, CGFloat(maxNodes) * 38))
    }

    private func ribbonThickness(for count: Int, allFlows: [Flow],
                                   totalHeight: CGFloat) -> CGFloat {
        let total = allFlows.reduce(0) { $0 + $1.count }
        guard total > 0 else { return 2 }
        let ratio = CGFloat(count) / CGFloat(total)
        // Ribbons 2…16pt; favors visibility for small flows while
        // letting big flows dominate.
        return max(2, min(16, ratio * totalHeight * 0.6))
    }

    private func isHighlighted(_ flow: Flow) -> Bool {
        switch hoverBus.context {
        case .none:
            return false
        case .flow(let folder, let bucket):
            return flow.sourceFolder == folder && flow.destBucket == bucket
        case .sourceFolder(let folder):
            return flow.sourceFolder == folder
        case .destBucket(let bucket):
            return flow.destBucket == bucket
        case .outcome(let outcome):
            // A ribbon highlights if any of its underlying proposals
            // belong to that outcome.
            return proposals.contains {
                $0.sourceFolder == flow.sourceFolder
                    && $0.bucket == flow.destBucket
                    && Self.outcome(for: $0) == outcome
            }
        }
    }

    private func label(forID id: String, in nodes: [Node]) -> String {
        nodes.first(where: { $0.id == id })?.label ?? id
    }

    /// Cubic-Bézier ribbon between two horizontal points. Rendered as
    /// a stroked path for thin flows and a filled rounded shape for
    /// thick flows so the visual reads as a continuous connection.
    @ViewBuilder
    private func ribbon(from start: CGPoint, to end: CGPoint,
                          thickness: CGFloat, isHighlighted: Bool,
                          isDimmed: Bool) -> some View {
        Path { p in
            p.move(to: start)
            let dx = end.x - start.x
            let c1 = CGPoint(x: start.x + dx * 0.5, y: start.y)
            let c2 = CGPoint(x: end.x - dx * 0.5, y: end.y)
            p.addCurve(to: end, control1: c1, control2: c2)
        }
        .stroke(
            LinearGradient(
                colors: [
                    Theme.gold.opacity(isHighlighted ? 0.95 : (isDimmed ? 0.04 : 0.22)),
                    Theme.gold.opacity(isHighlighted ? 0.55 : (isDimmed ? 0.02 : 0.10))
                ],
                startPoint: .leading, endPoint: .trailing
            ),
            style: StrokeStyle(lineWidth: thickness,
                                 lineCap: .round, lineJoin: .round)
        )
        .animation(.easeInOut(duration: 0.18), value: isHighlighted)
        .animation(.easeInOut(duration: 0.18), value: isDimmed)
    }
}
