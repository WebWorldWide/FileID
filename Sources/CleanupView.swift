import SwiftUI
import SwiftData

struct CleanupView: View {
    @ObservedObject var viewModel: AppViewModel
    @Query private var allFiles: [FileRecord]
    @State private var selectedTab: CleanupTab = .junk
    @State private var showingUndoToast = false

    enum CleanupTab: String, CaseIterable {
        case junk = "Junk Files"
        case duplicates = "Duplicates"
        case screenshots = "Screenshots"
        case large = "Large Files"
    }

    // MARK: - Computed

    var screenshots: [FileRecord] {
        allFiles.filter { $0.aiTags.contains("Screenshot") && !$0.isTrashed }
    }
    var duplicates: [FileRecord] {
        allFiles.filter { $0.duplicateGroupUUID != nil && !$0.isTrashed }
    }
    var largeFiles: [FileRecord] {
        allFiles.filter { $0.fileSizeMB > 50 && !$0.isTrashed }
            .sorted { $0.fileSizeMB > $1.fileSizeMB }
    }
    var junkFiles: [FileRecord] {
        allFiles.filter { $0.isJunk && !$0.isTrashed }
    }

    var activeFiles: [FileRecord] {
        switch selectedTab {
        case .junk:        return junkFiles
        case .duplicates:  return duplicates
        case .screenshots: return screenshots
        case .large:       return largeFiles
        }
    }

    var totalReclaimableMB: Double {
        activeFiles.reduce(0) { $0 + $1.fileSizeMB }
    }

    // Category breakdown for pie chart
    var categoryBreakdown: [(label: String, mb: Double, color: Color)] {
        let s = screenshots.reduce(0.0) { $0 + $1.fileSizeMB }
        let d = duplicates.reduce(0.0) { $0 + $1.fileSizeMB }
        let j = junkFiles.reduce(0.0) { $0 + $1.fileSizeMB }
        let l = largeFiles.reduce(0.0) { $0 + $1.fileSizeMB }
        return [
            ("Screenshots", s, .purple),
            ("Duplicates",  d, .orange),
            ("Junk",        j, .red),
            ("Large Files", l, .cyan)
        ].filter { $0.mb > 0 }
    }

    var body: some View {
        VStack(spacing: 0) {
            // Top header + pie chart
            HStack(alignment: .top, spacing: 24) {
                // Left: title + tab picker + action button
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Image(systemName: "trash.slash.fill")
                            .font(.largeTitle)
                            .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Cleanup Center")
                                .font(.title.bold())
                            Text("\(junkFiles.count + duplicates.count + screenshots.count) files flagged")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }

                    Picker("Category", selection: $selectedTab) {
                        ForEach(CleanupTab.allCases, id: \.self) { tab in
                            Text(tab.rawValue).tag(tab)
                        }
                    }
                    .pickerStyle(.segmented)
                    .frame(maxWidth: 520)

                    // Batch action button
                    HStack(spacing: 10) {
                        Button {
                            trashAll()
                        } label: {
                            Label("Trash All \(activeFiles.count) Files", systemImage: "trash.fill")
                                .fontWeight(.semibold)
                        }
                        .buttonStyle(.borderedProminent)
                        .tint(.red)
                        .disabled(activeFiles.isEmpty)

                        Text(String(format: "Frees %.1f MB", totalReclaimableMB))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                Spacer()

                // Right: mini pie chart
                if !categoryBreakdown.isEmpty {
                    CleanupPieChart(segments: categoryBreakdown)
                        .frame(width: 140, height: 140)
                        .padding(.trailing, 8)
                }
            }
            .padding(20)
            .background(.ultraThinMaterial)

            // File list / grid
            if activeFiles.isEmpty {
                Spacer()
                VStack(spacing: 12) {
                    Image(systemName: "sparkles")
                        .font(.system(size: 56))
                        .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.5))
                    Text("Nothing here!")
                        .font(.headline)
                        .foregroundStyle(.secondary)
                    Text("No \(selectedTab.rawValue.lowercased()) detected in your scanned folder.")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
                Spacer()
            } else {
                ScrollView {
                    LazyVGrid(
                        columns: [GridItem(.adaptive(minimum: 150, maximum: 180), spacing: 14)],
                        spacing: 14
                    ) {
                        ForEach(activeFiles) { file in
                            CleanupFileCard(file: file, viewModel: viewModel)
                        }
                    }
                    .padding(20)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.3))
        .overlay(alignment: .bottom) {
            if showingUndoToast {
                UndoToast()
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .padding(.bottom, 20)
            }
        }
        .animation(.spring(response: 0.4), value: showingUndoToast)
    }

    // MARK: - Actions

    func trashAll() {
        for file in activeFiles {
            Task {
                if let _ = try? await NSWorkspace.shared.recycle([file.url]) {
                    await MainActor.run { file.isTrashed = true }
                }
            }
        }
        withAnimation { showingUndoToast = true }
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) {
            withAnimation { showingUndoToast = false }
        }
    }
}

