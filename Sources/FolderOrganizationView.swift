import SwiftUI
import SwiftData

// MARK: - Main View

struct FolderOrganizationView: View {
    @ObservedObject var viewModel: AppViewModel
    @State private var selectedScenario: String = "Semantic"
    @Query private var allFiles: [FileRecord]
    
    // Canvas state
    @State private var canvasOffset: CGSize = .zero
    @State private var dragOffset: CGSize = .zero
    @State private var zoomScale: CGFloat = 0.7
    @State private var expandedNodes: Set<String> = []
    
    var eligibleFiles: [FileRecord] {
        allFiles.filter { $0.status == .completed || $0.status == .reviewRequired || $0.status == .namingRequired }
    }
    
    /// Builds the tree of proposed folder groups
    var proposedTree: [FolderNode] {
        let grouped: [String: [FileRecord]]
        
        switch selectedScenario {
        case "Timeline":
            grouped = Dictionary(grouping: eligibleFiles) { file in
                let year = Calendar.current.component(.year, from: file.creationDate)
                let month = Calendar.current.component(.month, from: file.creationDate)
                let monthName = Calendar.current.shortMonthSymbols[month - 1]
                return "\(year)/\(monthName)"
            }
        case "Hybrid":
            grouped = Dictionary(grouping: eligibleFiles) { file in
                let year = Calendar.current.component(.year, from: file.creationDate)
                let cat = Self.categoryName(for: file)
                return "\(year)/\(cat)"
            }
        default:
            grouped = Dictionary(grouping: eligibleFiles) { Self.categoryName(for: $0) }
        }
        
        return grouped.map { key, files in
            let parts = key.components(separatedBy: "/")
            return FolderNode(
                id: key,
                name: parts.last ?? key,
                icon: iconFor(category: parts.last ?? ""),
                color: colorFor(category: parts.last ?? ""),
                children: files.map { FileNode(id: $0.id.uuidString, name: $0.filename, file: $0) },
                parentPath: parts.count > 1 ? parts.first : nil
            )
        }.sorted { $0.children.count > $1.children.count }
    }
    
