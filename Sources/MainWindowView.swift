import SwiftUI

struct MainWindowView: View {
    @StateObject private var viewModel = AppViewModel()
    
    var body: some View {
        ZStack {
            NavigationSplitView {
                SidebarView(viewModel: viewModel)
                    .preferredColorScheme(.dark)
                    .navigationSplitViewColumnWidth(min: 300, ideal: 350)
            } detail: {
                MainContent(viewModel: viewModel)
                    .preferredColorScheme(.dark)
            }
            .background(LavaLampBackground())
            .accentColor(Color(red: 1.0, green: 0.8, blue: 0.0)) // High Vis Yellow
            
            // Absolutely top-level overlay so it ignores all inner constraints
            if viewModel.previewFile != nil {
                MediaPreviewOverlay(viewModel: viewModel)
                    .transition(.opacity)
                    .zIndex(1000)
            }
        }
    }
}

struct SidebarView: View {
    @ObservedObject var viewModel: AppViewModel
    
    var body: some View {
        List {
            if viewModel.isProcessing || viewModel.totalCount > 0 {
                Section("Navigation") {
                    Button(action: { viewModel.activeTab = "Library" }) {
                        Label("Media Library", systemImage: "photo.on.rectangle")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(viewModel.activeTab == "Library" ? Color(red: 1.0, green: 0.8, blue: 0.0) : .primary)
                    
                    Button(action: { viewModel.activeTab = "Cleanup" }) {
                        Label("Cleanup & Junk", systemImage: "trash.slash.fill")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(viewModel.activeTab == "Cleanup" ? Color(red: 1.0, green: 0.8, blue: 0.0) : .primary)
                    
                    Button(action: { viewModel.activeTab = "Restructure" }) {
                        Label("Folder Restructuring", systemImage: "folder.badge.gearshape")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(viewModel.activeTab == "Restructure" ? Color(red: 1.0, green: 0.8, blue: 0.0) : .primary)
                    
                    Button(action: { viewModel.activeTab = "People" }) {
                        Label("People", systemImage: "person.2.fill")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(viewModel.activeTab == "People" ? Color(red: 1.0, green: 0.8, blue: 0.0) : .primary)
                }
                
                Section("Progress") {
                    VStack(alignment: .leading, spacing: 8) {
                        ProgressView(value: Double(viewModel.processedCount), total: Double(max(1, viewModel.totalCount)))
                            .progressViewStyle(.linear)
                            .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                        
                        HStack {
                            Text("\(viewModel.processedCount) / \(viewModel.totalCount) Files")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            Spacer()
                            if viewModel.isProcessing {
                                Text(viewModel.timeRemainingString)
                                    .font(.caption2)
                                    .fontWeight(.bold)
                                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                            }
                        }
                    }
                    .padding(.vertical, 4)
                }
                
                if !viewModel.fileTree.isEmpty {
                    Section("File Hierarchy") {
                        OutlineGroup(viewModel.fileTree, children: \.children) { node in
                            HStack {
                                Image(systemName: node.children == nil ? "folder" : "folder.fill")
                                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                                
                                Text(node.name)
                                    .font(.system(size: 13, weight: .medium))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                
                                Spacer()
                                
                                Text("\(node.done)/\(node.total)")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
        }
        .listStyle(.sidebar)
    }
}

struct MainContent: View {
    @ObservedObject var viewModel: AppViewModel
    
    var body: some View {
        ZStack {
            if viewModel.isProcessing || viewModel.totalCount > 0 {
                if viewModel.activeTab == "Library" {
                    ProcessingGridView(viewModel: viewModel)
                } else if viewModel.activeTab == "Cleanup" {
                    CleanupView(viewModel: viewModel)
                } else if viewModel.activeTab == "Restructure" {
                    FolderOrganizationView(viewModel: viewModel)
                } else if viewModel.activeTab == "People" {
                    PeopleView(viewModel: viewModel)
                }
            } else {
                VStack(spacing: 20) {
                    if let imgPath = Bundle.main.path(forResource: "aura_tag_icon", ofType: "png"),
                       let nsImage = NSImage(contentsOfFile: imgPath) {
                        Image(nsImage: nsImage)
                            .resizable()
                            .scaledToFit()
                            .frame(height: 180)
                            .shadow(color: .black.opacity(0.8), radius: 30, y: 15)
                    } else {
                        Image(systemName: "cpu")
                            .font(.system(size: 60))
                            .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                    }
                    
                    Text("FileID Professional")
                        .font(.system(size: 34, weight: .black, design: .rounded))
                        .foregroundStyle(.white)
                    
                    Button {
                        viewModel.selectFolder()
                    } label: {
                        Text("Connect Root Storage")
                            .font(.system(size: 16, weight: .bold))
                            .padding(.horizontal, 32)
                            .padding(.vertical, 14)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                    .foregroundStyle(.black)
                    .controlSize(.large)
                    
                    Button {
                        viewModel.runQALoadTest()
                    } label: {
                        Text("Developer: Run 10,000 QA Clones")
                            .font(.system(size: 14))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(.gray)
                    .padding(.top, 10)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct ProcessingGridView: View {
    @ObservedObject var viewModel: AppViewModel
    @State private var selectedTab: String = "Media"
    
    var filteredFiles: [AppViewModel.FileStatus] {
        let matching = viewModel.activeFiles.filter { file in
            let matchesTab = selectedTab == "Media" ? 
                ["jpg", "jpeg", "png", "heic", "mp4", "mov"].contains(file.url.pathExtension.lowercased()) :
                file.url.pathExtension.lowercased() == "pdf"
                
            if !matchesTab { return false }
            
            if viewModel.searchText.isEmpty { return true }
            let query = viewModel.searchText.lowercased()
            return file.filename.lowercased().contains(query) ||
                   file.aiTags.contains(where: { $0.lowercased().contains(query) }) ||
                   (file.cameraModel?.lowercased().contains(query) ?? false) ||
                   (file.locationString?.lowercased().contains(query) ?? false)
        }
        // UI VIRTUALIZATION: Removed 500 limit. LazyVGrid handles infinite scrolling safely when backed by LRU Thumbnail Cache.
        return matching.reversed()
    }
    
    var body: some View {
        VStack {
            HStack {
                Picker("", selection: $selectedTab) {
                    Text("Photos & Videos").tag("Media")
                    Text("Documents & PDFs").tag("Documents")
                }
                .pickerStyle(.segmented)
                
                Spacer()
                
                HStack {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                    TextField("Search tags, locations, filenames...", text: $viewModel.searchText)
                        .textFieldStyle(.plain)
                    if !viewModel.searchText.isEmpty {
                        Button { viewModel.searchText = "" } label: {
                            Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(6)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(0.1)))
                .frame(width: 300)
            }
            .padding(.horizontal)
            .padding(.top, 8)
            
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 140), spacing: 16)], spacing: 16) {
                    ForEach(filteredFiles) { file in
                        if !file.isTrashed {
                            FileCard(file: file, viewModel: viewModel)
                        }
                    }
                }
                .padding()
            }
        }
    }
}

struct FileCard: View {
    var file: AppViewModel.FileStatus
    @ObservedObject var viewModel: AppViewModel
    @State private var thumbnail: NSImage?
    
    var body: some View {
        VStack(spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 12)
                    .fill(.regularMaterial)
                    .aspectRatio(1, contentMode: .fill)
                
                if file.status == .processing {
                    ProgressView()
                        .controlSize(.large)
                } else if file.status == .completed {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.largeTitle)
                        .foregroundStyle(.green)
                } else if file.status == .failed {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.largeTitle)
                        .foregroundStyle(.red)
                } else if let img = thumbnail {
                    Image(nsImage: img)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                        .frame(width: 140, height: 140)
                        .clipped()
                } else {
                    ProgressView().tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                }
            }
            .frame(width: 140, height: 140)
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .shadow(color: .black.opacity(0.3), radius: 8, x: 0, y: 4)
            .task {
                if thumbnail == nil {
                    thumbnail = await ThumbnailService.shared.getThumbnail(for: file.url)
                }
            }
            .overlay(alignment: .topTrailing) {
                if file.status != .processing && file.status != .pending { // Ensure we don't crash while reading!
                    Button(action: {
                        Task {
                            if let _ = try? await NSWorkspace.shared.recycle([file.url]) {
                                await MainActor.run { file.isTrashed = true }
                            }
                        }
                    }) {
                        Image(systemName: "trash.circle.fill")
                            .font(.system(size: 24))
                            .symbolRenderingMode(.palette)
                            .foregroundStyle(.white, .red)
                            .shadow(radius: 4)
                    }
                    .buttonStyle(.plain)
                    .padding(8)
                }
            }
            .frame(height: 120)
            
            Text(file.filename)
                .font(.caption)
                .lineLimit(1)
                .truncationMode(.middle)
            
            if !file.aiTags.isEmpty {
                // Remove horizontal scroll and limit to top 3 to keep grid rigid
                HStack {
                    ForEach(file.aiTags.prefix(3), id: \.self) { tag in
                        Text(tag)
                            .font(.system(size: 10, weight: .bold))
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(Capsule().fill(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.15)))
                            .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                            .overlay(Capsule().stroke(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.3), lineWidth: 1))
                    }
                    if file.aiTags.count > 3 {
                        Text("+\(file.aiTags.count - 3)")
                            .font(.system(size: 10, weight: .bold))
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                }
            }
            
            // EXIF Metadata Pills
            if file.cameraModel != nil || file.locationString != nil {
                HStack(spacing: 4) {
                    if let cam = file.cameraModel {
                        Label(cam, systemImage: "camera.fill")
                            .font(.system(size: 8))
                            .padding(.horizontal, 4)
                            .padding(.vertical, 2)
                            .background(RoundedRectangle(cornerRadius: 4).fill(Color.white.opacity(0.1)))
                            .foregroundStyle(.white)
                    }
                    if let loc = file.locationString {
                        Label(loc, systemImage: "location.fill")
                            .font(.system(size: 8))
                            .padding(.horizontal, 4)
                            .padding(.vertical, 2)
                            .background(RoundedRectangle(cornerRadius: 4).fill(Color.white.opacity(0.1)))
                            .foregroundStyle(.white)
                    }
                    Spacer()
                }
            }
            
            HStack {
                Text(file.creationDateStr)
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
                Spacer()
                Text(String(format: "%.1f MB", file.fileSizeMB))
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 4)
        }
        .padding(8)
        .background(
            RoundedRectangle(cornerRadius: 16)
                .fill(.ultraThinMaterial)
                .shadow(color: .black.opacity(0.1), radius: 5, x: 0, y: 2)
        )
        // Add a completely transparent button covering the entire card to guarantee clicks register instantly
        .overlay(
            Color.white.opacity(0.001)
                .onTapGesture(count: 2) {
                    withAnimation(.spring()) { viewModel.previewFile = file }
                }
        )
    }
}
