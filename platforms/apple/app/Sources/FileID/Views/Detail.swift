// Detail pane: tab switcher + onboarding splash.
import SwiftUI
import AppKit
import FileIDShared

struct Detail: View {
    let engine: EngineClient
    let store: ReadStore
    let activeTab: MainWindow.Tab
    @Binding var pickedURL: URL?
    /// When the sidebar is hidden, Detail shows a toggle button in the
    /// top safe-area inset to bring it back.
    @Binding var sidebarVisible: Bool
    /// Reserved for future cross-tab navigation. Currently unused
    /// but kept on the API surface so wiring it up later doesn't
    /// require threading a new closure through MainWindow.
    var onSwitchTab: (MainWindow.Tab) -> Void = { _ in }

    var body: some View {
        Group {
            if pickedURL == nil && store.totalFiles == 0 {
                EmptyState(onPickFolder: pickFolder)
            } else {
                tabContent.transition(.opacity)
            }
        }
        // Re-index Spotlight whenever a scan or Deep Analyze batch
        // completes — cheap (single bulk write) and keeps the
        // ⌘Space search results fresh.
        .onChange(of: engine.lastProgress?.phase) { _, new in
            if new == .completed {
                Task.detached { await SpotlightIndexer.indexAll(dbPath: ReadStore.defaultDBURL.path) }
            }
        }
        .onChange(of: engine.deepAnalyzeComplete?.processed ?? -1) { _, _ in
            Task.detached { await SpotlightIndexer.indexAll(dbPath: ReadStore.defaultDBURL.path) }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        // 28 pt strip at the top for traffic-light buttons + the sidebar
        // toggle when the sidebar is collapsed.
        .safeAreaInset(edge: .top, spacing: 0) {
            HStack(spacing: 0) {
                if !sidebarVisible {
                    Button {
                        withAnimation(.easeInOut(duration: 0.2)) {
                            sidebarVisible = true
                        }
                    } label: {
                        Image(systemName: "sidebar.left")
                            .font(.system(size: 14, weight: .medium))
                            .foregroundStyle(.secondary)
                            .frame(width: 28, height: 28)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("Show sidebar (⌃⌘S)")
                    .keyboardShortcut("s", modifiers: [.control, .command])
                    .padding(.leading, 80)   // clear the traffic-light buttons
                }
                Spacer()
            }
            .frame(height: 28)
        }
    }

    /// NSOpenPanel for `pickedURL`. Mirrors the sidebar's button so the
    /// splash CTA can call it directly.
    private func pickFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Pick a folder to scan"
        if panel.runModal() == .OK, let url = panel.url {
            pickedURL = url
        }
    }

    @ViewBuilder
    private var tabContent: some View {
        // Wrapped to give every tab swap an implicit cross-fade. Keyed
        // on the tab raw value so SwiftUI knows when it's a different
        // view tree.
        Group {
            switch activeTab {
            case .library:     LibraryView(engine: engine, store: store)
            case .people:      PeopleView(engine: engine, store: store,
                                          onSwitchTab: onSwitchTab)
            case .cleanup:     CleanupView(engine: engine, store: store)
            case .deep:        DeepAnalyzeView(engine: engine, store: store,
                                                onSwitchTab: onSwitchTab)
            case .restructure: RestructureView(store: store, engine: engine)
            case .settings:    SettingsTab(engine: engine, store: store)
            }
        }
        .id(activeTab)
        .transition(.opacity.combined(with: .move(edge: .trailing)))
        .animation(.easeInOut(duration: 0.22), value: activeTab)
    }
}

// MARK: - Empty state — onboarding splash

private struct EmptyState: View {
    let onPickFolder: () -> Void
    @State private var shimmer: Double = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private struct PipelineStep {
        let n: Int
        let title: String
        let detail: String
    }

    private let steps: [PipelineStep] = [
        .init(n: 1, title: "Scan",
              detail: "Reads your files, finds faces, indexes text in photos."),
        .init(n: 2, title: "Cluster",
              detail: "Groups faces by person."),
        .init(n: 3, title: "Verify",
              detail: "Double-checks the ambiguous matches."),
        .init(n: 4, title: "Name",
              detail: "You name the people you recognize."),
        .init(n: 5, title: "Deep Analyze",
              detail: "Writes captions and smart filenames using the names you gave."),
        .init(n: 6, title: "Restructure",
              detail: "Proposes a clean folder layout. Reversible via shortcuts before any real moves."),
    ]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                titleBlock
                stepsList
                cta
                footer
            }
            .frame(maxWidth: 720, alignment: .leading)
            .padding(.horizontal, 40)
            .padding(.vertical, 56)
            .frame(maxWidth: .infinity)
        }
    }

    private var titleBlock: some View {
        VStack(alignment: .leading, spacing: 6) {
            Group {
                if reduceMotion {
                    // Solid gold for users who prefer reduced motion —
                    // still distinctive, just not animated.
                    Text("FileID")
                        .font(.system(size: 56, weight: .bold))
                        .foregroundStyle(Theme.gold)
                } else {
                    // Iridescent gradient pulled from the FileID logo's
                    // rainbow backdrop. Slowly drifts so the title feels
                    // alive without being distracting — the splash's
                    // signature visual.
                    Text("FileID")
                        .font(.system(size: 56, weight: .bold))
                        .foregroundStyle(
                            LinearGradient(
                                colors: [
                                    Theme.gold, Theme.delight, Theme.ai,
                                    Theme.info, Theme.gold
                                ],
                                startPoint: UnitPoint(x: shimmer, y: 0),
                                endPoint: UnitPoint(x: shimmer + 1, y: 1)
                            )
                        )
                        .onAppear {
                            withAnimation(
                                .linear(duration: 12).repeatForever(autoreverses: false)
                            ) {
                                shimmer = 1
                            }
                        }
                }
            }
            Text("Local-first photo organizer. Everything runs on your Mac.")
                .font(.title3)
                .foregroundStyle(.secondary)
        }
    }

    private var stepsList: some View {
        VStack(alignment: .leading, spacing: 18) {
            ForEach(steps, id: \.n) { step in
                HStack(alignment: .firstTextBaseline, spacing: 16) {
                    Text("\(step.n)")
                        .font(.system(size: 14, weight: .semibold, design: .rounded))
                        .foregroundStyle(.black)
                        .frame(width: 24, height: 24)
                        .background(Circle().fill(Theme.gold))
                    VStack(alignment: .leading, spacing: 2) {
                        Text(step.title)
                            .font(.callout.bold())
                        Text(step.detail)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }

    private var cta: some View {
        Button(action: onPickFolder) {
            Label("Pick a folder", systemImage: "folder.badge.plus")
                .font(.callout.bold())
                .padding(.horizontal, 18)
                .padding(.vertical, 10)
                .background(RoundedRectangle(cornerRadius: 10).fill(Theme.gold))
                .foregroundStyle(.black)
        }
        .buttonStyle(.plain)
        .keyboardShortcut("o", modifiers: .command)
    }

    private var footer: some View {
        Text("…or drag a folder into this window. Press ⌘O at any time.")
            .font(.caption)
            .foregroundStyle(.tertiary)
    }
}
