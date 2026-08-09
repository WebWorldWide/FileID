// Sidebar: folder pick, tab nav, live Processing Control, queue, engine status.
import SwiftUI
import FileIDShared

struct Sidebar: View {
    let engine: EngineClient
    let store: ReadStore
    @Binding var activeTab: MainWindow.Tab
    @Binding var pickedURL: URL?
    /// Bound from MainWindow's HStack split.
    @Binding var sidebarVisible: Bool

    @State private var confirmWipeAndRescan = false

    var body: some View {
        // Plain ScrollView + VStack: List + Section regressed to blank
        // sidebar in production, primitive layout is stable.
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                sidebarSection(pickedURL == nil ? "GET STARTED" : "FOLDER") {
                    folderRow.padding(.horizontal, 14)
                }

                // Tabs are obviously navigation — no header needed. The
                // "Pick a folder…" hint covers the disabled case.
                Group {
                    if pickedURL == nil {
                        Text("Pick a folder above to enable tabs")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 14)
                    } else {
                        VStack(alignment: .leading, spacing: 3) {
                            ForEach(MainWindow.Tab.allCases) { tab in
                                navRow(tab)
                            }
                        }
                        .padding(.horizontal, 10)
                    }
                }

                if let url = pickedURL {
                    sidebarSection("SCAN CONTROL") {
                        ProcessingControl(engine: engine, store: store,
                                           pickedURL: url,
                                           changePickedURL: { pickedURL = $0 })
                            .padding(.horizontal, 14)
                    }
                }

                if !engine.queueState.pending.isEmpty {
                    sidebarSection("QUEUE") {
                        QueueListView(state: engine.queueState)
                            .padding(.horizontal, 14)
                    }
                }

