import SwiftUI

struct CleanupView: View {
    @ObservedObject var viewModel: AppViewModel
    
    var junkFiles: [AppViewModel.FileStatus] {
        viewModel.activeFiles.filter { $0.isJunk }
    }
    
    var highConfidenceJunk: [AppViewModel.FileStatus] {
        junkFiles.filter { $0.aiTags.contains { tag in
            let lower = tag.lowercased()
            return lower == "receipt" || lower == "invoice" || lower == "tax_document"
        }}
    }
    
    var groupedRecommendedJunk: [(String, [AppViewModel.FileStatus])] {
        let remaining = junkFiles.filter { file in
            !highConfidenceJunk.contains { $0.id == file.id }
        }
        
        let grouped = Dictionary(grouping: remaining) { file -> String in
            return file.aiTags.first(where: { tag in
                let lower = tag.lowercased()
                return lower == "screenshot" || lower == "text"
            }) ?? "Other Junk"
        }
        
        return grouped.map { ($0.key, $0.value) }.sorted { $0.0 < $1.0 }
    }
    
    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack {
                Image(systemName: "trash.slash.fill")
                    .font(.largeTitle)
                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                Text("Cleanup & Junk Analysis")
                    .font(.title.bold())
                Spacer()
                Text("\(junkFiles.count) Junk Files Found")
                    .font(.headline)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal)
            .padding(.top, 20)
            
            if junkFiles.isEmpty {
                VStack {
                    Spacer()
                    Image(systemName: "sparkles")
                        .font(.system(size: 60))
                        .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                    Text("No Junk Files Detected Yet")
                        .font(.headline)
                        .foregroundStyle(.secondary)
                    Text("Files containing faces are permanently safeguarded. Screenshots and receipts will appear here.")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                        .multilineTextAlignment(.center)
                        .padding()
                    Spacer()
                }
                .frame(maxWidth: .infinity)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 24) {
                        if !highConfidenceJunk.isEmpty {
                            Text("High Confidence Junk (Receipts & Invoices)")
                                .font(.headline)
                                .foregroundStyle(.red)
                                .padding(.horizontal)
                            
                            LazyVGrid(columns: [GridItem(.adaptive(minimum: 140), spacing: 16)], spacing: 16) {
                                ForEach(highConfidenceJunk) { file in
                                    if !file.isTrashed { FileCard(file: file, viewModel: viewModel) }
                                }
                            }
                            .padding(.horizontal)
                        }
                        
                        if !groupedRecommendedJunk.isEmpty {
                            ForEach(groupedRecommendedJunk, id: \.0) { group in
                                Text("\(group.0) (Review Recommended)")
                                    .font(.headline)
                                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                                    .padding(.horizontal)
                                    .padding(.top, 16)
                                
                                LazyVGrid(columns: [GridItem(.adaptive(minimum: 140), spacing: 16)], spacing: 16) {
                                    ForEach(group.1) { file in
                                        if !file.isTrashed { FileCard(file: file, viewModel: viewModel) }
                                    }
                                }
                                .padding(.horizontal)
                            }
                        }
                    }
                    .padding(.bottom)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.3))
    }
}
