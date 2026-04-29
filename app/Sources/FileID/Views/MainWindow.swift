// Root window: hand-rolled HStack split over LavaLamp.
import SwiftUI
import FileIDShared

struct MainWindow: View {
    let engine: EngineClient
    @State private var store = ReadStore()
    @State private var activeTab: Tab = .library
    @State private var pickedURL: URL?
    @State private var isDragHovering = false
    /// HStack split avoids NavigationSplitView's auto-inserted toolbar,
    /// which renders an unsuppressible white strip in full-screen mode.
    @State private var sidebarVisible: Bool = true
    private let sidebarWidth: CGFloat = 260

    private static let pickedFolderBookmarkKey = "pickedFolderBookmark.v2"

    enum Tab: String, CaseIterable, Identifiable {
        case library     = "Library"
        case deep        = "Deep Analyze"
        case cleanup     = "Cleanup"
        case restructure = "Restructure"
        case people      = "People"
        case review      = "Review"
        case settings    = "Settings"
        var id: String { rawValue }
        var systemImage: String {
            switch self {
            case .library:     return "photo.on.rectangle"
            case .deep:        return "sparkles"
            case .cleanup:     return "trash.slash"
            case .restructure: return "rectangle.3.offgrid"
            case .people:      return "person.2.crop.square.stack"
            case .review:      return "checkmark.seal"
            case .settings:    return "gearshape"
            }
        }
    }

    var body: some View {
        ZStack {
            LavaLampBackground()
                .ignoresSafeArea()
            HStack(spacing: 0) {
                if sidebarVisible {
                    Sidebar(engine: engine, activeTab: $activeTab,
                            pickedURL: $pickedURL,
                            sidebarVisible: $sidebarVisible)
                        .frame(width: sidebarWidth)
                        .preferredColorScheme(.dark)
                        .transition(.move(edge: .leading).combined(with: .opacity))
                    Divider()
                        .background(Color.white.opacity(0.08))
                }
                Detail(engine: engine, store: store, activeTab: activeTab,
                       pickedURL: $pickedURL,
                       sidebarVisible: $sidebarVisible)
                    .preferredColorScheme(.dark)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .accentColor(Theme.gold)
            .onAppear {
                store.openIfPossible()
                restorePickedFolderIfPossible()
            }
            .onChange(of: pickedURL) { _, newValue in
                persistPickedFolder(newValue)
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
        }
        .onDrop(of: [.fileURL], isTargeted: $isDragHovering) { providers in
            guard let provider = providers.first else { return false }
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                guard let url else { return }
                var isDir: ObjCBool = false
                if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue {
                    DispatchQueue.main.async {
                        if pickedURL != url {
                            engine.clearProgress()
                        }
                        pickedURL = url
                    }
                }
            }
            return true
        }
    }

    // MARK: - Folder persistence

    private func persistPickedFolder(_ url: URL?) {
        guard let url else {
            UserDefaults.standard.removeObject(forKey: Self.pickedFolderBookmarkKey)
            return
        }
        if let data = try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil) {
            UserDefaults.standard.set(data, forKey: Self.pickedFolderBookmarkKey)
        }
    }

    private func restorePickedFolderIfPossible() {
        guard pickedURL == nil,
              let data = UserDefaults.standard.data(forKey: Self.pickedFolderBookmarkKey)
        else { return }
        do {
            var stale = false
            let url = try URL(resolvingBookmarkData: data, options: [], relativeTo: nil, bookmarkDataIsStale: &stale)
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue {
                pickedURL = url
            } else {
                UserDefaults.standard.removeObject(forKey: Self.pickedFolderBookmarkKey)
            }
        } catch {
            UserDefaults.standard.removeObject(forKey: Self.pickedFolderBookmarkKey)
        }
    }
}