                // Engine status pill is only worth showing when something
                // is wrong (crash, error). When everything is healthy the
                // sidebar stays clean — fewer dividers, less visual noise.
                if shouldShowEngineSection {
                    sidebarSection("ENGINE") {
                        VStack(alignment: .leading, spacing: 8) {
                            EngineStatusRow(state: engine.state)
                            if let err = engine.lastError {
                                engineErrorRow(err)
                            }
                        }
                        .padding(.horizontal, 14)
                    }
                }
                Spacer(minLength: 12)
            }
            .padding(.vertical, 20)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Color.black.opacity(0.35))
        // 28 pt strip for traffic-light buttons + the in-sidebar collapse
        // toggle (replaces the system toolbar we removed for full-screen).
        .safeAreaInset(edge: .top, spacing: 0) {
            HStack(spacing: 0) {
                Spacer()
                Button {
                    withAnimation(.easeInOut(duration: 0.2)) {
                        sidebarVisible = false
                    }
                } label: {
                    Image(systemName: "sidebar.left")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Hide sidebar (⌃⌘S)")
                .keyboardShortcut("s", modifiers: [.control, .command])
                .padding(.trailing, 4)
            }
            .frame(height: 28)
        }
    }

    /// Show the engine pill only when there's something interesting:
    /// not yet ready (starting / crashed) or an error worth surfacing.
    /// "Engine: Ready" is noise — the running scan already proves it.
    private var shouldShowEngineSection: Bool {
        if engine.lastError != nil { return true }
        switch engine.state {
        case .starting, .crashed: return true
        case .ready:              return false
        }
    }

    @ViewBuilder
    private func sidebarSection<Content: View>(_ title: String,
                                                @ViewBuilder content: () -> Content)
        -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.system(size: 10, weight: .bold))
                .tracking(0.8)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 14)
            content()
        }
    }

    @ViewBuilder
    private func navRow(_ tab: MainWindow.Tab) -> some View {
        let active = activeTab == tab
        Button { activeTab = tab } label: {
            HStack(spacing: 10) {
                Image(systemName: tab.systemImage)
                    .font(.system(size: 14, weight: active ? .semibold : .regular))
                    .frame(width: 20, alignment: .center)
                Text(tab.rawValue)
                    .font(.system(size: 13, weight: active ? .semibold : .regular))
                Spacer(minLength: 0)
            }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 8)
                .padding(.horizontal, 12)
                .background(
                    RoundedRectangle(cornerRadius: 8)
                        .fill(active ? Theme.gold.opacity(0.18) : Color.clear)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(active ? Theme.gold.opacity(0.55) : Color.clear,
                                  lineWidth: 1)
                )
                .accessibilityLabel("\(tab.rawValue) tab")
                .accessibilityAddTraits(active ? [.isSelected, .isButton] : [.isButton])
                .accessibilityHint(active ? "Currently selected" : "Switches to \(tab.rawValue)")
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(active ? Theme.gold : Color.primary)
    }

    @ViewBuilder
    private var folderRow: some View {
        if let url = pickedURL {
            VStack(alignment: .leading, spacing: 4) {
                Text(url.lastPathComponent)
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1).truncationMode(.middle)
                    .foregroundStyle(Theme.gold)
                Text(url.deletingLastPathComponent().path)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .lineLimit(1).truncationMode(.head)
                Button(action: pickFolder) {
                    Label("Change folder…", systemImage: "folder")
                        .font(.callout)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .padding(.top, 2)
                Button {
                    engine.cancel()
                    engine.clearProgress()
                    pickedURL = nil
                } label: {
                    Label("Clear folder", systemImage: "xmark.circle")
                        .font(.callout)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help("Drop the current folder selection. Doesn't touch the library.")
                Button {
                    confirmWipeAndRescan = true
                } label: {
                    Label("Wipe library + rescan", systemImage: "arrow.clockwise.circle.fill")
                        .font(.callout)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.red.opacity(0.85))
                .help("Delete the current SQLite library and run a fresh scan against this folder.")
                .confirmationDialog(
                    "Wipe library and rescan from scratch?",
                    isPresented: $confirmWipeAndRescan,
                    titleVisibility: .visible
                ) {
                    Button("Wipe and rescan", role: .destructive) {
                        engine.wipeAndRescan(rootURL: url)
                    }
                    Button("Cancel", role: .cancel) { }
                } message: {
                    Text("Deletes every tag, caption, face cluster, and CLIP embedding for this library, then runs the full scan again. Your files are not touched.")
                }
            }
        } else {
            Button(action: pickFolder) {
                Label("Pick a folder…", systemImage: "folder.badge.plus")
                    .foregroundStyle(Theme.gold)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .keyboardShortcut("o", modifiers: .command)
        }
    }

    @ViewBuilder
    private func engineErrorRow(_ err: EngineError) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            VStack(alignment: .leading, spacing: 2) {
                Text(err.kind).font(.caption.bold())
                Text(err.message)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            }
            Spacer(minLength: 4)
            Button { engine.clearLastError() } label: {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Dismiss")
        }
        .padding(8)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(Color.orange.opacity(0.08))
        )
    }

    private func pickFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Pick a folder to scan"
        if panel.runModal() == .OK, let url = panel.url {
            // Pre-validate readability. NSOpenPanel happily returns
            // folders the user can't read (e.g. restricted system
            // dirs, offline network mounts), and the engine then fails
            // opaquely on bookmark resolve. Surface a clear NSAlert
            // here instead of letting it bubble up as a generic IPC
            // error.
            guard FileManager.default.isReadableFile(atPath: url.path) else {
                let alert = NSAlert()
                alert.messageText = "Can't read \(url.lastPathComponent)"
                alert.informativeText = "FileID doesn't have permission to read this folder. Pick a different folder or grant Full Disk Access in System Settings → Privacy & Security."
                alert.alertStyle = .warning
                alert.addButton(withTitle: "OK")
                alert.runModal()
                return
            }
            if pickedURL != url {
                engine.clearProgress()
            }
            pickedURL = url
        }
    }
}




