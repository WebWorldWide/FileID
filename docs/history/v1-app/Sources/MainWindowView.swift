import SwiftUI
import SwiftData

struct MainWindowView: View {
    @StateObject private var viewModel = AppViewModel()
    @Environment(\.modelContext) private var modelContext
    @State private var isDragHovering = false
    @AppStorage("hasOnboarded") private var hasOnboarded = false

    var body: some View {
        ZStack {
            if !hasOnboarded {
                OnboardingView()
                    .transition(.opacity)
                    .zIndex(2000)
            }

            NavigationSplitView {
                SidebarView(viewModel: viewModel)
                    .preferredColorScheme(.dark)
                    .navigationSplitViewColumnWidth(min: 200, ideal: 260)
            } detail: {
                MainContent(viewModel: viewModel)
                    .preferredColorScheme(.dark)
            }
            .background(LavaLampBackground())
            // Window styling lives in AppDelegate.configureMainWindow.
            // Don't add `.toolbar(.hidden, for: .windowToolbar)` here — it
            // hides the standard close/minimize/zoom buttons on macOS 26.
            .accentColor(Theme.gold)
            .onAppear {
                viewModel.configureStores(container: modelContext.container)
            }

            if isDragHovering {
                RoundedRectangle(cornerRadius: 20)
                    .stroke(Theme.gold, style: StrokeStyle(lineWidth: 3, dash: [12, 6]))
                    .background(Theme.gold.opacity(0.08))
                    .overlay(
                        VStack(spacing: 12) {
                            Image(systemName: "arrow.down.doc.fill")
                                .font(.system(size: 48))
                                .foregroundStyle(Theme.gold)
                            Text("Drop Folder to Scan")
                                .font(.title3.bold())
                                .foregroundStyle(Theme.gold)
                        }
                    )
                    .padding(20)
                    .allowsHitTesting(false)
            }
            
            if let person = viewModel.selectedPersonDetail {
                PersonDetailView(person: person, viewModel: viewModel)
                    .transition(.opacity)
                    .zIndex(900)
            }

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
        .onReceive(NotificationCenter.default.publisher(for: .fileIDOpenAIModelSettings)) { _ in
            viewModel.activeTab = "Settings"
            withAnimation { viewModel.closePreview() }
        }
        .onKeyPress(.escape) {
            if viewModel.previewFile != nil {
                withAnimation { viewModel.closePreview() }
                return .handled
            }
            if viewModel.selectedPersonDetail != nil {
                withAnimation { viewModel.closePersonDetail() }
                return .handled
            }
            return .ignored
        }
    }
}

struct SidebarView: View {
    @ObservedObject var viewModel: AppViewModel
    @State private var memoryMB: Int = 0
    @State private var memoryPollTask: Task<Void, Never>?

    var filesPerSec: Double {
        guard let start = viewModel.processingStartTime,
              viewModel.processedCount > 0 else { return 0 }
        let elapsed = Date().timeIntervalSince(start)
        // Floor of 0.1 s — without it, the very first second of a scan
        // can briefly report 100+ files/s as a single fast file divides
        // by elapsed=0.01 s, then drops to a steady ~15 files/s. The
        // chip flicker was a UX paper cut even though the value was
        // technically correct.
        return elapsed > 0.1 ? Double(viewModel.processedCount) / elapsed : 0
    }

    private var hardwareTooltip: String {
        let cores = Hardware.performanceCoreCount
        let workers = Hardware.workerCap
        let vision = Hardware.visionCeilingMB
        let thumbs = Hardware.thumbnailCacheMB
        let save = Hardware.saveEvery
        return "P-cores: \(cores)  Workers: \(workers)  Vision ceiling: \(vision) MB  Thumbs: \(thumbs) MB  Save every: \(save)"
    }

