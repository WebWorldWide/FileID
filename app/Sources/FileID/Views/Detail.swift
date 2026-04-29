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

    var body: some View {
        Group {
            if pickedURL == nil && store.totalFiles == 0 {
                EmptyState(onPickFolder: pickFolder)
            } else {
                tabContent
            }
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
        switch activeTab {
        case .library:     LibraryView(engine: engine, store: store)
        case .deep:        DeepAnalyzeView(engine: engine, store: store)
        case .cleanup:     CleanupView(engine: engine, store: store)
        case .restructure: RestructureView(store: store, engine: engine)
        case .people:      PeopleView(engine: engine, store: store)
        case .review:      ReviewView(engine: engine, store: store)
        case .settings:    SettingsTab(engine: engine, store: store)
        }
    }
}

// MARK: - Empty state — onboarding splash

private struct EmptyState: View {
    let onPickFolder: () -> Void

    private struct PipelineStep {
        let n: Int
        let title: String
        let detail: String
    }

    private let steps: [PipelineStep] = [
        .init(n: 1, title: "Scan",
              detail: "Apple Vision finds faces, EXIF, OCR text, and duplicates. Around 80 files per second on Apple Silicon."),
        .init(n: 2, title: "Cluster",
              detail: "Faces are grouped into people automatically using on-device face prints."),
        .init(n: 3, title: "AI verify",
              detail: "Local Qwen vision model verifies ambiguous matches and merges them. Far fewer false splits than face prints alone."),
        .init(n: 4, title: "Name",
              detail: "You name the most-photographed people. One-time, ~30 seconds."),
        .init(n: 5, title: "Deep Analyze",
              detail: "Local VLM writes a caption and suggested filename for every photo, using the names you provided."),
        .init(n: 6, title: "Restructure",
              detail: "Propose a clean folder layout. Apply via symlinks (reversible) or commit to real moves when you're sure."),
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
            Text("FileID")
                .font(.system(size: 44, weight: .bold))
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
