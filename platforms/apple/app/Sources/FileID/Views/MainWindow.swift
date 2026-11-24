// Root window: Prototype v3 shell. Title bar over a hairline-split
// sidebar + page, all on the LavaLamp.
import SwiftUI
import AppKit
import FileIDShared

struct MainWindow: View {
    let engine: EngineClient
    @State private var store = ReadStore()
    @State private var toolsSession = ToolsSession()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var atmospherePaused = false
    @AppStorage("activeTabRawValue") private var activeTabRaw: String = Tab.library.rawValue
    @AppStorage("sidebarVisible") private var sidebarVisible: Bool = true
    @AppStorage(LibraryScope.storageKey) private var scopeRootID: Int = 0
    @AppStorage(TextSize.storageKey) private var textSizeRaw: String = TextSize.standard.rawValue
    @State private var pickedURL: URL?
    @State private var scanCounterThrottle = ScanCounterThrottle()
    @State private var isDragHovering = false
    @State private var chatPresented = false
    private let toasts = ToastCenter.shared

    /// Ten pages in three areas plus Settings. Raw values are persisted,
    /// so the legacy names stay stable.
    enum Tab: String, CaseIterable, Identifiable {
        case library     = "Library"
        case people      = "People"
        case moments     = "Search & Moments"
        case deep        = "Deep Analyze"
        case chat        = "Chat"
        case takes       = "Best Takes"
        case cleanup     = "Cleanup"
        case restructure = "Restructure"
        case tools       = "File Tools"
        case settings    = "Settings"
        var id: String { rawValue }

        var title: String {
            switch self {
            case .deep:        return "Describe Files"
            case .cleanup:     return "Duplicates"
            case .restructure: return "Organize Folders"
            default:           return rawValue
            }
        }

        var summary: String {
            switch self {
            case .library:     return "Every scanned file, with search and preview"
            case .people:      return "Faces grouped by person; name them here"
            case .moments:     return "Search inside videos and documents"
            case .deep:        return "Write a description and a better file name for each file"
            case .chat:        return "Ask questions about your files"
            case .takes:       return "Compare several tries of the same shot and pick the best"
            case .cleanup:     return "Find extra copies and move them to Trash"
            case .restructure: return "Suggest a tidier folder layout, then move files"
            case .tools:       return "Convert, resize or reframe photos and videos"
            case .settings:    return "Appearance, AI models and storage"
            }
        }

        static let order: [Tab] = [.library, .people, .moments, .deep, .chat, .takes, .cleanup, .restructure, .tools]

        var area: Area? { Area.allCases.first { $0.tabs.contains(self) } }
        var shortcutIndex: Int? { Self.order.firstIndex(of: self).map { $0 + 1 } }

        var systemImage: String {
            switch self {
            case .library:     return "photo.on.rectangle"
            case .people:      return "person.2.crop.square.stack"
            case .moments:     return "sparkle.magnifyingglass"
            case .deep:        return "text.below.photo"
            case .chat:        return "bubble.left.and.bubble.right"
            case .takes:       return "film.stack"
            case .cleanup:     return "trash.slash"
            case .restructure: return "rectangle.3.offgrid"
            case .tools:       return "wand.and.stars"
            case .settings:    return "gearshape"
            }
        }
    }

    enum Area: String, CaseIterable, Identifiable {
        case browse = "Browse", analyze = "Analyze", act = "Act"
        var id: String { rawValue }
        var shortcutHint: String {
            switch self { case .browse: return "⌘1–3"; case .analyze: return "⌘4–6"; case .act: return "⌘7–9" }
        }
        var tabs: [Tab] {
            switch self {
            case .browse:  return [.library, .people, .moments]
            case .analyze: return [.deep, .chat, .takes]
            case .act:     return [.cleanup, .restructure, .tools]
            }
        }
        static let lastUsedKey = "lastArea"
        static func lastUsed(defaults: UserDefaults = .standard) -> Area {
            defaults.string(forKey: lastUsedKey).flatMap(Area.init(rawValue:)) ?? .browse
        }
        /// Switching areas returns to the page last used in that area.
        func remembered(current: Tab) -> Tab {
            if tabs.contains(current) { return current }
            let key = "lastTab.\(rawValue)"
            if let raw = UserDefaults.standard.string(forKey: key), let tab = Tab(rawValue: raw), tabs.contains(tab) { return tab }
            return tabs[0]
        }
    }