    @ViewBuilder private var elapsedCell: some View {
        if !viewModel.elapsedString.isEmpty {
            Text(viewModel.elapsedString)
                .font(.caption2.monospacedDigit())
                .foregroundStyle(.secondary)
                .help("Time elapsed since this scan started.")
        } else {
            Text("\u{2013}")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
    }

    @ViewBuilder private var etaCell: some View {
        if !viewModel.etaString.isEmpty {
            Text(viewModel.etaString)
                .font(.caption2.bold().monospacedDigit())
                .foregroundStyle(Theme.gold)
                .help("Estimated time remaining at the current throughput.")
        } else {
            Text(" ")
                .font(.caption2)
        }
    }

    var body: some View {
        List {
            if viewModel.currentFolderURL != nil {
                Section("Navigation") {
                    Button(action: { viewModel.activeTab = "Library" }) {
                        Label("Media Library", systemImage: "photo.on.rectangle")
                    }
                    .buttonStyle(.plain)
                    .contentShape(Rectangle())
                    .foregroundStyle(viewModel.activeTab == "Library" ? Theme.gold : .primary)
                    .padding(.vertical, 3)
                    .accessibilityLabel("Media Library")
                    .accessibilityAddTraits(viewModel.activeTab == "Library" ? .isSelected : [])
                    .help("Browse every scanned photo, video, and document")

                    Button(action: { viewModel.activeTab = "Cleanup" }) {
                        Label("Cleanup & Junk", systemImage: "trash.slash.fill")
                    }
                    .buttonStyle(.plain)
                    .contentShape(Rectangle())
                    .foregroundStyle(viewModel.activeTab == "Cleanup" ? Theme.gold : .primary)
                    .padding(.vertical, 3)
                    .accessibilityLabel("Cleanup and Junk")
                    .accessibilityAddTraits(viewModel.activeTab == "Cleanup" ? .isSelected : [])
                    .help("Review flagged junk files and duplicates")

                    Button(action: { viewModel.activeTab = "Restructure" }) {
                        Label("Folder Restructuring", systemImage: "folder.badge.gearshape")
                    }
                    .buttonStyle(.plain)
                    .contentShape(Rectangle())
                    .foregroundStyle(viewModel.activeTab == "Restructure" ? Theme.gold : .primary)
                    .padding(.vertical, 3)
                    .accessibilityLabel("Folder Restructuring")
                    .accessibilityAddTraits(viewModel.activeTab == "Restructure" ? .isSelected : [])
                    .help("Preview a proposed smart folder layout and apply it")

                    Button(action: { viewModel.activeTab = "People" }) {
                        Label("People", systemImage: "person.2.fill")
                    }
                    .buttonStyle(.plain)
                    .contentShape(Rectangle())
                    .foregroundStyle(viewModel.activeTab == "People" ? Theme.gold : .primary)
                    .padding(.vertical, 3)
                    .accessibilityLabel("People — face clusters")
                    .accessibilityAddTraits(viewModel.activeTab == "People" ? .isSelected : [])
                    .help("Face clusters — name, merge, or split detected identities")

                    Button(action: { viewModel.activeTab = "Review" }) {
                        Label("Review & Accept", systemImage: "checkmark.shield.fill")
                    }
                    .buttonStyle(.plain)
                    .contentShape(Rectangle())
                    .foregroundStyle(viewModel.activeTab == "Review" ? Theme.gold : .primary)
                    .padding(.vertical, 3)
                    .accessibilityLabel("Review and Accept proposed changes")
                    .accessibilityAddTraits(viewModel.activeTab == "Review" ? .isSelected : [])
                    .help("Approve or skip every proposed rename and move")

                    Button(action: { viewModel.activeTab = "Settings" }) {
                        Label("Settings", systemImage: "gearshape.fill")
                    }
                    .buttonStyle(.plain)
                    .contentShape(Rectangle())
                    .foregroundStyle(viewModel.activeTab == "Settings" ? Theme.gold : .primary)
                    .padding(.vertical, 3)
                    .accessibilityLabel("Settings")
                    .accessibilityAddTraits(viewModel.activeTab == "Settings" ? .isSelected : [])
                    .help("Tune performance, AI models, and uninstall FileID")
                }
                
                Section("Processing Control") {
                    VStack(alignment: .leading, spacing: Theme.Space.s) {
                        HStack(spacing: Theme.Space.s) {
                            Image(systemName: sidebarPhaseIcon(viewModel.scanPhase))
                                .font(.system(size: 10, weight: .bold))
                                .foregroundStyle(sidebarPhaseColor(viewModel.scanPhase))
                            if viewModel.isProcessing && viewModel.scanPhase != .idle && viewModel.scanPhase != .ready {
                                Text(viewModel.scanPhase.rawValue)
                                    .font(.system(size: 10, weight: .semibold))
                                    .foregroundStyle(sidebarPhaseColor(viewModel.scanPhase))
                                    .padding(.horizontal, 6)
                                    .padding(.vertical, 2)
                                    .background(sidebarPhaseColor(viewModel.scanPhase).opacity(0.15), in: Capsule())
                            }
                            Text(viewModel.currentStatus)
                                .font(.system(size: 11, weight: .medium))
                                .foregroundStyle(.primary)
                                .lineLimit(2)
                        }

                        // 4 Hz during scans keeps the counter rows smooth without
                        // pinning the main thread at 120 Hz ProMotion refresh.
                        // Idle: 10 s — nothing animates when there's no scan.
                        TimelineView(.periodic(from: .now, by: viewModel.isProcessing ? 0.25 : 10)) { _ in
                            VStack(alignment: .leading, spacing: 6) {
                                let phase = viewModel.scanPhase
                                let isDiscovering = phase == .discovering
                                let (phaseDone, phaseTotal): (Int, Int) = {
                                    switch phase {
                                    case .discovering: return (0, 0)
                                    case .tagging:     return (viewModel.processedCount, viewModel.totalCount)
                                    case .clustering:  return (viewModel.clusteringFacesDone, viewModel.clusteringFacesTotal)
                                    case .naming:      return (viewModel.namingDone, viewModel.namingTotal)
                                    case .scoring:     return (viewModel.scoringDone, viewModel.scoringTotal)
                                    case .idle, .ready: return (viewModel.processedCount, viewModel.totalCount)
                                    }
                                }()
                                let showDeterminate = phaseTotal > 0 && phaseDone <= phaseTotal

                                if showDeterminate {
                                    ProgressView(
                                        value: Double(phaseDone),
                                        total: Double(max(phaseTotal, 1))
                                    )
                                    .progressViewStyle(.linear)
                                    .tint(sidebarPhaseColor(phase))
                                    .animation(.linear(duration: 0.15), value: phaseDone)
                                } else {
                                    ProgressView()
                                        .progressViewStyle(.linear)
                                        .tint(Theme.gold)
                                }

                                if viewModel.isProcessing && isDiscovering {
                                    Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 8, verticalSpacing: 3) {
                                        GridRow {
                                            Label("\(viewModel.discoveredCount) found", systemImage: "magnifyingglass")
                                                .font(.system(size: 10, design: .monospaced))
                                                .foregroundStyle(.secondary)
                                                .contentTransition(.numericText())
                                            elapsedCell
                                                .gridColumnAlignment(.trailing)
                                        }
                                        GridRow {
                                            Label("\(viewModel.processedCount) tagged", systemImage: "tag")
                                                .font(.system(size: 10, design: .monospaced))
                                                .foregroundStyle(.secondary)
                                                .contentTransition(.numericText())
                                            etaCell
                                                .gridColumnAlignment(.trailing)
                                        }
                                    }
                                } else {
                                    Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 8, verticalSpacing: 3) {
                                        GridRow {
                                            if viewModel.isProcessing {
                                                Text(phaseCounterLabel(phase: phase, done: phaseDone, total: phaseTotal))
                                                    .font(.system(size: 10, design: .monospaced))
                                                    .foregroundStyle(.secondary)
                                                    .contentTransition(.numericText())
                                            } else {
                                                Text("\(viewModel.totalCount) files total")
                                                    .font(.caption)
                                                    .foregroundStyle(.secondary)
                                            }
                                            elapsedCell
                                                .gridColumnAlignment(.trailing)
                                        }
                                        if !viewModel.etaString.isEmpty && viewModel.isProcessing {
                                            GridRow {
                                                Color.clear.frame(height: 0)
                                                etaCell
                                                    .gridColumnAlignment(.trailing)
                                            }
                                        }
                                    }
                                }

                                if viewModel.isProcessing {
                                    HStack {
                                        if filesPerSec > 0 {
                                            Label(String(format: "%.1f/s", filesPerSec), systemImage: "speedometer")
                                                .font(.system(size: 9, design: .monospaced))
                                                .foregroundStyle(Theme.gold)
                                                .contentTransition(.numericText())
                                                .help("Files tagged per second, rolling 60-second average.")
                                        }
                                        Spacer()
                                        Label("\(memoryMB) MB", systemImage: "memorychip")
                                            .font(.system(size: 9, design: .monospaced))
                                            .foregroundStyle(memoryMB > 1200 ? .orange : .secondary)
                                            .contentTransition(.numericText())
                                            .help(hardwareTooltip)
                                    }
                                }
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
                                        .foregroundStyle(Color.orange)
                                        .frame(maxWidth: .infinity)
                                        .padding(.vertical, 6)
                                        .background(Color.orange.opacity(0.18))
                                        .overlay(
                                            RoundedRectangle(cornerRadius: 6)
                                                .stroke(Color.orange.opacity(0.6), lineWidth: 1)
                                        )
                                        .clipShape(RoundedRectangle(cornerRadius: 6))
                                        .animation(.easeInOut(duration: 0.15), value: viewModel.isPaused)
                                }
                                .buttonStyle(.plain)
                                // Without contentShape, hover hit-testing follows the
                                // intrinsic label size — not the frame(maxWidth:).
                                // Tooltips on Pause/Cancel/Export were silently dead
                                // because hover never landed inside the styled rect.
                                .contentShape(Rectangle())
                                .disabled(viewModel.isCancelled)
                                .help(viewModel.isPaused
                                      ? "Resume scanning"
                                      : "Pause scanning (progress kept)")

                                Button(action: { viewModel.cancelProcessing() }) {
                                    Label(viewModel.isCancelled ? "Cancelling…" : "Cancel",
                                          systemImage: "xmark.circle.fill")
                                        .font(.caption.bold())
                                        .foregroundStyle(.red)
                                        .frame(maxWidth: .infinity)
                                        .padding(.vertical, 6)
                                        .background(Color.red.opacity(0.12))
                                        .overlay(
                                            RoundedRectangle(cornerRadius: 6)
                                                .stroke(Color.red.opacity(0.6), lineWidth: 1)
                                        )
                                        .clipShape(RoundedRectangle(cornerRadius: 6))
                                }
                                .buttonStyle(.plain)
                                .contentShape(Rectangle())
                                .disabled(viewModel.isCancelled)
                                .help("Cancel scan and discard unsaved batch")
                            }

                            Button(action: { viewModel.exportReport() }) {
                                let blue = Color(red: 0.3, green: 0.6, blue: 1.0)
                                Label("Export", systemImage: "square.and.arrow.up")
                                    .font(.caption.bold())
                                    .foregroundStyle(blue)
                                    .frame(maxWidth: .infinity)
                                    .padding(.vertical, 6)
                                    .background(blue.opacity(0.15))
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 6)
                                            .stroke(blue.opacity(0.55), lineWidth: 1)
                                    )
                                    .clipShape(RoundedRectangle(cornerRadius: 6))
                            }
                            .buttonStyle(.plain)
                            .contentShape(Rectangle())
                            .disabled(viewModel.totalCount == 0)
                            .help("Export scan results as CSV or JSON")

                            Button(action: { viewModel.selectFolder() }) {
                                Image(systemName: "arrow.counterclockwise")
                                    .font(.caption.bold())
                                    .foregroundStyle(.secondary)
                                    .padding(6)
                                    .background(Color.white.opacity(0.08))
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 6)
                                            .stroke(Color.white.opacity(0.15), lineWidth: 1)
                                    )
                                    .clipShape(RoundedRectangle(cornerRadius: 6))
                            }
                            .buttonStyle(.plain)
                            .contentShape(Rectangle())
                            .help("Start a new scan (current results stay)")
                        }
                    }
                    .padding(.vertical, 4)
                }
                
                // Hidden during scan to prevent SwiftUI AttributeGraph
                // overflow on large libraries. Renders once at the end of
                // a scan via AppViewModel.finishNamingPhase.
                if !viewModel.fileTree.isEmpty && !viewModel.isProcessing {
                    Section("File Hierarchy") {
                        OutlineGroup(viewModel.fileTree, children: \.children) { node in
                            HStack {
                                Image(systemName: node.children == nil ? "folder" : "folder.fill")
                                    .foregroundStyle(Theme.gold)
                                
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
        .onAppear {
            memoryPollTask?.cancel()
            memoryPollTask = Task {
                while !Task.isCancelled {
                    let mb = sidebarResidentMB()
                    await MainActor.run { memoryMB = mb }
                    try? await Task.sleep(nanoseconds: 750_000_000)
                }
            }
        }
        .onDisappear {
            memoryPollTask?.cancel()
            memoryPollTask = nil
        }
    }

    nonisolated static func residentMB_() -> Int { sidebarResidentMB() }
}

// MARK: - Sidebar Phase Helpers

private func sidebarPhaseColor(_ phase: AppViewModel.ScanPhase) -> Color {
    switch phase {
    case .idle, .ready: return .secondary
    case .discovering:  return .blue
    case .tagging:      return .green
    case .clustering:   return .purple
    case .naming:       return Theme.gold
    case .scoring:      return .orange
    }
}

private func phaseCounterLabel(phase: AppViewModel.ScanPhase, done: Int, total: Int) -> String {
    switch phase {
    case .clustering: return "\(done) / \(total) faces"
    case .naming:     return "\(done) / \(total) named"
    case .scoring:    return "\(done) / \(total) scored"
    default:          return "\(done) / \(total)"
    }
}

private func sidebarPhaseIcon(_ phase: AppViewModel.ScanPhase) -> String {
    switch phase {
    case .idle:        return "circle"
    case .discovering: return "magnifyingglass"
    case .tagging:     return "brain"
    case .clustering:  return "person.2.fill"
    case .naming:      return "pencil"
    case .scoring:     return "star.fill"
    case .ready:       return "checkmark.circle.fill"
    }
}

private func sidebarResidentMB() -> Int { Hardware.residentMB() }

struct MainContent: View {
    @ObservedObject var viewModel: AppViewModel
    
    var body: some View {
        ZStack {
            if viewModel.isWiping {
                WipingSplash()
            } else if viewModel.currentFolderURL != nil {
                // Keep every tab alive at all times — including during scan.
                // Opacity + hit-testing gate which one is active.
                //
                // History:
                //   Batch 4 introduced this ZStack-keep-alive pattern (was
                //     `.id(activeTab)` which destroyed + rebuilt the view
                //     subtree on every switch).
                //   Batch 5 added a scan-time unmount-inactive-tabs gate —
                //     six live `@Query` subscriptions compounded the 17 K-file
                //     throughput cliff by fanning every batch save out to all
                //     six. With the per-tab fetchLimit caps Batch 5 also
                //     landed (CleanupView 500, FileGrid 2 000), the
                //     fan-out cost is small enough — ~450 ms extra per
                //     save batch at saveEvery=400, i.e. ~1.8 % throughput
                //     overhead — to be the right trade for instant tab
                //     switches during scan.
                //   Batch 14 reverts the scan-time unmount: switching from
                //     Library to Cleanup mid-scan was a 1-3 s stall (four
                //     `@Query` predicates initialising simultaneously on
                //     the main thread), which dwarfs the fan-out savings.
                //
                // Net: all six mounted always. The bounded queries keep
                // the per-batch-save cost manageable; the user gets
                // tab switches that don't lock up the UI.
                let active = viewModel.activeTab
                ZStack {
                    TabHost(tag: "Library", active: active, mounted: true) {
                        ProcessingGridView(viewModel: viewModel)
                    }
                    TabHost(tag: "Cleanup", active: active, mounted: true) {
                        CleanupView(viewModel: viewModel)
                    }
                    TabHost(tag: "Restructure", active: active, mounted: true) {
                        FolderOrganizationView(viewModel: viewModel)
                    }
                    TabHost(tag: "People", active: active, mounted: true) {
                        PeopleView(viewModel: viewModel)
                    }
                    TabHost(tag: "Review", active: active, mounted: true) {
                        AcceptChangesView(viewModel: viewModel)
                    }
                    TabHost(tag: "Settings", active: active, mounted: true) {
                        SettingsView(viewModel: viewModel)
                    }
                }
            } else {
                VStack(spacing: 20) {
                    ZStack {
                        Circle()
                            .fill(Theme.gold.opacity(0.12))
                            .frame(width: 130, height: 130)
                        Image(systemName: "tag.circle.fill")
                            .font(.system(size: 80))
                            .symbolRenderingMode(.palette)
                            .foregroundStyle(Theme.gold, Theme.gold.opacity(0.2))
                            .shadow(color: Theme.gold.opacity(0.4), radius: 20, y: 8)
                    }
                    
                    Text("FileID Professional")
                        .font(.system(size: 34, weight: .black, design: .rounded))
                        .foregroundStyle(.white)
                    
                    Button {
                        viewModel.selectFolder()
                    } label: {
                        Text(viewModel.pendingScanFolderURL != nil
                             ? "Waiting on downloads…"
                             : "Connect Root Storage")
                            .font(.system(size: 16, weight: .bold))
                            .padding(.horizontal, 32)
                            .padding(.vertical, 14)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.gold)
                    .foregroundStyle(.black)
                    .controlSize(.large)
                    
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .animation(.easeInOut(duration: 0.22), value: viewModel.activeTab)
    }
}

// Shown while `AppViewModel.startProcessing` is tearing down the previous
// scan's SwiftData rows. With every `@Query` torn down for the duration, the
// wipe's change-notification storm fires into nothing and the next scan's
// Discovery begins in seconds instead of minutes.
private struct WipingSplash: View {
    var body: some View {
        VStack(spacing: 16) {
            ProgressView()
                .controlSize(.large)
                .tint(Theme.gold)
            Text("Clearing previous scan…")
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(.white.opacity(0.9))
            Text("This can take a few seconds on large libraries.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 32)
        .padding(.vertical, 28)
        .background(
            RoundedRectangle(cornerRadius: 16)
                .fill(.ultraThinMaterial)
                .shadow(color: .black.opacity(0.25), radius: 16, y: 6)
        )
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .transition(.opacity)
    }
}

// Keeps a tab view mounted even when another tab is active. @Query
// subscriptions survive the swap, so switching back is instant instead of
// paying a fresh SwiftData fetch.
//
// During a scan `mounted` drops inactive tabs entirely: six live @Query
// subscriptions compounded the 17 K-file throughput cliff by fanning every
// batch save out to every mounted tab. Idle → every tab mounted; scanning →
// only the active tab (+ Library, which the user watches fill).
private struct TabHost<Content: View>: View {
    let tag: String
    let active: String
    let mounted: Bool
    @ViewBuilder var content: Content

    var body: some View {
        if mounted {
            let isActive = tag == active
            content
                .opacity(isActive ? 1 : 0)
                .allowsHitTesting(isActive)
                .accessibilityHidden(!isActive)
        } else {
            Color.clear
        }
    }
}

struct ProcessingGridView: View {
    @ObservedObject var viewModel: AppViewModel
    @State private var selectedTab: String = "Media"

    private var typePicker: some View {
        ThemedSegmentedControl(
            selection: $selectedTab,
            options: [("Media", "Photos & Videos"), ("Documents", "Documents")]
        )
    }

    private var sortPicker: some View {
        ThemedTogglePicker(
            selection: $viewModel.sortByAesthetic,
            falseLabel: "Date",
            trueLabel: "Best"
        )
        .accessibilityLabel("Sort order")
        .help("Sort files by on-disk creation date (Date) or by aesthetic score, sharpest first (Best).")
    }

    private var searchField: some View {
        HStack {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Search...", text: $viewModel.searchText)
                .textFieldStyle(.plain)
            if !viewModel.searchText.isEmpty {
                Button { viewModel.searchText = "" } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .contentShape(Rectangle())
                .help("Clear search")
            }
        }
        .padding(6)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(0.1)))
        .frame(maxWidth: 240)
    }

    private var deepAnalyzeButton: some View {
        let isInstalled = AIModelKind.qwen2VL2B.descriptor.isInstalled
        let running = viewModel.isProcessing && viewModel.scanPhase == .scoring
        return Button {
            viewModel.runDeepAnalyzeNow()
        } label: {
            HStack(spacing: 6) {
                if running {
                    ProgressView().controlSize(.small)
                    Text("Analyzing…").font(.caption.bold())
                } else {
                    Label("Deep Analyze", systemImage: "sparkles.rectangle.stack")
                        .font(.caption.bold())
                }
            }
        }
        .buttonStyle(.bordered)
        .tint(.purple)
        .disabled(viewModel.isProcessing || !isInstalled)
        .help(isInstalled
              ? "Run Deep Analyze on every un-analyzed file in the library"
              : "Download Qwen2.5-VL in Settings → AI Models first")
    }

    var body: some View {
        VStack(spacing: 0) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 8) {
                    typePicker
                    sortPicker
                    deepAnalyzeButton
                    Spacer(minLength: 8)
                    searchField
                }
                .padding(.horizontal)
                .padding(.top, 8)

                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 8) { typePicker; sortPicker; deepAnalyzeButton; Spacer() }
                    searchField.frame(maxWidth: .infinity)
                }
                .padding(.horizontal)
                .padding(.top, 8)
            }

            // `.id` forces a rebuild when the sort toggles so @Query picks up new sort descriptors.
            FileGrid(
                viewModel: viewModel,
                tab: selectedTab,
                query: viewModel.searchText,
                sortByAesthetic: viewModel.sortByAesthetic,
                isProcessing: viewModel.isProcessing
            )
            .id("\(selectedTab)-\(viewModel.sortByAesthetic)-\(viewModel.isProcessing)")
        }
    }
}

