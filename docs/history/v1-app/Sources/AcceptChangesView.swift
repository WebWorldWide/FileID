import SwiftUI
import SwiftData

struct AcceptChangesView: View {
    @ObservedObject var viewModel: AppViewModel
    // fetchLimit capped per machine class (Hardware.gridFetchLimit) so the
    // @Query notification cost stays predictable on 100K-file libraries.
    @Query(AcceptChangesView.pendingDescriptor) private var pendingFiles: [FileRecord]
    @State private var showOnlySelected = false

    private static let pendingDescriptor: FetchDescriptor<FileRecord> = {
        var d = FetchDescriptor<FileRecord>(
            predicate: #Predicate { $0.statusValue == "reviewRequired" },
            sortBy: [SortDescriptor(\.creationDate, order: .reverse)]
        )
        d.fetchLimit = Hardware.gridFetchLimit
        return d
    }()

    // Cache invalidated by the .onChange hooks at the bottom of body —
    // recomputing on every body eval at 500 rows added perceptible lag.
    @State private var displayedFiles: [FileRecord] = []
    @State private var selectedCount: Int = 0
    @State private var renameCount:   Int = 0

    private func recomputeCaches() {
        let selected = pendingFiles.filter { $0.isSelectedForRename }
        displayedFiles = showOnlySelected ? selected : pendingFiles
        selectedCount  = selected.count
        renameCount    = selected.filter {
            $0.proposedFilename != nil && $0.proposedFilename != $0.filename
        }.count
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            VStack(spacing: 8) {
                HStack {
                    Image(systemName: "checkmark.shield.fill")
                        .font(.title)
                        .foregroundStyle(Theme.gold)
                    
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
                        .tint(Theme.gold)
                        .help("Rename files on disk when accepting changes")
                    Toggle("Write EXIF", isOn: $viewModel.applyEXIFWrite)
                        .toggleStyle(.checkbox)
                        .tint(Theme.gold)
                        .help("Write tags to EXIF metadata when accepting changes")
                }
            }
            .padding(20)
            .background(.ultraThinMaterial)
            
            // Filter bar
            HStack {
                Toggle("Show only selected", isOn: $showOnlySelected)
                    .toggleStyle(.checkbox)
                    .font(.caption)
                    .help("Hide unselected rows")
                Spacer()
                
                Button("Deselect All") {
                    for file in pendingFiles { file.isSelectedForRename = false }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("Clear selection for all pending changes")
                
                Button("Select All") {
                    for file in pendingFiles { file.isSelectedForRename = true }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("Select every pending change")
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 8)
            .background(Color.black.opacity(0.2))
            
            // File list — or empty state when nothing is pending
            if pendingFiles.isEmpty {
                VStack(spacing: 14) {
                    Spacer()
                    Image(systemName: "checkmark.seal.fill")
                        .font(.system(size: 56))
                        .foregroundStyle(Theme.gold.opacity(0.6))
                    Text("All changes applied")
                        .font(.title3.bold())
                    Text("Nothing pending review. Scan a new folder or wait for the current scan to produce proposals.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 360)
                    Spacer()
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List {
                    ForEach(displayedFiles) { file in
                        ChangeRow(file: file, viewModel: viewModel)
                    }
                }
                .listStyle(.inset)
            }

            // Action bar
            HStack(spacing: 16) {
                Button("Skip Changes") {
                    // Clear selection without calling approveChanges — that method
                    // disconnects folder access and stops the watcher, which is the
                    // wrong intent here. "Skip" should just leave proposals alone.
                    for file in pendingFiles { file.isSelectedForRename = false }
                }
                .buttonStyle(.bordered)
                .tint(.secondary)
                .disabled(pendingFiles.isEmpty)
                .help("Deselect all pending changes without applying")
                
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
                .tint(Theme.gold)
                .foregroundStyle(.black)
                .controlSize(.large)
                .disabled(selectedCount == 0)
                .help("Apply selected renames and EXIF writes to disk")
            }
            .padding(16)
            .background(.ultraThinMaterial)
        }
        .onAppear { recomputeCaches() }
        .onChange(of: pendingFiles.count)    { _, _ in recomputeCaches() }
        .onChange(of: showOnlySelected)      { _, _ in recomputeCaches() }
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
                .tint(Theme.gold)
                .help("Include this file in the accept batch")
            
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
                    HStack(spacing: 6) {
                        ForEach(file.aiTags, id: \.self) { tag in
                            Text(tag)
                                .font(.system(size: 8, weight: .medium))
                                .lineLimit(1)
                                .fixedSize()
                                .padding(.horizontal, 4)
                                .padding(.vertical, 1)
                                .background(Capsule().fill(Theme.gold.opacity(0.12)))
                                .foregroundStyle(Theme.gold)
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