    private var activeTabBinding: Binding<Tab> {
        Binding(get: { Tab(rawValue: activeTabRaw) ?? .library },
                set: { newValue in
                    activeTabRaw = newValue.rawValue
                    if let area = newValue.area {
                        UserDefaults.standard.set(newValue.rawValue, forKey: "lastTab.\(area.rawValue)")
                        UserDefaults.standard.set(area.rawValue, forKey: Area.lastUsedKey)
                    }
                })
    }
    private var activeTab: Tab { Tab(rawValue: activeTabRaw) ?? .library }

    /// A job list taken from a search reply goes stale (the pill kept saying
    /// "Moments 0%"), so any page asks again each second while a job runs.
    nonisolated static func jobsNeedRefresh(_ jobs: [CatalogJob]) -> Bool {
        jobs.contains { $0.state == "queued" || $0.state == "running" }
    }

    private var aiActive: Bool {
        engine.deepAnalyzeInFlight || engine.faceClusteringInFlight || engine.catalogJobs.contains { $0.state == "running" }
    }

    private var engineReady: Bool {
        if case .ready = engine.state { return true }
        return false
    }

    /// Keeps the stored location choice while the list loads, drops it once its location is
    /// gone, and hands the scope to the Library, Search & Moments and Chat.
    private func applyScope(_ roots: [LibraryRoot]) {
        let stored = scopeRootID > 0 ? Int64(scopeRootID) : nil
        let kept = LibraryScope.reconciled(stored, roots: roots)
        if kept != stored { scopeRootID = Int(kept ?? 0) }
        let scope = LibraryScope(rootID: kept, roots: roots)
        store.setScope(scope)
        ChatHandoff.shared.rootID = scope?.root.id
    }

