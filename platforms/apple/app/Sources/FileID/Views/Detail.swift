// Page area: crumb + serif title header, then the active page.
import SwiftUI
import AppKit
import FileIDShared

struct Detail: View {
    let engine: EngineClient
    let store: ReadStore
    let toolsSession: ToolsSession
    let activeTab: MainWindow.Tab
    @Binding var pickedURL: URL?
    var onSwitchTab: (MainWindow.Tab) -> Void = { _ in }
    var pickFolder: () -> Void = {}

    var body: some View {
        VStack(spacing: 0) {
            if pickedURL == nil && store.totalFiles == 0 {
                EmptyState(onPickFolder: pickFolder)
            } else {
                pageHeader
                tabContent
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Color(red: 15/255, green: 15/255, blue: 17/255).opacity(0.8))
        .background(.ultraThinMaterial)
        .onChange(of: engine.lastProgress?.phase) { _, new in
            if new == .completed {
                Task { await SpotlightIndexer.indexAll(dbPath: ReadStore.defaultDBURL.path) }
            }
        }
        .onChange(of: engine.deepAnalyzeComplete?.processed ?? -1) { _, _ in
            Task { await SpotlightIndexer.indexAll(dbPath: ReadStore.defaultDBURL.path) }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var crumb: String {
        activeTab == .settings ? "FileID / Settings" : "\(activeTab.area?.rawValue ?? "FileID") / \(activeTab.rawValue)"
    }

    private var pageMeta: String {
        switch activeTab {
        case .library: return store.totalFiles > 0 ? "\(store.totalFiles.formatted()) files" : ""
        case .deep:
            if let d = engine.deepAnalyzeProgress, engine.deepAnalyzeInFlight { return "\(d.processed.formatted()) / \(d.total.formatted())" }
            return ""
        case .settings: return "macOS"
        case .restructure: return pickedURL?.lastPathComponent ?? ""
        default: return ""
        }
    }

    private var pageHeader: some View {
        HStack(alignment: .bottom, spacing: 16) {
            VStack(alignment: .leading, spacing: 7) {
                SectionLabel(crumb)
                Text(activeTab.rawValue).font(Theme.Font.pageTitle).foregroundStyle(Theme.text1).lineLimit(1)
            }
            Spacer(minLength: 0)
            MetaText(pageMeta, color: Theme.text2, size: 12)
        }
        .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.top, 18).padding(.bottom, 14)
        .overlay(alignment: .bottom) { Hairline() }
    }

    @ViewBuilder
    private var tabContent: some View {
        Group {
            switch activeTab {
            case .library:     LibraryView(engine: engine, store: store)
            case .people:      PeopleView(engine: engine, store: store, onSwitchTab: onSwitchTab)
            case .moments:     CatalogWorkbench(engine: engine, inline: true)
            case .deep:        DeepAnalyzeView(engine: engine, store: store, onSwitchTab: onSwitchTab)
            case .chat:        ChatWorkbench(engine: engine)
            case .takes:       TakeWorkbench(engine: engine, candidates: [], inline: true)
            case .cleanup:     CleanupView(engine: engine, store: store)
            case .restructure: RestructureView(store: store, engine: engine, selectedRoot: pickedURL)
            case .tools:       ToolsWorkbench(engine: engine, session: toolsSession, inline: true)
            case .settings:    SettingsTab(engine: engine, store: store)
            }
        }
        .id(activeTab)
        .transition(.opacity)
        .animation(.easeInOut(duration: 0.18), value: activeTab)
    }
}

// MARK: - Empty state — before any folder is picked

private struct EmptyState: View {
    let onPickFolder: () -> Void

    private let steps: [(String, String, String)] = [
        ("Browse", "Scan the folder", "Reads your files, finds faces, indexes text in photos."),
        ("Browse", "Confirm faces", "Group faces by person and name the ones you recognize."),
        ("Analyze", "Captions and names", "Deep Analyze writes captions and proposes specific file names."),
        ("Analyze", "Review moments", "Chapters, searchable evidence and the best take of each attempt."),
        ("Act", "Clean up and restructure", "Trash extra copies, preview a folder layout, try it with shortcuts, then move.")
    ]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel("FileID / Get started")
                    Text("FileID").font(Theme.Font.display(56)).foregroundStyle(Theme.text1)
                    Text("Local-first file organizer. Every model runs on this Mac. Files never leave it.")
                        .font(Theme.Font.body(15)).foregroundStyle(Theme.text2)
                }
                .padding(.bottom, 26)

                Panel {
                    PanelHeader("What happens next")
                    ForEach(Array(steps.enumerated()), id: \.offset) { index, step in
                        HStack(alignment: .firstTextBaseline, spacing: 14) {
                            Text(String(index + 1)).font(Theme.Font.mono(10.5, .semibold)).foregroundStyle(Theme.gold)
                                .frame(width: 16, height: 16).overlay(Rectangle().stroke(Theme.gold, lineWidth: 1))
                            VStack(alignment: .leading, spacing: 3) {
                                HStack(spacing: 8) {
                                    Text(step.1).font(Theme.Font.body(14, .semibold)).foregroundStyle(Theme.text1)
                                    MetaText(step.0.uppercased(), size: 10.5)
                                }
                                Text(step.2).font(Theme.Font.body(13)).foregroundStyle(Theme.text3).fixedSize(horizontal: false, vertical: true)
                            }
                            Spacer(minLength: 0)
                        }
                        .padding(.horizontal, 14).padding(.vertical, 10)
                        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
                    }
                }
                .padding(.bottom, 20)

                HStack(spacing: 12) {
                    Button { onPickFolder() } label: { Label("Pick a folder", systemImage: "folder.badge.plus") }
                        .buttonStyle(.dsPrimary)
                        .keyboardShortcut("o", modifiers: .command)
                    MetaText("…or drag a folder into this window. ⌘O at any time.")
                }
            }
            .frame(maxWidth: 720, alignment: .leading)
            .padding(.horizontal, 40)
            .padding(.vertical, 48)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