private let fileGridMediaExts: Set<String> = FileTypes.images.union(FileTypes.videos)
private let fileGridDocumentExts: Set<String> = FileTypes.documents

private struct FileGrid: View {
    @ObservedObject var viewModel: AppViewModel
    let tab: String
    let query: String
    let sortByAesthetic: Bool
    let isProcessing: Bool

    @Query private var files: [FileRecord]

    @State private var cachedFiltered: [FileRecord] = []

    init(viewModel: AppViewModel, tab: String, query: String, sortByAesthetic: Bool, isProcessing: Bool) {
        self.viewModel = viewModel
        self.tab = tab
        self.query = query
        self.sortByAesthetic = sortByAesthetic
        self.isProcessing = isProcessing
        let sort: [SortDescriptor<FileRecord>] = sortByAesthetic
            ? [SortDescriptor(\.aestheticScore, order: .reverse)]
            : [SortDescriptor(\.creationDate, order: .reverse)]
        // Spring animation re-layouts the whole grid on every save batch during a scan.
        let anim: Animation = isProcessing
            ? .easeOut(duration: 0.12)
            : .spring(response: 0.4, dampingFraction: 0.82)
        // fetchLimit keeps SwiftData from materializing a 50K-row array on every
        // save notification during scans. The grid only renders ~40 cards at a
        // time; 2000 covers scroll, search, and the "did it just appear" UX.
        var descriptor = FetchDescriptor<FileRecord>(
            predicate: #Predicate<FileRecord> { $0.isTrashed == false },
            sortBy: sort
        )
        descriptor.fetchLimit = 2_000
        _files = Query(descriptor, animation: anim)
    }

