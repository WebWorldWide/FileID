import SwiftUI
import SwiftData

struct MainWindowView: View {
    @StateObject private var viewModel = AppViewModel()
    @Environment(\.modelContext) private var modelContext
    @State private var isDragHovering = false
    
    var body: some View {
        ZStack {
            VStack(spacing: 0) {
                NavigationSplitView {
                    SidebarView(viewModel: viewModel)
                        .preferredColorScheme(.dark)
                        .navigationSplitViewColumnWidth(min: 300, ideal: 350)
                } detail: {
                    MainContent(viewModel: viewModel)
                        .preferredColorScheme(.dark)
                }
                .background(LavaLampBackground())
                .accentColor(Color(red: 1.0, green: 0.8, blue: 0.0))
                .onAppear {
                    viewModel.modelContainer = modelContext.container
                }
                
                // Bottom status bar
                if viewModel.isProcessing || viewModel.totalCount > 0 {
                    BottomStatusBar(viewModel: viewModel)
                }
            }
            
            // Drag-and-drop overlay
            if isDragHovering {
                RoundedRectangle(cornerRadius: 20)
                    .stroke(Color(red: 1.0, green: 0.8, blue: 0.0), style: StrokeStyle(lineWidth: 3, dash: [12, 6]))
                    .background(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.08))
                    .overlay(
                        VStack(spacing: 12) {
                            Image(systemName: "arrow.down.doc.fill")
                                .font(.system(size: 48))
                                .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                            Text("Drop Folder to Scan")
                                .font(.title3.bold())
                                .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                        }
                    )
                    .padding(20)
                    .allowsHitTesting(false)
            }
            
            // Preview overlay
            if viewModel.previewFile != nil {
                MediaPreviewOverlay(viewModel: viewModel)
                    .transition(.opacity)
                    .zIndex(1000)
            }
        }
        .onDrop(of: [.fileURL], isTargeted: $isDragHovering) { providers in
            guard let provider = providers.first else { return false }
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                guard let url = url else { return }
                var isDir: ObjCBool = false
                if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue {
                    DispatchQueue.main.async { viewModel.startProcessing(folderURL: url) }
                }
            }
            return true
        }
        .onReceive(NotificationCenter.default.publisher(for: .fileIDOpenFolder)) { _ in
            viewModel.selectFolder()
        }
        .onReceive(NotificationCenter.default.publisher(for: .fileIDRescan)) { _ in
            if let url = viewModel.currentFolderURL { viewModel.startProcessing(folderURL: url) }
        }
        .onKeyPress(.escape) {
            if viewModel.previewFile != nil {
                withAnimation { viewModel.previewFile = nil }
                return .handled
            }
            return .ignored
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
                    
                    Button(action: { viewModel.activeTab = "Review" }) {
                        Label("Review & Accept", systemImage: "checkmark.shield.fill")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(viewModel.activeTab == "Review" ? Color(red: 1.0, green: 0.8, blue: 0.0) : .primary)
                    
                    Button(action: { viewModel.activeTab = "Settings" }) {
                        Label("Settings", systemImage: "gearshape.fill")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(viewModel.activeTab == "Settings" ? Color(red: 1.0, green: 0.8, blue: 0.0) : .primary)
                }
                
                Section("Processing Control") {
                    VStack(alignment: .leading, spacing: 12) {
                        ProgressView(value: Double(viewModel.processedCount), total: Double(max(1, viewModel.totalCount)))
                            .progressViewStyle(.linear)
                            .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                        
                        HStack {
                            Text("\(viewModel.processedCount) / \(viewModel.totalCount) Files")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            Spacer()
                            if viewModel.isProcessing && !viewModel.isPaused {
                                Text(viewModel.timeRemainingString)
                                    .font(.caption2)
                                    .fontWeight(.bold)
                                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                            }
                        }

                        HStack(spacing: 8) {
                            if viewModel.isProcessing {
                                Button(action: {
                                    if viewModel.isPaused {
                                        viewModel.resume()
                                    } else {
                                        viewModel.pause()
                                    }
                                }) {
                                    Label(viewModel.isPaused ? "Resume" : "Pause", 
                                          systemImage: viewModel.isPaused ? "play.fill" : "pause.fill")
                                        .font(.caption.bold())
                                        .frame(maxWidth: .infinity)
                                        .padding(.vertical, 6)
                                        .background(Color.white.opacity(0.1))
                                        .clipShape(RoundedRectangle(cornerRadius: 6))
                                }
                                .buttonStyle(.plain)
                            }

                            Button(action: { viewModel.exportReport() }) {
                                Label("Export", systemImage: "square.and.arrow.up")
                                    .font(.caption.bold())
                                    .frame(maxWidth: .infinity)
                                    .padding(.vertical, 6)
                                    .background(Color.white.opacity(0.1))
                                    .clipShape(RoundedRectangle(cornerRadius: 6))
                            }
                            .buttonStyle(.plain)
                            .disabled(viewModel.totalCount == 0)

                            Button(action: { viewModel.selectFolder() }) {
                                Image(systemName: "arrow.counterclockwise")
                                    .font(.caption.bold())
                                    .padding(6)
                                    .background(Color.white.opacity(0.1))
                                    .clipShape(RoundedRectangle(cornerRadius: 6))
                            }
                            .buttonStyle(.plain)
                            .help("Start New Scan")
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

// MARK: - Bottom Status Bar

struct BottomStatusBar: View {
    @ObservedObject var viewModel: AppViewModel
    @State private var memoryMB: Int = 0
    @State private var timer = Timer.publish(every: 2, on: .main, in: .common).autoconnect()

    var filesPerSec: Double {
        guard let start = viewModel.processingStartTime,
              viewModel.processedCount > 0 else { return 0 }
        let elapsed = Date().timeIntervalSince(start)
        return elapsed > 0 ? Double(viewModel.processedCount) / elapsed : 0
    }

    var body: some View {
        HStack(spacing: 16) {
            // Status dot
            Circle()
                .fill(viewModel.isProcessing ? .green : .secondary)
                .frame(width: 7, height: 7)
                .overlay {
                    if viewModel.isProcessing {
                        Circle()
                            .fill(.green.opacity(0.4))
                            .frame(width: 14, height: 14)
                    }
                }

            Text(viewModel.currentStatus)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(.secondary)
                .lineLimit(1)

            if viewModel.isProcessing {
                Divider().frame(height: 12)

                Label(String(format: "%.1f files/sec", filesPerSec), systemImage: "speedometer")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))

                Divider().frame(height: 12)

                Text(viewModel.timeRemainingString)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Spacer()

            // Memory
            Label("\(memoryMB) MB", systemImage: "memorychip")
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(memoryMB > 800 ? .orange : .secondary)

            Divider().frame(height: 12)

            // File count
            Text("\(viewModel.processedCount) / \(viewModel.totalCount)")
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
        .background(.ultraThinMaterial)
        .overlay(Divider(), alignment: .top)
        .onReceive(timer) { _ in
            let info = mach_task_basic_info()
            var count = mach_msg_type_number_t(MemoryLayout<mach_task_basic_info>.size) / 4
            var infoRef = info
            _ = withUnsafeMutablePointer(to: &infoRef) {
                $0.withMemoryRebound(to: integer_t.self, capacity: 1) {
                    task_info(mach_task_self_, task_flavor_t(MACH_TASK_BASIC_INFO), $0, &count)
                }
            }
            memoryMB = Int(infoRef.resident_size / 1_048_576)
        }
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
                } else if viewModel.activeTab == "Review" {
                    AcceptChangesView(viewModel: viewModel)
                } else if viewModel.activeTab == "Settings" {
                    SettingsView(viewModel: viewModel)
                }
            } else {
                VStack(spacing: 20) {
                    // FileID branded icon — no external asset dependency
                    ZStack {
                        Circle()
                            .fill(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.12))
                            .frame(width: 130, height: 130)
                        Image(systemName: "tag.circle.fill")
                            .font(.system(size: 80))
                            .symbolRenderingMode(.palette)
                            .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0), Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.2))
                            .shadow(color: Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.4), radius: 20, y: 8)
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
    
    var body: some View {
        VStack {
            HStack {
                Picker("", selection: $selectedTab) {
                    Text("Photos & Videos").tag("Media")
                    Text("Documents & PDFs").tag("Documents")
                }
                .pickerStyle(.segmented)
                .onChange(of: selectedTab) {
                    viewModel.resetPagination(tab: selectedTab)
                }
                
                Spacer()
                
                HStack {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                    TextField("Search tags, locations, filenames...", text: $viewModel.searchText)
                        .textFieldStyle(.plain)
                        .onChange(of: viewModel.searchText) {
                            viewModel.resetPagination(tab: selectedTab)
                        }
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
                    ForEach(viewModel.visibleFiles) { file in
                        if !file.isTrashed {
                            FileCard(file: file, viewModel: viewModel)
                                .onAppear {
                                    if file == viewModel.visibleFiles.last {
                                        viewModel.loadNextPage(tab: selectedTab)
                                    }
                                }
                        }
                    }
                }
                .padding()
                
                if viewModel.isLoadingPage {
                    ProgressView()
                        .padding()
                }
            }
        }
        .onAppear {
            viewModel.resetPagination(tab: selectedTab)
        }
    }
}

struct FileCard: View {
    var file: FileRecord
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
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .stroke(file.aestheticScore > 0.85 ? Color(red: 1.0, green: 0.8, blue: 0.0) : .clear, lineWidth: 2)
                    .blur(radius: 1)
            )
            .task {
                if thumbnail == nil {
                    thumbnail = await ThumbnailService.shared.getThumbnail(for: file.url)
                }
            }
            .overlay(alignment: .topTrailing) {
                HStack(spacing: 4) {
                    if file.aestheticScore > 0.85 {
                        Label("AI Choice", systemImage: "sparkles")
                            .font(.system(size: 8, weight: .black))
                            .padding(.horizontal, 6)
                            .padding(.vertical, 3)
                            .background(Capsule().fill(Color(red: 1.0, green: 0.8, blue: 0.0)))
                            .foregroundStyle(.black)
                            .shadow(color: .yellow.opacity(0.5), radius: 4)
                    }
                    
                    if file.duplicateGroupUUID != nil {
                        Image(systemName: "rectangle.on.rectangle.angled.fill")
                            .font(.system(size: 10))
                            .padding(4)
                            .background(Circle().fill(.blue))
                            .foregroundStyle(.white)
                            .shadow(radius: 2)
                    }

                    if file.status != .processing && file.status != .pending { 
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
                    }
                }
                .padding(6)
            }
            .frame(height: 120)
            
            Text(file.filename)
                .font(.caption)
                .lineLimit(1)
                .truncationMode(.middle)
            
            if !file.aiTags.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 4) {
                        ForEach(file.aiTags, id: \.self) { tag in
                            Text(tag)
                                .font(.system(size: 9, weight: .semibold))
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 3)
                                .background(Capsule().fill(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.15)))
                                .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                                .overlay(Capsule().stroke(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.3), lineWidth: 0.5))
                        }
                    }
                }
                .frame(height: 22)
            }
            
            HStack(spacing: 4) {
                if let cam = file.cameraModel {
                    Label(cam, systemImage: "camera.fill")
                        .font(.system(size: 8))
                        .lineLimit(1)
                        .padding(.horizontal, 4)
                        .padding(.vertical, 2)
                        .background(RoundedRectangle(cornerRadius: 4).fill(Color.white.opacity(0.1)))
                        .foregroundStyle(.white.opacity(0.7))
                }
                if let loc = file.locationString {
                    Label(loc, systemImage: "location.fill")
                        .font(.system(size: 8))
                        .lineLimit(1)
                        .padding(.horizontal, 4)
                        .padding(.vertical, 2)
                        .background(RoundedRectangle(cornerRadius: 4).fill(Color.white.opacity(0.1)))
                        .foregroundStyle(.white.opacity(0.7))
                }
                Spacer()
                Text(file.creationDate.formatted(date: .numeric, time: .omitted))
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
                Text("·")
                    .foregroundStyle(.tertiary)
                Text(String(format: "%.1f MB", file.fileSizeMB))
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(8)
        .background(
            RoundedRectangle(cornerRadius: 16)
                .fill(.ultraThinMaterial)
                .shadow(color: .black.opacity(0.1), radius: 5, x: 0, y: 2)
        )
        .overlay(
            Color.white.opacity(0.001)
                .onTapGesture(count: 2) {
                    withAnimation(.spring()) { viewModel.previewFile = file }
                }
        )
    }
}