    /// Builds the original folder structure from current file locations
    var originalTree: [FolderNode] {
        guard let root = viewModel.currentFolderURL else { return [] }
        let grouped = Dictionary(grouping: eligibleFiles) { file -> String in
            let relative = file.url.deletingLastPathComponent().path.replacingOccurrences(of: root.path, with: "")
            return relative.isEmpty ? "/" : relative
        }
        return grouped.map { path, files in
            FolderNode(
                id: "orig_\(path)",
                name: path == "/" ? root.lastPathComponent : String(path.split(separator: "/").last ?? "root"),
                icon: "folder.fill",
                color: .gray,
                children: files.map { FileNode(id: "orig_\($0.id.uuidString)", name: $0.filename, file: $0) },
                parentPath: nil
            )
        }.sorted { $0.name < $1.name }
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Control Bar
            controlBar
            
            if eligibleFiles.isEmpty {
                emptyState
            } else {
                // Knowledge Graph Canvas
                GeometryReader { geo in
                    ZStack {
                        // Dot grid background
                        DotGridCanvas()
                            .scaleEffect(zoomScale)
                            .offset(canvasTranslation)
                        
                        // Node graph content
                        graphContent(in: geo.size)
                            .scaleEffect(zoomScale)
                            .offset(canvasTranslation)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .clipped()
                    .contentShape(Rectangle())
                    .gesture(panGesture)
                    .gesture(MagnificationGesture().onChanged { val in
                        zoomScale = min(max(val, 0.15), 2.5)
                    })
                    .onAppear {
                        // Center the canvas
                        canvasOffset = CGSize(width: -200, height: 0)
                    }
                    .overlay(alignment: .bottomTrailing) {
                        // Minimap
                        ZStack {
                            RoundedRectangle(cornerRadius: 12)
                                .fill(.black.opacity(0.6))
                                .frame(width: 180, height: 120)
                            
                            // Simplified representation
                            HStack(spacing: 40) {
                                RoundedRectangle(cornerRadius: 2).fill(.red.opacity(0.3)).frame(width: 30, height: 80)
                                RoundedRectangle(cornerRadius: 2).fill(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.3)).frame(width: 30, height: 80)
                            }
                            
                            // Current view indicator
                            RoundedRectangle(cornerRadius: 4)
                                .stroke(Color(red: 1.0, green: 0.8, blue: 0.0), lineWidth: 1.5)
                                .frame(width: 60, height: 40)
                                .offset(x: -canvasOffset.width / 20, y: -canvasOffset.height / 20)
                        }
                        .padding(24)
                        .shadow(radius: 10)
                        .opacity(zoomScale < 1.0 ? 1.0 : 0.0)
                        .animation(.spring(), value: zoomScale)
                    }
                }
                .background(Color(white: 0.04))
                
                // Bottom status bar
                statusBar
            }
        }
    }
    
    // MARK: - Computed Helpers
    
    var canvasTranslation: CGSize {
        CGSize(
            width: (canvasOffset.width + dragOffset.width) * zoomScale,
            height: (canvasOffset.height + dragOffset.height) * zoomScale
        )
    }
    
    var panGesture: some Gesture {
        DragGesture()
            .onChanged { value in dragOffset = value.translation }
            .onEnded { value in
                canvasOffset.width += value.translation.width
                canvasOffset.height += value.translation.height
                dragOffset = .zero
            }
    }
    
    // MARK: - Graph Content
    
    func graphContent(in size: CGSize) -> some View {
        HStack(alignment: .top, spacing: 0) {
            // LEFT: Before (Original Structure)
            VStack(alignment: .leading, spacing: 0) {
                sectionHeader(title: "CURRENT", subtitle: "On Disk", icon: "tray.full.fill", color: .red)
                
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(originalTree) { node in
                        GraphFolderNode(
                            node: node,
                            expandedNodes: $expandedNodes,
                            viewModel: viewModel,
                            isSource: true
                        )
                    }
                }
                .padding(.top, 16)
            }
            .frame(width: 420)
            .padding(30)
            
            // CENTER: Connection lines
            connectionLines(width: 300, height: max(CGFloat(max(originalTree.count, proposedTree.count)) * 120, 600))
                .frame(width: 300)
            
            // RIGHT: After (AI Proposal)
            VStack(alignment: .leading, spacing: 0) {
                sectionHeader(title: "AI PROPOSAL", subtitle: selectedScenario, icon: "wand.and.stars", color: Color(red: 1.0, green: 0.8, blue: 0.0))
                
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(proposedTree) { node in
                        GraphFolderNode(
                            node: node,
                            expandedNodes: $expandedNodes,
                            viewModel: viewModel,
                            isSource: false
                        )
                    }
                }
                .padding(.top, 16)
            }
            .frame(width: 420)
            .padding(30)
        }
        .padding(60)
    }
    
    // MARK: - Connection Lines
    
    func connectionLines(width: CGFloat, height: CGFloat) -> some View {
        Canvas { ctx, size in
            let leftCount = max(originalTree.count, 1)
            let rightCount = max(proposedTree.count, 1)
            let leftSpacing = size.height / CGFloat(leftCount + 1)
            let rightSpacing = size.height / CGFloat(rightCount + 1)
            
            // Draw flowing Bezier curves from each left node to each right node
            for (li, _) in originalTree.enumerated() {
                let startY = leftSpacing * CGFloat(li + 1)
                let start = CGPoint(x: 0, y: startY)
                
                for (ri, _) in proposedTree.enumerated() {
                    let endY = rightSpacing * CGFloat(ri + 1)
                    let end = CGPoint(x: size.width, y: endY)
                    
                    var path = Path()
                    path.move(to: start)
                    
                    let cp1 = CGPoint(x: size.width * 0.4, y: startY)
                    let cp2 = CGPoint(x: size.width * 0.6, y: endY)
                    path.addCurve(to: end, control1: cp1, control2: cp2)
                    
                    ctx.stroke(
                        path,
                        with: .color(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.12)),
                        style: StrokeStyle(lineWidth: 1.5, dash: [6, 4])
                    )
                }
            }
            
            // Highlight the primary flow line (thicker, glowing)
            if let firstLeft = originalTree.first, let firstRight = proposedTree.first {
                let startY = leftSpacing
                let endY = rightSpacing
                let start = CGPoint(x: 0, y: startY)
                let end = CGPoint(x: size.width, y: endY)
                
                var path = Path()
                path.move(to: start)
                path.addCurve(to: end, control1: CGPoint(x: size.width * 0.4, y: startY), control2: CGPoint(x: size.width * 0.6, y: endY))
                
                // Glow
                ctx.stroke(path, with: .color(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.25)), style: StrokeStyle(lineWidth: 4))
                let _ = firstLeft.id + firstRight.id // suppress unused warning
            }
        }
        .frame(height: height)
    }
    
    // MARK: - Subviews
    
    var controlBar: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Image(systemName: "point.3.connected.trianglepath.dotted")
                        .font(.title2)
                        .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                    Text("Knowledge Graph")
                        .font(.title2.bold())
                }
                Text("Drag to pan · Pinch to zoom · Click folders to expand")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            
            Picker("Scenario", selection: $selectedScenario) {
                Text("By Category").tag("Semantic")
                Text("By Date").tag("Timeline")
                Text("Date + Category").tag("Hybrid")
            }
            .pickerStyle(.segmented)
            .frame(width: 300)
            
            Spacer()
            
            // Zoom controls
            HStack(spacing: 8) {
                Button { withAnimation { zoomScale = max(zoomScale - 0.15, 0.15) } } label: {
                    Image(systemName: "minus.magnifyingglass")
                }
                .buttonStyle(.bordered)
                
                Text("\(Int(zoomScale * 100))%")
                    .font(.caption.monospacedDigit())
                    .frame(width: 40)
                
                Button { withAnimation { zoomScale = min(zoomScale + 0.15, 2.5) } } label: {
                    Image(systemName: "plus.magnifyingglass")
                }
                .buttonStyle(.bordered)
                
                Button { withAnimation { zoomScale = 0.7; canvasOffset = CGSize(width: -200, height: 0) } } label: {
                    Image(systemName: "arrow.counterclockwise")
                }
                .buttonStyle(.bordered)
            }
            
            Button {
                Task { await viewModel.applyFolderStructure() }
            } label: {
                Label("Execute", systemImage: "arrow.right.circle.fill")
                    .fontWeight(.bold)
            }
            .buttonStyle(.borderedProminent)
            .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
            .foregroundStyle(.black)
            .disabled(viewModel.isProcessing || eligibleFiles.isEmpty)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .background(.ultraThinMaterial)
    }
    
    func sectionHeader(title: String, subtitle: String, icon: String, color: Color) -> some View {
        HStack(spacing: 10) {
            Image(systemName: icon)
                .font(.title3)
                .foregroundStyle(color)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 13, weight: .heavy, design: .monospaced))
                    .foregroundStyle(color)
                Text(subtitle)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(RoundedRectangle(cornerRadius: 10).fill(color.opacity(0.1)))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(color.opacity(0.3), lineWidth: 1))
    }
    
    var statusBar: some View {
        HStack {
            Label("\(eligibleFiles.count) files analyzed", systemImage: "doc.on.doc.fill")
            Text("·").foregroundStyle(.tertiary)
            Label("\(proposedTree.count) categories", systemImage: "folder.fill")
            Text("·").foregroundStyle(.tertiary)
            let totalMB = eligibleFiles.reduce(0) { $0 + $1.fileSizeMB }
            Text(String(format: "%.1f MB total", totalMB))
            Spacer()
            Text("Zoom: \(Int(zoomScale * 100))%")
                .font(.caption.monospacedDigit())
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
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 60))
                .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.5))
            Text("No Files Ready")
                .font(.headline)
                .foregroundStyle(.secondary)
            Text("Process a folder first — the knowledge graph will appear once the AI has analyzed your files.")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 400)
            Spacer()
        }
    }
    
    // MARK: - Category Helpers
    
    static func categoryName(for file: FileRecord) -> String {
        let ext = file.url.pathExtension.lowercased()
        if ext == "pdf" {
            if file.aiTags.contains("Invoice") { return "Invoices" }
            if file.aiTags.contains("Receipt") { return "Receipts" }
            if file.aiTags.contains("Tax_Document") { return "Taxes" }
            return "Documents"
        }
        if file.aiTags.contains("Screenshot") { return "Screenshots" }
        if ["mp4", "mov"].contains(ext) { return "Videos" }
        if file.hasFaces { return "People" }
        if file.aiTags.contains(where: { ["Landscape", "Outdoor", "Nature", "Mountain", "Beach", "Sky"].contains($0) }) {
            return "Nature"
        }
        if file.aiTags.contains(where: { ["Food", "Cooking"].contains($0) }) { return "Food" }
        if file.aiTags.contains(where: { ["Dog", "Cat", "Animal"].contains($0) }) { return "Animals" }
        return "Photos"
    }
    
    func iconFor(category: String) -> String {
        switch category {
        case "Invoices", "Receipts", "Taxes", "Documents": return "doc.text.fill"
        case "Screenshots": return "rectangle.dashed.and.paperclip"
        case "Videos": return "film.fill"
        case "People": return "person.2.fill"
        case "Nature": return "leaf.fill"
        case "Food": return "fork.knife"
        case "Animals": return "pawprint.fill"
        default: return "photo.fill"
        }
    }
    
    func colorFor(category: String) -> Color {
        switch category {
        case "Invoices", "Receipts", "Taxes", "Documents": return .orange
        case "Screenshots": return .purple
        case "Videos": return .pink
        case "People": return .cyan
        case "Nature": return .green
        case "Food": return .yellow
        case "Animals": return .mint
        default: return Color(red: 1.0, green: 0.8, blue: 0.0)
        }
    }
}