    private func computeFiltered() -> [FileRecord] {
        let q = query.lowercased()
        let hasQuery = !q.isEmpty
        return files.filter { file in
            let ext = file.url.pathExtension.lowercased()
            let matchesTab = tab == "Media"
                ? fileGridMediaExts.contains(ext)
                : fileGridDocumentExts.contains(ext)
            guard matchesTab else { return false }
            guard hasQuery else { return true }
            return file.filename.lowercased().contains(q)
                || file.aiTags.contains(where: { $0.lowercased().contains(q) })
                || (file.cameraModel?.lowercased().contains(q) ?? false)
                || (file.locationString?.lowercased().contains(q) ?? false)
        }
    }

    var body: some View {
        ScrollView {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 130), spacing: 14)], spacing: 14) {
                // `cachedFiltered` is recomputed on the triggers below, so body
                // re-evals (hover, scroll) don't re-filter a 2 K-row array.
                // No per-card .transition() — staggered insert animations
                // measured ~40 ms per layout pass on visible-card change which
                // showed up as scroll jank during scans.
                ForEach(cachedFiltered, id: \.id) { file in
                    FileCard(file: file, viewModel: viewModel, navContext: cachedFiltered)
                }
            }
            .padding()
        }
        .onAppear { cachedFiltered = computeFiltered() }
        .onChange(of: files.count)    { _, _ in cachedFiltered = computeFiltered() }
        .onChange(of: query)          { _, _ in cachedFiltered = computeFiltered() }
        .onChange(of: tab)            { _, _ in cachedFiltered = computeFiltered() }
    }
}

