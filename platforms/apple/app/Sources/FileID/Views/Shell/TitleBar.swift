import SwiftUI
import AppKit
import FileIDShared

/// Prototype v3 title bar: switch folders, move between the three areas,
/// search files and pages or ask Chat (⌘K), watch running work, open Settings.
struct TitleBar: View {
    let engine: EngineClient
    let store: ReadStore
    @Binding var activeTab: MainWindow.Tab
    @Binding var pickedURL: URL?
    @Binding var sidebarVisible: Bool
    @Binding var chatPresented: Bool
    var pickFolder: () -> Void

    @State private var folderOpen = false
    @State private var activityOpen = false
    @State private var confirmWipe = false
    @State private var searchText = ""
    @State private var searchOpen = false
    @FocusState private var searchFocused: Bool
    @State private var userOpened = false

    private var isNarrow: Bool { false }

    var body: some View {
        HStack(spacing: 12) {
            leading
                .frame(maxWidth: .infinity, alignment: .leading)
            areaTabs
            trailing
                .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .padding(.leading, 78)
        .padding(.trailing, 14)
        .frame(height: Theme.Metrics.titleBar)
        .onAppear {
            // The window makes the first text field key on open; the search
            // field should only light up when the user asks for it.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) { searchFocused = false; NSApp.keyWindow?.makeFirstResponder(nil) }
        }
        .background(Theme.bg1.opacity(0.62), ignoresSafeAreaEdges: [])
        .background(.ultraThinMaterial, ignoresSafeAreaEdges: [])
        .overlay(alignment: .bottom) { Hairline() }
        .confirmationDialog("Wipe library and rescan from scratch?", isPresented: $confirmWipe, titleVisibility: .visible) {
            Button("Wipe and rescan", role: .destructive) { if let url = pickedURL { engine.wipeAndRescan(rootURL: url) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Deletes every tag, caption, face cluster and embedding for this library, then runs the full scan again. Your files are not touched.")
        }
    }

    // MARK: Folder switcher

    private var leading: some View {
        HStack(spacing: 10) {
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { sidebarVisible.toggle() }
            } label: {
                Image(systemName: "sidebar.left").font(.system(size: 13, weight: .medium)).foregroundStyle(Theme.text2)
                    .frame(width: 26, height: 26).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(sidebarVisible ? "Hide sidebar (⌃⌘S)" : "Show sidebar (⌃⌘S)")
            .keyboardShortcut("s", modifiers: [.control, .command])

            Button { folderOpen.toggle() } label: {
                HStack(spacing: 10) {
                    Text("FileID").font(Theme.Font.display(18)).foregroundStyle(Theme.text1)
                    VHairline(color: Color.white.opacity(0.18)).frame(height: 14)
                    Text(pickedURL?.lastPathComponent ?? "No folder").font(Theme.Font.body(13.5, .semibold))
                        .foregroundStyle(pickedURL == nil ? Theme.text3 : Theme.text1).lineLimit(1)
                    Text("▾").font(Theme.Font.mono(11)).foregroundStyle(Theme.text3)
                }
                .padding(.horizontal, 10)
                .frame(height: Theme.Metrics.controlLarge)
                .background(Color.black.opacity(0.28), ignoresSafeAreaEdges: [])
                .overlay(Rectangle().stroke(folderOpen ? Theme.gold : Color.white.opacity(0.16), lineWidth: 1))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Switch folder")
            .popover(isPresented: $folderOpen, arrowEdge: .bottom) { folderPopover }
        }
    }

    private var folderPopover: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionLabel("Folders").padding(.horizontal, 12).padding(.top, 10).padding(.bottom, 8)
            if let url = pickedURL {
                HStack(alignment: .top, spacing: 10) {
                    Text("■").font(.system(size: 12)).foregroundStyle(Theme.gold).frame(width: 14)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(url.lastPathComponent).font(Theme.Font.body(14, .semibold)).foregroundStyle(Theme.text1)
                        Text(url.path).font(Theme.Font.meta).foregroundStyle(Theme.text3).lineLimit(1).truncationMode(.middle)
                    }
                    Spacer(minLength: 8)
                    MetaText(store.totalFiles > 0 ? "\(store.totalFiles.formatted()) files" : "Not scanned", color: Theme.text2)
                }
                .padding(.horizontal, 12).padding(.vertical, 9)
                .overlay(alignment: .top) { Hairline(color: Theme.hairlineSoft) }
            } else {
                MetaText("No folder picked yet.").padding(.horizontal, 12).padding(.bottom, 10)
            }
            HStack(spacing: 8) {
                Button("Add folder…") { folderOpen = false; pickFolder() }.buttonStyle(DSSecondaryButtonStyle(height: 28)).keyboardShortcut("o", modifiers: .command)
                if let url = pickedURL {
                    Button("Rescan \(url.lastPathComponent)") { folderOpen = false; engine.startScan(rootURL: url) }
                        .buttonStyle(DSSecondaryButtonStyle(height: 28)).keyboardShortcut("r", modifiers: .command)
                        .disabled(engine.lastProgress.map { $0.phase == .discovering || $0.phase == .tagging || $0.phase == .postScan } ?? false)
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.panelHeader, ignoresSafeAreaEdges: [])
            .overlay(alignment: .top) { Hairline(color: Theme.hairlineStrong) }
            if pickedURL != nil {
                HStack(spacing: 8) {
                    Button("Clear folder") { folderOpen = false; engine.cancel(); engine.clearProgress(); pickedURL = nil }
                        .buttonStyle(.dsGhost).help("Drop the current folder selection. Doesn't touch the library.")
                    Spacer()
                    Button("Wipe library + rescan…") { folderOpen = false; confirmWipe = true }
                        .buttonStyle(DSGhostButtonStyle(color: Theme.danger)).help("Delete the current library and run a fresh scan against this folder.")
                }
                .padding(.horizontal, 10).padding(.vertical, 8)
                .overlay(alignment: .top) { Hairline(color: Theme.hairlineSoft) }
            }
        }
        .frame(width: 360)
        .background(Color(hex: 0x161618), ignoresSafeAreaEdges: [])
        .preferredColorScheme(.dark)
    }

    // MARK: Areas

    private var areaTabs: some View {
        HStack(spacing: 0) {
            ForEach(MainWindow.Area.allCases) { area in
                let on = activeTab.area == area
                Button {
                    activeTab = area.remembered(current: activeTab)
                } label: {
                    Text(area.rawValue)
                        .font(Theme.Font.body(13.5, on ? .semibold : .medium))
                        .foregroundStyle(on ? Theme.onGold : Theme.text2)
                        .padding(.horizontal, 18)
                        .frame(height: Theme.Metrics.control)
                        .background(on ? Theme.gold : Color.clear, ignoresSafeAreaEdges: [])
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(on ? .isSelected : [])
            }
        }
        .background(Color.black.opacity(0.28), ignoresSafeAreaEdges: [])
        .overlay(Rectangle().stroke(Theme.hairlineStrong, lineWidth: 1))
        .disabled(pickedURL == nil && store.totalFiles == 0)
        .opacity(pickedURL == nil && store.totalFiles == 0 ? 0.5 : 1)
    }

    // MARK: Search / activity / settings

    private var trailing: some View {
        HStack(spacing: 8) {
            searchField
            activityButton
            Button { chatPresented.toggle() } label: {
                Image(systemName: "bubble.left.and.bubble.right").font(.system(size: 13, weight: .medium))
                    .foregroundStyle(chatPresented ? Theme.gold : Theme.text2)
                    .frame(width: 30, height: 30)
                    .background(chatPresented ? Theme.gold.opacity(0.14) : Color.black.opacity(0.32), ignoresSafeAreaEdges: [])
                    .overlay(Rectangle().stroke(chatPresented ? Theme.gold : Color.white.opacity(0.16), lineWidth: 1))
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(chatPresented ? "Close chat (⌘⇧K)" : "Open chat beside this page (⌘⇧K)")
            .keyboardShortcut("k", modifiers: [.command, .shift])
            .disabled(activeTab == .chat)
            Button { activeTab = .settings } label: {
                Image(systemName: "gearshape").font(.system(size: 13, weight: .medium)).foregroundStyle(Theme.text2)
                    .frame(width: 30, height: 30)
                    .background(activeTab == .settings ? Theme.gold.opacity(0.14) : Color.black.opacity(0.32), ignoresSafeAreaEdges: [])
                    .overlay(Rectangle().stroke(activeTab == .settings ? Theme.gold : Color.white.opacity(0.16), lineWidth: 1))
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Settings (⌘,)")
            .keyboardShortcut(",", modifiers: .command)
        }
    }

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").font(.system(size: 12, weight: .semibold)).foregroundStyle(Theme.text3)
            TextField("Search or ask", text: $searchText)
                .textFieldStyle(.plain)
                .font(Theme.Font.body(13.5))
                .foregroundStyle(Theme.text1)
                .focused($searchFocused)
                .onSubmit { pickFirst() }
                .onChange(of: searchFocused) { _, focused in
                    if !focused { searchOpen = false } else if userOpened { searchOpen = true }
                }
                .onChange(of: searchText) { _, text in if searchFocused && !text.isEmpty { searchOpen = true } }
                .onTapGesture { userOpened = true; searchFocused = true; searchOpen = true }
                .onExitCommand { searchFocused = false; searchText = "" }
            Text("⌘K").font(Theme.Font.mono(11)).foregroundStyle(Theme.text3)
                .padding(.horizontal, 5).padding(.vertical, 2)
                .overlay(Rectangle().stroke(Color.white.opacity(0.16), lineWidth: 1))
        }
        .padding(.leading, 10).padding(.trailing, 6)
        .frame(height: Theme.Metrics.controlLarge)
        .frame(minWidth: 120, idealWidth: 280, maxWidth: 280)
        .background(Color.black.opacity(0.32), ignoresSafeAreaEdges: [])
        .overlay(Rectangle().stroke(searchOpen || !searchText.isEmpty ? Theme.gold : Color.white.opacity(0.16), lineWidth: 1))
        .background {
            Button("") { userOpened = true; searchFocused = true; searchOpen = true }.keyboardShortcut("k", modifiers: .command).opacity(0).frame(width: 0, height: 0)
        }
        .popover(isPresented: $searchOpen, arrowEdge: .bottom) { searchPopover }
    }

    private struct SearchItem: Identifiable { let id: String; let label: String; let hint: String; let color: Color; let run: () -> Void }

    private var searchItems: [(head: String, items: [SearchItem])] {
        let q = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        var groups: [(String, [SearchItem])] = []
        if !q.isEmpty {
            groups.append(("Ask Chat", [SearchItem(id: "ask", label: "“\(q)”", hint: "↵", color: Theme.ai) {
                UserDefaults.standard.set(q, forKey: "chatDraft")
                if activeTab != .chat { chatPresented = true }
            }]))
            groups.append(("Library", [SearchItem(id: "lib", label: "Find “\(q)” in the library", hint: "Library", color: Theme.text1) {
                UserDefaults.standard.set(q, forKey: "libraryQueryHandoff")
                activeTab = .library
            }]))
        }
        let pages = MainWindow.Tab.allCases.filter { q.isEmpty || $0.rawValue.localizedCaseInsensitiveContains(q) }.prefix(q.isEmpty ? 10 : 4)
        if !pages.isEmpty {
            groups.append(("Pages", pages.map { tab in
                SearchItem(id: tab.rawValue, label: tab.rawValue,
                           hint: [tab.area?.rawValue ?? "FileID", tab.shortcutIndex.map { "⌘\($0)" }].compactMap { $0 }.joined(separator: " · "),
                           color: Theme.text1) { activeTab = tab }
            }))
        }
        return groups
    }

    private func pickFirst() {
        if let first = searchItems.first?.items.first { run(first) }
    }

    private func run(_ item: SearchItem) {
        item.run()
        searchText = ""
        searchOpen = false
        searchFocused = false
        userOpened = false
    }

    private var searchPopover: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(searchItems.enumerated()), id: \.offset) { groupIndex, group in
                SectionLabel(group.head).padding(.horizontal, 12).padding(.top, 10).padding(.bottom, 4)
                ForEach(Array(group.items.enumerated()), id: \.element.id) { itemIndex, item in
                    Button { run(item) } label: {
                        HStack(spacing: 12) {
                            Text(item.label).font(Theme.Font.body(14)).foregroundStyle(item.color).lineLimit(1)
                            Spacer()
                            MetaText(item.hint, size: 11)
                        }
                        .padding(.horizontal, 12).frame(height: 32)
                        .background(groupIndex == 0 && itemIndex == 0 ? Theme.gold.opacity(0.08) : Color.clear, ignoresSafeAreaEdges: [])
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .padding(.bottom, 6)
        .frame(width: 400)
        .background(Color(hex: 0x161618), ignoresSafeAreaEdges: [])
        .preferredColorScheme(.dark)
    }

    // MARK: Activity

    private struct ActivityRow: Identifiable {
        let id: String; let name: String; let sub: String; let state: String; let color: Color
        let progress: Double; let tone: ProgressTone; let button: (label: String, run: () -> Void)?; let open: MainWindow.Tab
    }

    private var scanRunning: Bool {
        guard let p = engine.lastProgress else { return false }
        return p.phase == .discovering || p.phase == .tagging || p.phase == .postScan
    }

    private var rows: [ActivityRow] {
        var out: [ActivityRow] = []
        if let p = engine.lastProgress, p.phase != .idle {
            let frac = p.total > 0 ? Double(p.processed) / Double(p.total) : (p.phase == .completed ? 1 : 0)
            let running = scanRunning
            out.append(ActivityRow(id: "scan", name: "Library scan",
                                   sub: running ? "\(p.processed.formatted()) of \(max(p.total, p.discovered).formatted()) files" : "\(p.processed.formatted()) files",
                                   state: p.phase == .completed ? "DONE" : running ? (engine.isPaused ? "PAUSED " : "") + "\(Int(frac * 100))%" : p.phase.rawValue.uppercased(),
                                   color: p.phase == .completed ? Theme.delight : engine.isPaused ? Theme.text3 : Theme.gold,
                                   progress: frac, tone: p.phase == .completed ? .done : engine.isPaused ? .paused : .gold,
                                   button: running ? (engine.isPaused ? "Resume" : "Pause", { engine.isPaused ? engine.resume() : engine.pause() }) : nil,
                                   open: .library))
        }
        if let d = engine.deepAnalyzeProgress, engine.deepAnalyzeInFlight {
            let frac = d.total > 0 ? Double(d.processed) / Double(d.total) : 0
            out.append(ActivityRow(id: "deep", name: "Deep Analyze", sub: "\(d.processed.formatted()) of \(d.total.formatted()) · \(d.modelKind)",
                                   state: "\(Int(frac * 100))%", color: Theme.ai, progress: frac, tone: .ai,
                                   button: ("Cancel", { engine.deepAnalyzeCancel() }), open: .deep))
        } else if let done = engine.deepAnalyzeComplete, done.processed > 0 {
            out.append(ActivityRow(id: "deep", name: "Deep Analyze", sub: "\(done.processed.formatted()) files", state: "DONE", color: Theme.delight,
                                   progress: 1, tone: .done, button: nil, open: .deep))
        }
        if engine.faceClusteringInFlight {
            out.append(ActivityRow(id: "faces", name: "Face grouping", sub: "Comparing faces", state: "RUNNING", color: Theme.ai, progress: 0.5, tone: .ai, button: nil, open: .people))
        }
        for job in engine.catalogJobs where job.state != "completed" || job.updatedAt > Date().timeIntervalSince1970 - 3600 {
            let running = job.state == "running" || job.state == "queued"
            out.append(ActivityRow(id: "job-\(job.id)", name: job.kind.capitalized, sub: "\(job.fileIDs.count) file\(job.fileIDs.count == 1 ? "" : "s")",
                                   state: job.state == "completed" ? "DONE" : "\(job.state.uppercased()) \(Int(job.progress * 100))%",
                                   color: running ? Theme.ai : job.state == "completed" ? Theme.delight : Theme.text3,
                                   progress: job.progress, tone: running ? .ai : job.state == "completed" ? .done : .paused, button: nil, open: .moments))
        }
        return out
    }

    private var currentActivity: (label: String, color: Color, progress: Double, tone: ProgressTone, idle: Bool) {
        if scanRunning, let p = engine.lastProgress {
            let frac = p.total > 0 ? Double(p.processed) / Double(p.total) : 0
            return ("Scanning", Theme.gold, frac, .gold, false)
        }
        if engine.deepAnalyzeInFlight, let d = engine.deepAnalyzeProgress {
            return ("Captions", Theme.ai, d.total > 0 ? Double(d.processed) / Double(d.total) : 0, .ai, false)
        }
        if engine.faceClusteringInFlight { return ("Faces", Theme.ai, 0.5, .ai, false) }
        if let job = engine.catalogJobs.first(where: { $0.state == "running" }) { return ("Moments", Theme.ai, job.progress, .ai, false) }
        return ("Idle", Theme.info, 1, .info, true)
    }

    private var activityButton: some View {
        let a = currentActivity
        return Button { activityOpen.toggle() } label: {
            HStack(spacing: 8) {
                Rectangle().fill(a.color).frame(width: 6, height: 6)
                Text(a.label).font(Theme.Font.mono(11.5)).tracking(0.7).textCase(.uppercase).foregroundStyle(Theme.text1)
                GrainProgressBar(progress: a.progress, tone: a.tone).frame(width: 54)
                if !a.idle { Text("\(Int(a.progress * 100))%").font(Theme.Font.mono(11.5)).foregroundStyle(a.color).frame(minWidth: 28, alignment: .trailing) }
            }
            .padding(.horizontal, 10)
            .frame(height: Theme.Metrics.controlLarge)
            .background(Color.black.opacity(0.32), ignoresSafeAreaEdges: [])
            .overlay(Rectangle().stroke(activityOpen ? Theme.gold : Color.white.opacity(0.16), lineWidth: 1))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Activity")
        .popover(isPresented: $activityOpen, arrowEdge: .bottom) { activityPopover }
    }

    private var activityPopover: some View {
        let list = rows
        let running = list.filter { $0.tone == .gold || $0.tone == .ai }.count
        return VStack(alignment: .leading, spacing: 0) {
            HStack { SectionLabel("Activity"); Spacer(); MetaText(running > 0 ? "\(running) running" : "Nothing running", size: 11) }
                .padding(.horizontal, 14).padding(.vertical, 10)
                .overlay(alignment: .bottom) { Hairline() }
            if list.isEmpty {
                MetaText("Scans, captions and moment jobs appear here.").padding(14)
            }
            ForEach(list) { row in
                VStack(alignment: .leading, spacing: 8) {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        Text(row.name).font(Theme.Font.body(14, .semibold)).foregroundStyle(Theme.text1)
                        MetaText(row.sub).lineLimit(1)
                        Spacer(minLength: 4)
                        MetaText(row.state, color: row.color)
                    }
                    GrainProgressBar(progress: row.progress, tone: row.tone)
                    HStack(spacing: 6) {
                        if let button = row.button { Button(button.label) { button.run() }.buttonStyle(DSSecondaryButtonStyle(height: 24)) }
                        Button("Open \(row.open.rawValue)") { activityOpen = false; activeTab = row.open }.buttonStyle(DSSecondaryButtonStyle(height: 24))
                    }
                }
                .padding(.horizontal, 14).padding(.vertical, 12)
                .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
            }
        }
        .frame(width: 400)
        .background(Color(hex: 0x161618), ignoresSafeAreaEdges: [])
        .preferredColorScheme(.dark)
    }
}