// MARK: - Data Models

struct FolderNode: Identifiable {
    let id: String
    let name: String
    let icon: String
    let color: Color
    let children: [FileNode]
    let parentPath: String?
}

struct FileNode: Identifiable {
    let id: String
    let name: String
    let file: FileRecord
}

// MARK: - Graph Folder Node Card

struct GraphFolderNode: View {
    let node: FolderNode
    @Binding var expandedNodes: Set<String>
    @ObservedObject var viewModel: AppViewModel
    let isSource: Bool
    
    var isExpanded: Bool { expandedNodes.contains(node.id) }
    
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Folder header
            Button {
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                    if expandedNodes.contains(node.id) {
                        expandedNodes.remove(node.id)
                    } else {
                        expandedNodes.insert(node.id)
                    }
                }
            } label: {
                HStack(spacing: 10) {
                    // Expand/collapse chevron
                    Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(.secondary)
                        .frame(width: 14)
                    
                    Image(systemName: node.icon)
                        .font(.system(size: 16))
                        .foregroundStyle(node.color)
                    
                    VStack(alignment: .leading, spacing: 2) {
                        Text(node.name)
                            .font(.system(size: 13, weight: .bold, design: .rounded))
                            .foregroundStyle(.white)
                            .lineLimit(1)
                        
                        Text("\(node.children.count) file\(node.children.count == 1 ? "" : "s")")
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                    }
                    
                    Spacer()
                    
                    // Size badge
                    let totalMB = node.children.reduce(0.0) { $0 + $1.file.fileSizeMB }
                    Text(String(format: "%.1f MB", totalMB))
                        .font(.system(size: 9, weight: .medium, design: .monospaced))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(RoundedRectangle(cornerRadius: 4).fill(node.color.opacity(0.15)))
                        .foregroundStyle(node.color)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
            }
            .buttonStyle(.plain)
            
            // Expanded children
            if isExpanded {
                VStack(alignment: .leading, spacing: 1) {
                    ForEach(node.children.prefix(30)) { child in
                        HStack(spacing: 8) {
                            // Connection dot
                            Circle()
                                .fill(node.color.opacity(0.4))
                                .frame(width: 6, height: 6)
                            
                            // Thumbnail
                            ThumbnailView(url: child.file.url)
                                .frame(width: 28, height: 28)
                                .clipShape(RoundedRectangle(cornerRadius: 4))
                            
                            Text(child.name)
                                .font(.system(size: 11))
                                .foregroundStyle(.white.opacity(0.8))
                                .lineLimit(1)
                                .truncationMode(.middle)
                            
                            Spacer()
                            
                            // Status indicator
                            if child.file.status == .completed {
                                Image(systemName: "checkmark.circle.fill")
                                    .font(.system(size: 10))
                                    .foregroundStyle(.green.opacity(0.7))
                            }
                        }
                        .padding(.horizontal, 14)
                        .padding(.leading, 24)
                        .padding(.vertical, 4)
                        .onTapGesture(count: 2) {
                            withAnimation(.spring()) { viewModel.previewFile = child.file }
                        }
                    }
                    
                    if node.children.count > 30 {
                        Text("+ \(node.children.count - 30) more")
                            .font(.system(size: 10))
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 14)
                            .padding(.leading, 24)
                            .padding(.vertical, 4)
                    }
                }
                .padding(.bottom, 8)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .background(
            RoundedRectangle(cornerRadius: 14)
                .fill(.ultraThinMaterial)
                .shadow(color: node.color.opacity(isExpanded ? 0.25 : 0.1), radius: isExpanded ? 15 : 8, y: 4)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 14)
                .stroke(node.color.opacity(isExpanded ? 0.5 : 0.2), lineWidth: isExpanded ? 1.5 : 1)
        )
    }
}

// MARK: - Dot Grid Background

struct DotGridCanvas: View {
    var body: some View {
        Canvas { context, size in
            let dotSize: CGFloat = 1.5
            let spacing: CGFloat = 35
            
            for x in stride(from: -3000 as CGFloat, through: 3000, by: spacing) {
                for y in stride(from: -3000 as CGFloat, through: 3000, by: spacing) {
                    let rect = CGRect(x: x, y: y, width: dotSize, height: dotSize)
                    context.fill(Path(ellipseIn: rect), with: .color(Color.white.opacity(0.06)))
                }
            }
        }
        .frame(width: 6000, height: 6000)
    }
}
