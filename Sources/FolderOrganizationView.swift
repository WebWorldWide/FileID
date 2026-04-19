import SwiftUI

struct FolderOrganizationView: View {
    @ObservedObject var viewModel: AppViewModel
    @State private var selectedScenario: String = "Semantic"
    
    // Infinite Canvas State
    @State private var canvasOffset: CGSize = .zero
    @State private var dragOffset: CGSize = .zero
    @State private var zoomScale: CGFloat = 1.0
    
    var body: some View {
        VStack(spacing: 0) {
            // Top Control Bar
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Canvas 2.0: AI Restructuring")
                        .font(.title2.bold())
                    Text("Drag to pan, pinch to zoom. Review the master plan before execution.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                
                Picker("Scenario", selection: $selectedScenario) {
                    Text("Semantic Clustering").tag("Semantic")
                    Text("Timeline (Year/Month)").tag("Timeline")
                    Text("Hybrid").tag("Hybrid")
                }
                .pickerStyle(.segmented)
                .frame(width: 350)
                
                Spacer()
                
                Button {
                    Task { await viewModel.applyFolderStructure() }
                } label: {
                    Text("Execute Restructuring")
                        .fontWeight(.bold)
                        .padding(.horizontal, 10)
                }
                .buttonStyle(.borderedProminent)
                .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                .foregroundStyle(.black)
                .disabled(viewModel.isProcessing)
            }
            .padding()
            .background(Color.black.opacity(0.4))
            
            // 2D Infinite Canvas
            if viewModel.folderProposals.isEmpty {
                VStack {
                    Spacer()
                    ProgressView().tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                        .controlSize(.large)
                    Text("Drafting the Master Plan...")
                        .font(.headline)
                        .padding()
                    Spacer()
                }
            } else {
                GeometryReader { geo in
                    ZStack(alignment: .center) {
                        // Background Grid
                        CanvasGrid()
                            .scaleEffect(zoomScale)
                            .offset(x: (canvasOffset.width + dragOffset.width) * zoomScale,
                                    y: (canvasOffset.height + dragOffset.height) * zoomScale)
                        
                        // The actual Node Graph
                        NodeCanvas(viewModel: viewModel, scenario: selectedScenario)
                            .scaleEffect(zoomScale)
                            .offset(x: (canvasOffset.width + dragOffset.width) * zoomScale,
                                    y: (canvasOffset.height + dragOffset.height) * zoomScale)
                    }
                    .frame(width: geo.size.width, height: geo.size.height)
                    .contentShape(Rectangle())
                    .gesture(
                        DragGesture()
                            .onChanged { value in
                                dragOffset = value.translation
                            }
                            .onEnded { value in
                                canvasOffset.width += value.translation.width
                                canvasOffset.height += value.translation.height
                                dragOffset = .zero
                            }
                    )
                    .gesture(
                        MagnificationGesture()
                            .onChanged { val in zoomScale = min(max(val, 0.2), 3.0) }
                    )
                }
                .background(Color(white: 0.05))
            }
        }
    }
}

// Custom Arrow Shape for Bezier Curves
struct ArrowPath: Shape {
    var start: CGPoint
    var end: CGPoint
    
    func path(in rect: CGRect) -> Path {
        var p = Path()
        p.move(to: start)
        
        // Control points for a smooth left-to-right flow
        let c1 = CGPoint(x: start.x + abs(end.x - start.x) * 0.5, y: start.y)
        let c2 = CGPoint(x: end.x - abs(end.x - start.x) * 0.5, y: end.y)
        
        p.addCurve(to: end, control1: c1, control2: c2)
        
        // Calculate tangent angle at the end point for the arrowhead
        let dx = end.x - c2.x
        let dy = end.y - c2.y
        let angle = atan2(dy, dx)
        
        // Draw Arrowhead
        let arrowLength: CGFloat = 12
        let arrowAngle: CGFloat = .pi / 6 // 30 degrees
        
        let p1 = CGPoint(
            x: end.x - arrowLength * cos(angle - arrowAngle),
            y: end.y - arrowLength * sin(angle - arrowAngle)
        )
        let p2 = CGPoint(
            x: end.x - arrowLength * cos(angle + arrowAngle),
            y: end.y - arrowLength * sin(angle + arrowAngle)
        )
        
        p.move(to: end)
        p.addLine(to: p1)
        p.move(to: end)
        p.addLine(to: p2)
        
        return p
    }
}

struct NodeCanvas: View {
    @ObservedObject var viewModel: AppViewModel
    let scenario: String
    