// MARK: - Pie Chart

struct CleanupPieChart: View {
    let segments: [(label: String, mb: Double, color: Color)]

    var total: Double { segments.reduce(0) { $0 + $1.mb } }

    var body: some View {
        ZStack {
            Canvas { ctx, size in
                let center = CGPoint(x: size.width / 2, y: size.height / 2)
                let radius = min(size.width, size.height) / 2 - 4
                var startAngle = Angle.degrees(-90)

                for segment in segments {
                    let sweep = Angle.degrees(360 * segment.mb / max(total, 1))
                    let endAngle = startAngle + sweep

                    var path = Path()
                    path.move(to: center)
                    path.addArc(center: center, radius: radius,
                                startAngle: startAngle, endAngle: endAngle,
                                clockwise: false)
                    path.closeSubpath()
                    ctx.fill(path, with: .color(segment.color.opacity(0.85)))

                    // Segment border
                    ctx.stroke(path, with: .color(.black.opacity(0.3)), lineWidth: 1.5)
                    startAngle = endAngle
                }

                // Donut hole
                var hole = Path()
                hole.addEllipse(in: CGRect(x: size.width/2 - radius*0.45,
                                           y: size.height/2 - radius*0.45,
                                           width: radius*0.9, height: radius*0.9))
                ctx.fill(hole, with: .color(Color(white: 0.07)))
            }

            // Center label
            VStack(spacing: 0) {
                let totalGB = total / 1024
                if totalGB > 1 {
                    Text(String(format: "%.1f GB", totalGB))
                        .font(.system(size: 13, weight: .bold, design: .rounded))
                } else {
                    Text(String(format: "%.0f MB", total))
                        .font(.system(size: 13, weight: .bold, design: .rounded))
                }
                Text("reclaimable")
                    .font(.system(size: 8))
                    .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - Cleanup File Card

struct CleanupFileCard: View {
    @Bindable var file: FileRecord
    @ObservedObject var viewModel: AppViewModel
    @State private var thumbnail: NSImage?
    @State private var isHovered = false

    var body: some View {
        VStack(spacing: 8) {
            ZStack(alignment: .topTrailing) {
                // Thumbnail
                Group {
                    if let img = thumbnail {
                        Image(nsImage: img)
                            .resizable()
                            .aspectRatio(contentMode: .fill)
                    } else {
                        Color.gray.opacity(0.2)
                        ProgressView().tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                    }
                }
                .frame(width: 150, height: 120)
                .clipped()
                .clipShape(RoundedRectangle(cornerRadius: 10))

                // Trash button (visible on hover)
                if isHovered {
                    Button {
                        Task {
                            if let _ = try? await NSWorkspace.shared.recycle([file.url]) {
                                await MainActor.run { file.isTrashed = true }
                            }
                        }
                    } label: {
                        Image(systemName: "trash.circle.fill")
                            .font(.system(size: 26))
                            .symbolRenderingMode(.palette)
                            .foregroundStyle(.white, .red)
                            .shadow(radius: 4)
                    }
                    .buttonStyle(.plain)
                    .padding(6)
                    .transition(.scale.combined(with: .opacity))
                }

                // Duplicate badge
                if file.duplicateGroupUUID != nil {
                    Image(systemName: "doc.on.doc.fill")
                        .font(.system(size: 10))
                        .padding(4)
                        .background(Circle().fill(.orange))
                        .foregroundStyle(.white)
                        .offset(x: -6, y: 6)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                }
            }

            VStack(alignment: .leading, spacing: 3) {
                Text(file.filename)
                    .font(.system(size: 10, weight: .medium))
                    .lineLimit(1)
                    .truncationMode(.middle)

                HStack {
                    Text(String(format: "%.1f MB", file.fileSizeMB))
                        .font(.system(size: 9))
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text(file.creationDate.formatted(date: .abbreviated, time: .omitted))
                        .font(.system(size: 9))
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .padding(8)
        .background(
            RoundedRectangle(cornerRadius: 14)
                .fill(.ultraThinMaterial)
                .shadow(color: .black.opacity(0.15), radius: 6, y: 3)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 14)
                .stroke(isHovered ? Color.red.opacity(0.5) : Color.clear, lineWidth: 1.5)
        )
        .animation(.easeInOut(duration: 0.15), value: isHovered)
        .onHover { isHovered = $0 }
        .task {
            if thumbnail == nil {
                thumbnail = await ThumbnailService.shared.getThumbnail(for: file.url)
            }
        }
        .onTapGesture(count: 2) {
            withAnimation(.spring()) { viewModel.previewFile = file }
        }
    }
}

// MARK: - Undo Toast

struct UndoToast: View {
    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "trash.fill").foregroundStyle(.red)
            Text("Files moved to Trash")
                .font(.system(size: 13, weight: .medium))
            Spacer()
            Button("Undo") {
                NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.trash")!)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(.ultraThickMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .shadow(color: .black.opacity(0.3), radius: 12, y: 4)
        .padding(.horizontal, 40)
    }
}
