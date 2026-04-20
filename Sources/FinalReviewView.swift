import SwiftUI
import SwiftData

struct FinalReviewView: View {
    @ObservedObject var viewModel: AppViewModel
    @Query(filter: #Predicate<FileRecord> { $0.statusValue == "reviewRequired" }) private var reviewFiles: [FileRecord]
    
    var body: some View {
        VStack(spacing: 0) {
            
            Text("Review Pending Changes")
                .font(.largeTitle)
                .bold()
                .padding()
            
            Text("Select the files you want to modify. Semantically identical duplicates are outlined in the same color!")
                .foregroundColor(.secondary)
                .padding(.horizontal)
            
            HStack(spacing: 40) {
                Toggle("Rename Files", isOn: $viewModel.applyFilenameRename)
                    .toggleStyle(.checkbox)
                
                Toggle("Inject EXIF Metadata (Spotlight Searchable)", isOn: $viewModel.applyEXIFWrite)
                    .toggleStyle(.checkbox)
            }
            .padding()
            
            List {
                ForEach(reviewFiles) { file in
                    ReviewRow(file: file)
                }
            }
            .listStyle(.inset)
            .padding()
            
            HStack(spacing: 20) {
                Button("Deselect All") {
                    for file in reviewFiles {
                        file.isSelectedForRename = false
                    }
                }
                .buttonStyle(.bordered)
                
                Button("Select All") {
                    for file in reviewFiles {
                        file.isSelectedForRename = true
                    }
                }
                .buttonStyle(.bordered)
                
                Button("Apply Approved") {
                    Task {
                        await viewModel.approveChanges()
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                .foregroundStyle(.black)
            }
            .padding()
            .background(Color(white: 0.1))
        }
    }
}

struct ReviewRow: View {
    @Bindable var file: FileRecord
    
    var body: some View {
        HStack(spacing: 16) {
            Toggle("", isOn: $file.isSelectedForRename)
                .toggleStyle(.checkbox)
                .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
            
            // Image Preview (Using ThumbnailService as before)
            ZStack {
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color(white: 0.15))
                    .frame(width: 80, height: 80)
                
                ThumbnailView(url: file.url)
                    .frame(width: 80, height: 80)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            }
            
            VStack(alignment: .leading, spacing: 4) {
                Text("\(file.filename)")
                    .strikethrough(file.isSelectedForRename, color: .red)
                    .foregroundColor(.secondary)
                    .font(.caption)
                    .lineLimit(1)
                
                Image(systemName: "arrow.down")
                    .foregroundColor(Color(red: 1.0, green: 0.8, blue: 0.0))
                
                Text("\(file.proposedFilename ?? "")")
                    .foregroundColor(file.isSelectedForRename ? .primary : .secondary)
                    .font(.headline)
                    .bold()
                    .lineLimit(1)
            }
            
            Spacer()
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 4)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(colorForDuplicateGroup(file.duplicateGroupUUID).opacity(0.1))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(colorForDuplicateGroup(file.duplicateGroupUUID), lineWidth: file.duplicateGroupUUID != nil ? 2 : 0)
        )
    }
    
    private func colorForDuplicateGroup(_ uuid: UUID?) -> Color {
        guard let uuid = uuid else { return .clear }
        let colors: [Color] = [.red, .orange, .green, .blue, .purple, .pink, .yellow, .mint, .cyan]
        let hash = abs(uuid.hashValue) % colors.count
        return colors[hash]
    }
}

// Helper Thumbnail View
struct ThumbnailView: View {
    let url: URL
    @State private var image: NSImage?
    
    var body: some View {
        Group {
            if let image = image {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFill()
            } else {
                ProgressView()
            }
        }
        .task {
            image = await ThumbnailService.shared.getThumbnail(for: url)
        }
    }
}