    var body: some View {
        let grouped = Dictionary(grouping: viewModel.folderProposals.prefix(200)) { proposal -> String in
            let components = proposal.proposedPath.components(separatedBy: "/")
            if scenario == "Timeline" {
                return components.count > 1 ? components[1] : "Misc"
            }
            return components.first ?? "Misc"
        }
        
        let keys = grouped.keys.sorted()
        
        ZStack(alignment: .center) {
            HStack(alignment: .top, spacing: 180) { // Tightened from 350
                // ZONE 1: BEFORE (Current Mess)
                VStack {
                    Text("CURRENT MESS")
                        .font(.headline)
                        .foregroundStyle(.gray)
                        .padding(.bottom, 10)
                    
                    NodeBox(icon: "tray.full.fill", title: "Original Inbox", subtitle: "\(viewModel.folderProposals.count) unsorted files", color: .red)
                        .anchorPreference(key: NodeAnchorKey.self, value: .trailing, transform: { [NodeConnection(id: "ROOT", point: $0)] })
                }
                .frame(width: 220) // Smaller cards
                
                // ZONE 2: HIERARCHY LEVEL 1 (Years / Main Category)
                VStack(spacing: 40) { // Tightened from 120
                    Text("AI HARMONY")
                        .font(.headline)
                        .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                        .padding(.bottom, 10)
                    
                    ForEach(keys, id: \.self) { level1 in
                        NodeBox(icon: "folder.fill", title: level1, subtitle: "\(grouped[level1]!.count) files", color: .green)
                            .backgroundPreferenceValue(NodeAnchorKey.self) { anchors in
                                GeometryReader { geo in
                                    if let rootAnchor = anchors.first(where: { $0.id == "ROOT" }) {
                                        let start = geo[rootAnchor.point]
                                        let end = CGPoint(x: 0, y: geo.size.height / 2)
                                        
                                        ArrowPath(start: start, end: end)
                                            .stroke(Color(white: 0.4), style: StrokeStyle(lineWidth: 3, lineCap: .round))
                                    }
                                }
                            }
                            .anchorPreference(key: NodeAnchorKey.self, value: .trailing, transform: { [NodeConnection(id: level1, point: $0)] })
                            
                        // HIERARCHY LEVEL 2 (Months / Subcategories nested underneath Level 1)
                        let subGroup = Dictionary(grouping: grouped[level1]!) { proposal -> String in
                            let components = proposal.proposedPath.components(separatedBy: "/")
                            if components.count > 1 { return components[1] }
                            return "Root"
                        }
                        
                        let subKeys = subGroup.keys.sorted()
                        VStack(spacing: 20) { // Tightened
                            ForEach(subKeys, id: \.self) { level2 in
                                HStack(spacing: 120) {
                                    NodeBox(icon: "folder", title: level2, subtitle: "\(subGroup[level2]!.count) items", color: .mint)
                                        .frame(width: 180)
                                        .backgroundPreferenceValue(NodeAnchorKey.self) { anchors in
                                            GeometryReader { geo in
                                                if let parentAnchor = anchors.first(where: { $0.id == level1 }) {
                                                    let start = geo[parentAnchor.point]
                                                    let end = CGPoint(x: 0, y: geo.size.height / 2)
                                                    
                                                    ArrowPath(start: start, end: end)
                                                        .stroke(Color(white: 0.35), style: StrokeStyle(lineWidth: 2, lineCap: .round))
                                                }
                                            }
                                        }
                                        .anchorPreference(key: NodeAnchorKey.self, value: .trailing, transform: { [NodeConnection(id: "\(level1)_\(level2)", point: $0)] })
                                        
                                    // FILES (Attached to Level 2)
                                    VStack(spacing: 8) {
                                        ForEach(subGroup[level2]!.prefix(3)) { proposal in
                                            NodeBox(icon: "doc.fill", title: proposal.fileStatus.url.lastPathComponent, subtitle: proposal.fileStatus.aiTags.prefix(2).joined(separator: ", "), color: .cyan)
                                                .frame(width: 200)
                                                .backgroundPreferenceValue(NodeAnchorKey.self) { anchors in
                                                    GeometryReader { geo in
                                                        if let parentAnchor = anchors.first(where: { $0.id == "\(level1)_\(level2)" }) {
                                                            let start = geo[parentAnchor.point]
                                                            let end = CGPoint(x: 0, y: geo.size.height / 2)
                                                            
                                                            ArrowPath(start: start, end: end)
                                                                .stroke(Color(white: 0.3), style: StrokeStyle(lineWidth: 1.5, lineCap: .round, dash: [4, 4]))
                                                        }
                                                    }
                                                }
                                        }
                                        if subGroup[level2]!.count > 3 {
                                            Text("+ \(subGroup[level2]!.count - 3) more")
                                                .font(.system(size: 10))
                                                .foregroundStyle(.secondary)
                                        }
                                    }
                                }
                            }
                        }
                        .padding(.leading, 100) // Indent the nested hierarchy
                        .padding(.vertical, 20)
                    }
                }
                .frame(width: 260)
            }
            .padding(100)
        }
    }
}

struct NodeBox: View {
    let icon: String
    let title: String
    let subtitle: String
    let color: Color
    
    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: icon)
                .font(.title2)
                .foregroundStyle(color)
            
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.system(size: 14, weight: .bold, design: .rounded))
                    .foregroundStyle(.white)
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)
                
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding()
        .frame(maxWidth: .infinity)
        .background(
            RoundedRectangle(cornerRadius: 16)
                .fill(.ultraThinMaterial)
                .shadow(color: color.opacity(0.2), radius: 15, y: 5)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 16)
                .stroke(color.opacity(0.4), lineWidth: 1)
        )
    }
}

struct CanvasGrid: View {
    var body: some View {
        Canvas { context, size in
            let dotSize: CGFloat = 2
            let spacing: CGFloat = 40
            
            // Draw a very large grid relative to zero
            for x in stride(from: -4000, to: 4000, by: spacing) {
                for y in stride(from: -4000, to: 4000, by: spacing) {
                    let rect = CGRect(x: CGFloat(x), y: CGFloat(y), width: dotSize, height: dotSize)
                    context.fill(Path(ellipseIn: rect), with: .color(Color.white.opacity(0.08)))
                }
            }
        }
        .frame(width: 8000, height: 8000)
    }
}

struct NodeConnection: Equatable {
    let id: String
    let point: Anchor<CGPoint>
}

struct NodeAnchorKey: PreferenceKey {
    static var defaultValue: [NodeConnection] = []
    static func reduce(value: inout [NodeConnection], nextValue: () -> [NodeConnection]) {
        value.append(contentsOf: nextValue())
    }
}
