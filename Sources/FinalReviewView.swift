import SwiftUI
import AppKit

struct FinalReviewView: View {
    @ObservedObject var viewModel: AppViewModel
    
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
                ForEach(viewModel.activeFiles) { file in
                    if file.status == .reviewRequired {
                        ReviewRow(file: file)
                    }
                }
            }
            .listStyle(.inset)
            .padding()
            
            HStack(spacing: 20) {
                Button("Deselect All") {
                    for i in 0..<viewModel.activeFiles.count {
                        if viewModel.activeFiles[i].status == .reviewRequired {
                            viewModel.activeFiles[i].isSelectedForRename = false
                        }
                    }
                }
                .buttonStyle(.bordered)
                
                Button("Select All") {
                    for i in 0..<viewModel.activeFiles.count {
                        if viewModel.activeFiles[i].status == .reviewRequired {
                            viewModel.activeFiles[i].isSelectedForRename = true
                        }
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
    @Bindable var file: AppViewModel.FileStatus
    
    var body: some View {
        HStack(spacing: 16) {
            Toggle("", isOn: $file.isSelectedForRename)
                .toggleStyle(.checkbox)
                .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
            
            // Image Preview
            if let thumbURL = file.thumbnailURL {
                AsyncImage(url: thumbURL) { phase in
                    if let image = phase.image {
                        image.resizable().scaledToFill()
                    } else {
                        Rectangle().fill(Color(white: 0.15)).overlay { ProgressView() }
                    }
                }
                .frame(width: 80, height: 80)
                .clipShape(RoundedRectangle(cornerRadius: 8))
            } else {
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color(white: 0.15))
                    .frame(width: 80, height: 80)
                    .overlay(Image(systemName: "photo").foregroundColor(.gray))
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
    
    // Hash the UUID into a consistent bright color so duplicates share a tint!
    private func colorForDuplicateGroup(_ uuid: UUID?) -> Color {
        guard let uuid = uuid else { return .clear }
        let colors: [Color] = [.red, .orange, .green, .blue, .purple, .pink, .yellow, .mint, .cyan]
        let hash = abs(uuid.hashValue) % colors.count
        return colors[hash]
    }
}