struct FileCard: View {
    // Plain `let` instead of @Bindable: nothing on this card mutates an
    // individual @Model field that needs to round-trip through SwiftData
    // observation. The trash button below mutates `file.isTrashed`, but the
    // SwiftData `@Query` parent picks that up via change-tracking notifications
    // — we don't need per-cell observers. Dropping @Bindable cuts the
    // observation graph by ~N (one per visible card).
    let file: FileRecord
    @ObservedObject var viewModel: AppViewModel
    let navContext: [FileRecord]
    @State private var thumbnail: NSImage?
    @State private var isHovered = false

    private static let goldStroke = Theme.gold

    var body: some View {
        VStack(spacing: 6) {
            // Flat ZStack instead of GeometryReader. The aspectRatio modifier
            // alone is enough to lock the square; GeometryReader was forcing a
            // layout pass per card on every parent size change (scrolling,
            // window resize) which dominated scroll cost.
            ZStack(alignment: .topTrailing) {
                Rectangle()
                    .fill(Color.white.opacity(0.04))
                    .aspectRatio(1, contentMode: .fit)
                    .overlay {
                        thumbnailContent
                    }
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(file.aestheticScore > 0.85 ? Self.goldStroke : .clear, lineWidth: 1.5)
                    )

                badges
            }

            Text(file.filename)
                .font(.system(size: 11))
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)

