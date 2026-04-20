import SwiftUI
import SwiftData

struct AcceptChangesView: View {
    @ObservedObject var viewModel: AppViewModel
    @Query(filter: #Predicate<FileRecord> { $0.statusValue == "reviewRequired" }) private var pendingFiles: [FileRecord]
    @State private var showOnlySelected = false
    
    var displayedFiles: [FileRecord] {
        showOnlySelected ? pendingFiles.filter { $0.isSelectedForRename } : pendingFiles
    }
    
    var selectedCount: Int { pendingFiles.filter { $0.isSelectedForRename }.count }
    var renameCount: Int { pendingFiles.filter { $0.isSelectedForRename && $0.proposedFilename != nil && $0.proposedFilename != $0.filename }.count }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            VStack(spacing: 8) {
                HStack {
                    Image(systemName: "checkmark.shield.fill")
                        .font(.title)
                        .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                    
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Accept Changes")
                            .font(.title2.bold())
                        Text("Review all proposed modifications before they're applied to your files.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                }
                
                // Summary stats
                HStack(spacing: 20) {
                    StatBadge(icon: "doc.badge.arrow.up.fill", label: "Renames", count: renameCount, color: .orange)
                    StatBadge(icon: "tag.fill", label: "EXIF Writes", count: viewModel.applyEXIFWrite ? selectedCount : 0, color: .cyan)
                    StatBadge(icon: "checkmark.circle.fill", label: "Selected", count: selectedCount, color: .green)
                    StatBadge(icon: "doc.fill", label: "Total", count: pendingFiles.count, color: .gray)
                    
                    Spacer()
                    
                    // Global toggles
                    Toggle("Rename Files", isOn: $viewModel.applyFilenameRename)
                        .toggleStyle(.checkbox)
                    Toggle("Write EXIF", isOn: $viewModel.applyEXIFWrite)
                        .toggleStyle(.checkbox)
                }
            }
            .padding(20)
            .background(.ultraThinMaterial)
            
            // Filter bar
            HStack {
                Toggle("Show only selected", isOn: $showOnlySelected)
                    .toggleStyle(.checkbox)
                    .font(.caption)
                Spacer()
                
                Button("Deselect All") {
                    for file in pendingFiles { file.isSelectedForRename = false }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                
                Button("Select All") {
                    for file in pendingFiles { file.isSelectedForRename = true }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 8)
            .background(Color.black.opacity(0.2))
            
            // File list
            List {
                ForEach(displayedFiles) { file in
                    ChangeRow(file: file, viewModel: viewModel)
                }
            }
            .listStyle(.inset)
            
            // Action bar
            HStack(spacing: 16) {
                Button("Reject All") {
                    for file in pendingFiles { file.isSelectedForRename = false }
                    Task { await viewModel.approveChanges() }
                }
                .buttonStyle(.bordered)
                .tint(.red)
                
                Spacer()
                
                Text("\(selectedCount) of \(pendingFiles.count) changes will be applied")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                
                Spacer()
                
                Button {
                    Task { await viewModel.executeRenaming() }
                } label: {
                    Label("Accept \(selectedCount) Changes", systemImage: "checkmark.circle.fill")
                        .fontWeight(.bold)
                }
                .buttonStyle(.borderedProminent)
                .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                .foregroundStyle(.black)
                .controlSize(.large)
                .disabled(selectedCount == 0)
            }
            .padding(16)
            .background(.ultraThinMaterial)
        }
    }
}

// MARK: - Change Row

struct ChangeRow: View {
    @Bindable var file: FileRecord
    @ObservedObject var viewModel: AppViewModel
    
    var hasRename: Bool {
        file.proposedFilename != nil && file.proposedFilename != file.filename
    }
    
    var body: some View {
        HStack(spacing: 14) {
            // Toggle
            Toggle("", isOn: $file.isSelectedForRename)
                .toggleStyle(.checkbox)
                .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
            
            // Thumbnail
            ThumbnailView(url: file.url)
                .frame(width: 56, height: 56)
                .clipShape(RoundedRectangle(cornerRadius: 8))
            
            // Before → After
            VStack(alignment: .leading, spacing: 6) {
                // Original name
                HStack(spacing: 6) {
                    Text("BEFORE")
                        .font(.system(size: 8, weight: .heavy))
                        .foregroundStyle(.red.opacity(0.7))
                    Text(file.filename)
                        .font(.system(size: 12))
                        .strikethrough(file.isSelectedForRename && hasRename, color: .red)
                        .foregroundStyle(file.isSelectedForRename ? .secondary : .primary)
                        .lineLimit(1)
                }
                
                if hasRename {
                    HStack(spacing: 6) {
                        Text("AFTER")
                            .font(.system(size: 8, weight: .heavy))
                            .foregroundStyle(.green.opacity(0.7))
                        Text(file.proposedFilename ?? "")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(file.isSelectedForRename ? .primary : .secondary)
                            .lineLimit(1)
                    }
                }
                
                // Tags
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 3) {
                        ForEach(file.aiTags, id: \.self) { tag in
                            Text(tag)
                                .font(.system(size: 8, weight: .medium))
                                .lineLimit(1)
                                .fixedSize()
                                .padding(.horizontal, 4)
                                .padding(.vertical, 1)
                                .background(Capsule().fill(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.12)))
                                .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                        }
                    }
                }
                .frame(height: 16)
            }
            
            Spacer()
            
            // Change type indicators
            VStack(alignment: .trailing, spacing: 4) {
                if hasRename {
                    Label("Rename", systemImage: "pencil")
                        .font(.system(size: 9, weight: .medium))
                        .foregroundStyle(.orange)
                }
                if viewModel.applyEXIFWrite {
                    Label("EXIF", systemImage: "tag.fill")
                        .font(.system(size: 9, weight: .medium))
                        .foregroundStyle(.cyan)
                }
            }
            
            // Duplicate group indicator
            if file.duplicateGroupUUID != nil {
                Circle()
                    .fill(colorForGroup(file.duplicateGroupUUID))
                    .frame(width: 12, height: 12)
                    .overlay(
                        Image(systemName: "doc.on.doc.fill")
                            .font(.system(size: 6))
                            .foregroundStyle(.white)
                    )
                    .help("Part of a duplicate group")
            }
        }
        .padding(.vertical, 6)
        .opacity(file.isSelectedForRename ? 1.0 : 0.5)
    }
    
    private func colorForGroup(_ uuid: UUID?) -> Color {
        guard let uuid = uuid else { return .clear }
        let colors: [Color] = [.red, .orange, .green, .blue, .purple, .pink, .yellow, .mint, .cyan]
        let hash = abs(uuid.hashValue) % colors.count
        return colors[hash]
    }
}

// MARK: - Stat Badge

struct StatBadge: View {
    let icon: String
    let label: String
    let count: Int
    let color: Color
    
    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .font(.system(size: 12))
                .foregroundStyle(color)
            VStack(alignment: .leading, spacing: 0) {
                Text("\(count)")
                    .font(.system(size: 16, weight: .bold, design: .rounded))
                Text(label)
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 8).fill(color.opacity(0.1)))
    }
}