    var body: some View {
        ZStack {
            LavaLampBackground(paused: atmospherePaused, aiActive: aiActive)
                .ignoresSafeArea()

            VStack(spacing: 0) {
                TitleBar(engine: engine, store: store, activeTab: activeTabBinding, pickedURL: $pickedURL, scopeRootID: $scopeRootID,
                         sidebarVisible: $sidebarVisible, chatPresented: $chatPresented, pickFolder: pickFolder)
                    .chromeTextSize()
                    .background(TrafficLightAligner(barHeight: Theme.Metrics.titleBar))
                    .focusSection()
                GeometryReader { geometry in
                HStack(spacing: 0) {
                    if sidebarVisible && (!chatPresented || geometry.size.width >= 1440) {
                        Sidebar(engine: engine, store: store, activeTab: activeTabBinding, pickedURL: $pickedURL, pickFolder: pickFolder)
                            .chromeTextSize()
                            .frame(width: Theme.Metrics.sidebar)
                            .focusSection()
                            .transition(reduceMotion ? .opacity : .move(edge: .leading).combined(with: .opacity))
                        VHairline()
                    }
                    Detail(engine: engine, store: store, toolsSession: toolsSession, activeTab: activeTab,
                           pickedURL: $pickedURL, onSwitchTab: { activeTabBinding.wrappedValue = $0 }, pickFolder: pickFolder)
                        .font(Theme.Font.body(13.5))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .engineUnavailableOverlay(engine: engine, activeTab: activeTab)
                        .focusSection()
                    // Chat stays beside the current page, as before the redesign;
                    // the Chat page is the same conversation at full width.
                    if chatPresented && activeTab != .chat {
                        VHairline()
                        ChatWorkbench(engine: engine, store: store, onClose: { chatPresented = false })
                            .font(Theme.Font.body(13.5))
                            .frame(width: 380)
                            .focusSection()
                            .background(Theme.surfaceBase.opacity(0.86), ignoresSafeAreaEdges: [])
                            .background(.ultraThinMaterial, ignoresSafeAreaEdges: [])
                            .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
                    }
                }
                }
            }
            .preferredColorScheme(.dark)
            .animation(reduceMotion ? nil : Theme.springStandard, value: sidebarVisible)
            .animation(reduceMotion ? nil : Theme.springStandard, value: chatPresented)
            .accentColor(Theme.gold)
            .onReceive(NotificationCenter.default.publisher(for: .fileIDChooseFolder)) { _ in pickFolder() }
            .onReceive(NotificationCenter.default.publisher(for: .fileIDGoToTab)) { note in
                if let raw = note.object as? String, let tab = Tab(rawValue: raw) { activeTabBinding.wrappedValue = tab }
            }
            .onReceive(NotificationCenter.default.publisher(for: .fileIDToggleChat)) { _ in
                if activeTab != .chat { chatPresented.toggle() }
            }
            .onReceive(NotificationCenter.default.publisher(for: .fileIDToggleMotion)) { _ in atmospherePaused.toggle() }
            .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.didMountNotification)) { _ in
                restorePickedFolderIfPossible()
            }
            .onAppear {
                store.openIfPossible()
                restorePickedFolderIfPossible()
                let handoff = ChatHandoff.shared
                handoff.store = store
                handoff.toolsSession = toolsSession
                handoff.activeTab = activeTab
                handoff.libraryRoot = pickedURL
                handoff.navigate = { activeTabBinding.wrappedValue = $0 }
                handoff.chooseFolder = { chooseProposed($0) }
                #if DEBUG
                if SelfShot.opensChatDrawer { chatPresented = true }
                SelfShot.run(engine: engine, store: store,
                             navigate: { raw in if let tab = Tab(rawValue: raw) { activeTabBinding.wrappedValue = tab } },
                             openFolder: { url, scan in if scan { choose(url) } else { pickedURL = url } })
                #endif
            }
            .onChange(of: pickedURL) { _, newValue in
                persistPickedFolder(newValue)
                ChatHandoff.shared.libraryRoot = newValue
            }
            .onChange(of: activeTabRaw) { _, _ in ChatHandoff.shared.activeTab = activeTab }
            .onChange(of: engine.libraryRoots, initial: true) { _, roots in applyScope(roots) }
            .onChange(of: scopeRootID) { _, _ in applyScope(engine.libraryRoots) }
            // The counts in the sidebar and page header must catch up when a scan ends on any
            // page, not only while the Library page is the one on screen.
            .onChange(of: engine.lastProgress?.processed) { _, _ in
                if scanCounterThrottle.shouldRefresh(scanPhase: engine.lastProgress?.phase, now: Date()) { store.openIfPossible() }
            }
            .onChange(of: engine.lastTerminalEventAt) { _, _ in
                // A first scan creates the catalog after launch, so the reader may not be open yet.
                store.openIfPossible()
                store.notifyChanged()
            }
            .onChange(of: engineReady, initial: true) { _, ready in
                guard ready else { return }
                store.openIfPossible()
                store.notifyChanged()
                engine.refreshLibraryRoots()
            }
            .onChange(of: textSizeRaw, initial: true) { _, raw in ContentTextScale.shared.size = TextSize(stored: raw) }
            // Guide the user through the workflow as each stage finishes.
            .task(id: Self.jobsNeedRefresh(engine.catalogJobs)) {
                guard Self.jobsNeedRefresh(engine.catalogJobs) else { return }
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .seconds(1)) } catch { return }
                    _ = engine.send(.catalogRequest(request: CatalogRequest(requestID: UUID().uuidString, action: "jobs")))
                }
            }
            .task(id: engine.lastFaceClustering?.personCount ?? -1) {
                guard let result = engine.lastFaceClustering, result.personCount > 0 else { return }
                toasts.show("Scan complete · \(Plural.noun(result.personCount, "person", "people")) grouped", actionLabel: activeTab == .people ? nil : "Open People", duration: 7) {
                    activeTabBinding.wrappedValue = .people
                }
            }
            .task(id: engine.deepAnalyzeComplete?.processed ?? -1) {
                guard let done = engine.deepAnalyzeComplete, done.processed > 0 else { return }
                toasts.show(DescribeRunSummary.toast(processed: done.processed, ran: done.modelKind, chosen: DeepAnalyzeSettings.shared.activeKind), actionLabel: activeTab == .library ? nil : "Open Library", duration: 7) {
                    activeTabBinding.wrappedValue = .library
                }
            }

            if let toast = toasts.current {
                VStack { Spacer(); ToastView(toast: toast).padding(.bottom, 22) }
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .allowsHitTesting(true)
            }

            if isDragHovering {
                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                    .stroke(Theme.gold, style: StrokeStyle(lineWidth: 2, dash: [10, 6]))
                    .background(Theme.gold.opacity(0.08), ignoresSafeAreaEdges: [])
                    .overlay(
                        VStack(spacing: 12) {
                            Image(systemName: "arrow.down.doc.fill").font(.system(size: 40)).foregroundStyle(Theme.gold)
                            Text("Drop folder to scan").font(Theme.Font.display(28)).foregroundStyle(Theme.gold)
                        }
                    )
                    .padding(14)
                    .allowsHitTesting(false)
            }
        }
        .animation(reduceMotion ? nil : Theme.springStandard, value: toasts.current?.id)
        .environment(\.atmospherePaused, atmospherePaused)
        .onDrop(of: [.fileURL], isTargeted: $isDragHovering) { providers in
            guard let provider = providers.first else { return false }
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                guard let url else { return }
                var isDir: ObjCBool = false
                guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue else {
                    DispatchQueue.main.async { ToastCenter.shared.show("Drop a folder, not a file.") }
                    return
                }
                guard FileManager.default.isReadableFile(atPath: url.path) else {
                    let folderName = url.lastPathComponent
                    DispatchQueue.main.async {
                        let alert = NSAlert()
                        alert.messageText = "Folder isn’t readable"
                        alert.informativeText = "“\(folderName)” can’t be read. If it’s a network share, mount it and try again. If it’s a system folder, pick a folder you own."
                        alert.alertStyle = .warning
                        alert.runModal()
                    }
                    return
                }
                DispatchQueue.main.async { choose(url) }
            }
            return true
        }
    }

    // MARK: - Folder picking

    func pickFolder() { pickFolder(startingAt: nil) }

    /// A folder Chat proposed. The sandboxed build can read only folders the user picked, so
    /// it opens the picker on that folder: one click both confirms it and grants access.
    private func chooseProposed(_ url: URL) {
        #if FILEID_APP_STORE
        pickFolder(startingAt: url)
        #else
        choose(url)
        #endif
    }

    private func pickFolder(startingAt start: URL?) {
        let panel = NSOpenPanel()
        if let start { panel.directoryURL = start }
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Choose a folder for FileID to scan. Nothing in it is moved or changed."
        panel.prompt = "Scan This Folder"
        if panel.runModal() == .OK, let url = panel.url {
            guard FileManager.default.isReadableFile(atPath: url.path) else {
                let alert = NSAlert()
                alert.messageText = "Can’t read \(url.lastPathComponent)"
                alert.informativeText = "FileID doesn’t have permission to read this folder. Choose a different folder, or turn on Full Disk Access for FileID in System Settings → Privacy & Security."
                alert.alertStyle = .warning
                alert.addButton(withTitle: "OK")
                alert.runModal()
                return
            }
            choose(url)
        }
    }

    private func choose(_ url: URL) {
        // Read the scan state before clearing it: a cleared progress looks idle
        // and a second scan would start on top of the running one.
        let scanning = engine.lastProgress.map { [.discovering, .tagging, .postScan].contains($0.phase) } ?? false
        if scanning {
            guard pickedURL != url else { return }
            let alert = NSAlert()
            alert.messageText = "A scan is running. Stop it and scan the new folder?"
            alert.informativeText = "Files already read are kept. You can scan the first folder again later and it picks up where it stopped."
            alert.alertStyle = .warning
            alert.addButton(withTitle: "Stop and Scan \(url.lastPathComponent)")
            alert.addButton(withTitle: "Keep Scanning")
            guard alert.runModal() == .alertFirstButtonReturn else { return }
            engine.cancelScan()
        }
        if pickedURL != url { engine.clearProgress() }
        pickedURL = url
        engine.startScan(rootURL: url)
    }

    // MARK: - Folder persistence

    private func persistPickedFolder(_ url: URL?) {
        guard let url else {
            UserDefaults.standard.removeObject(forKey: SecurityScopedBookmark.pickedFolderDefaultsKey)
            SecurityScopedBookmark.releaseRetainedAccess()
            return
        }
        let key = SecurityScopedBookmark.pickedFolderDefaultsKey
        Task.detached(priority: .utility) {
            if let data = try? SecurityScopedBookmark.make(for: url) {
                guard SecurityScopedBookmark.retainAccess(for: data) != nil else { return }
                await MainActor.run { UserDefaults.standard.set(data, forKey: key) }
            }
        }
    }

    private func restorePickedFolderIfPossible() {
        guard pickedURL == nil,
              UserDefaults.standard.data(forKey: SecurityScopedBookmark.pickedFolderDefaultsKey) != nil
        else { return }
        let key = SecurityScopedBookmark.pickedFolderDefaultsKey
        Task.detached(priority: .utility) {
            let url = PickedFolderRestore.accessibleFolder(
                defaults: .standard, key: key,
                retain: { SecurityScopedBookmark.retainAccess(for: $0) },
                isDirectory: { var isDir: ObjCBool = false; return FileManager.default.fileExists(atPath: $0.path, isDirectory: &isDir) && isDir.boolValue })
            guard let url else { return }
            await MainActor.run {
                guard self.pickedURL == nil else { return }
                self.pickedURL = url
            }
        }
    }
}