            // Single-line tag summary (was a horizontal ScrollView with N
            // capsules per card — ~12 ms per card to lay out at 40 visible
            // cards = ~480 ms scroll cost). Show the top 3 tags joined.
            if !file.aiTags.isEmpty {
                Text(file.aiTags.prefix(3).joined(separator: " · "))
                    .font(.system(size: 9, weight: .medium))
                    .foregroundStyle(Self.goldStroke.opacity(0.85))
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }

            HStack(spacing: 4) {
                Text(file.creationDate.formatted(date: .numeric, time: .omitted))
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
                    .help("File creation date on disk. For re-imported photos this may differ from the original photo-capture date.")
                Spacer()
                Text(String(format: "%.1f MB", file.fileSizeMB))
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(6)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color.white.opacity(0.03))
        )
        .contentShape(Rectangle())
        .onHover { isHovered = $0 }
        .onTapGesture(count: 2) {
            guard file.status != .pending && file.status != .processing else { return }
            viewModel.openPreview(file, in: navContext)
        }
        .task(id: file.url) {
            if thumbnail == nil {
                thumbnail = await ThumbnailService.shared.getThumbnail(for: file.url)
            }
        }
    }

    // Extracted to keep `body` cheap to type-check. SwiftUI re-evaluates the
    // outer `body` on every observed change; pulling this out lets the
    // sub-tree share an identity even when `body` runs.
    @ViewBuilder
    private var thumbnailContent: some View {
        if file.status == .processing {
            ProgressView().controlSize(.small)
        } else if file.status == .failed {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.title2)
                .foregroundStyle(.red)
        } else if let img = thumbnail {
            Image(nsImage: img)
                .resizable()
                .scaledToFill()
        } else {
            ProgressView().controlSize(.small).tint(Self.goldStroke)
        }
    }

    @ViewBuilder
    private var badges: some View {
        HStack(spacing: 4) {
            if file.aestheticScore > 0.85 {
                Image(systemName: "sparkles")
                    .font(.system(size: 9, weight: .bold))
                    .padding(4)
                    .background(Circle().fill(Self.goldStroke))
                    .foregroundStyle(.black)
            }
            if file.duplicateGroupUUID != nil {
                Image(systemName: "rectangle.on.rectangle.angled.fill")
                    .font(.system(size: 9))
                    .padding(4)
                    .background(Circle().fill(.blue))
                    .foregroundStyle(.white)
            }
            // Trash button only renders on hover — was always-mounted, which
            // meant every card paid the Button + symbolRenderingMode cost.
            if isHovered && file.status != .processing && file.status != .pending {
                Button {
                    let url   = file.url
                    let store = viewModel.dataStore
                    let target = file
                    Task {
                        let result = try? await NSWorkspace.shared.recycle([url])
                        if let moved = result, moved[url] != nil {
                            await MainActor.run { target.isTrashed = true }
                            if let store {
                                await store.reconcilePersonSamples(removed: [url])
                            }
                        } else {
                            NSLog("MainWindowView recycle failed: \(url.path)")
                        }
                    }
                } label: {
                    Image(systemName: "trash.circle.fill")
                        .font(.system(size: 22))
                        .symbolRenderingMode(.palette)
                        .foregroundStyle(.white, .red)
                }
                .buttonStyle(.plain)
                .contentShape(Rectangle())
                .help("Move this file to the Trash")
            }
        }
        .padding(5)
    }
}
